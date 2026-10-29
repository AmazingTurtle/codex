//! Cold workflow chat reads are restricted to persisted attempt-owned turns.
use super::*;
use codex_core::workflows::WorkflowAttempt;
use codex_core::workflows::WorkflowRun;
use serde::Deserialize;
use serde::Serialize;

const MAX_GROUPS: usize = 32;
const MAX_TURNS_PER_PAGE: usize = 32;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
#[path = "workflow_transcript_live.rs"]
mod live_snapshot;

#[derive(Deserialize, Serialize)]
struct Cursor {
    execution_id: String,
    workflow_id: String,
    target: api::WorkflowTranscriptTarget,
    attempt_id: String,
    turn_id: String,
    item_cursor: Option<String>,
}

struct OwnedAttempt {
    step_id: String,
    label: String,
    phase: Option<String>,
    attempt: WorkflowAttempt,
    result: Option<serde_json::Value>,
}

impl WorkflowRequestProcessor {
    pub(crate) async fn transcript_list(
        &self,
        connection_id: ConnectionId,
        params: api::WorkflowTranscriptListParams,
    ) -> Result<api::WorkflowTranscriptListResponse, JSONRPCErrorError> {
        let (run_root, run) = self
            .cold_run(&params.thread_id, &params.workflow_id)
            .await?;
        let root = ThreadId::from_string(&run.owner_thread_id)
            .map_err(|error| invalid_request(error.to_string()))?;
        let execution_id = params.execution_id.as_deref().unwrap_or(&run.execution_id);
        let limit = params.limit.unwrap_or(100);
        if !(1..=100).contains(&limit) {
            return Err(invalid_request(
                "transcript limit must be between 1 and 100",
            ));
        }
        let attempts = if execution_id == run.execution_id
            && !run.lifecycle.snapshots.contains_key(execution_id)
        {
            selected_attempts(&run, &params.target)?
        } else {
            historical_attempts(&run_root, &run.id, execution_id, &params.target)?
        };
        let mut limitation = if run.format_version == 2 {
            codex_core::workflows::execution_transcript_limitation(&run_root, &run.id, execution_id)
                .map_err(invalid_request)?
        } else {
            Some("Legacy workflow history has no immutable execution ownership index.".to_owned())
        };
        if attempts.iter().any(|entry| {
            entry.attempt.child_thread_id.is_some() && entry.attempt.owned_turn_ids.is_empty()
        }) {
            limitation.get_or_insert_with(|| "Some historical attempts have no recorded turn ownership; their conversation cannot be shown safely.".to_owned());
        }
        let turns = attempts
            .iter()
            .rev()
            .flat_map(|entry| {
                entry
                    .attempt
                    .owned_turn_ids
                    .iter()
                    .rev()
                    .map(move |turn| (entry, turn))
            })
            .collect::<Vec<_>>();
        let cursor = params
            .cursor
            .map(|cursor| {
                if cursor.len() > 16 * 1024 {
                    return Err(invalid_request("invalid transcript cursor"));
                }
                serde_json::from_str::<Cursor>(&cursor)
                    .map_err(|_| invalid_request("invalid transcript cursor"))
            })
            .transpose()?;
        let initial_page = cursor.is_none();
        let start = if let Some(cursor) = &cursor {
            if cursor.execution_id != execution_id
                || cursor.workflow_id != run.id
                || cursor.target != params.target
            {
                return Err(invalid_request(
                    "transcript cursor belongs to another execution or target",
                ));
            }
            turns
                .iter()
                .position(|(entry, turn)| {
                    entry.attempt.id == cursor.attempt_id && *turn == &cursor.turn_id
                })
                .ok_or_else(|| {
                    invalid_request("transcript cursor ownership is no longer available")
                })?
        } else {
            0
        };
        let mut remaining = limit as usize;
        let mut data: Vec<api::WorkflowTranscriptGroup> = Vec::new();
        let mut child_directories = std::collections::HashMap::new();
        let mut next_cursor = None;
        let mut overlays = Vec::new();
        let mut overlay_bytes = 0usize;
        let mut overlay_items = 0usize;
        for (index, (entry, turn)) in turns.iter().enumerate().skip(start) {
            let Some(child) = entry.attempt.child_thread_id.as_ref() else {
                continue;
            };
            let child_id = ThreadId::from_string(child)
                .map_err(|_| invalid_request("workflow contains an invalid child identity"))?;
            if let std::collections::hash_map::Entry::Vacant(e) = child_directories.entry(child_id)
            {
                if self.persisted_root(child_id).await? != root {
                    return Err(invalid_request(
                        "workflow child belongs to another conversation",
                    ));
                }
                let thread = self
                    .thread_store
                    .read_thread(StoreReadThreadParams {
                        thread_id: child_id,
                        include_archived: true,
                        include_history: false,
                    })
                    .await
                    .map_err(|error| {
                        invalid_request(format!("workflow child is unavailable: {error}"))
                    })?;
                e.insert(
                    codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(thread.cwd)
                        .map_err(|error| internal_error(error.to_string()))?,
                );
                self.subscribe_owned_chat(
                    &run,
                    execution_id,
                    &entry.step_id,
                    &entry.attempt,
                    child_id,
                    connection_id,
                )
                .await;
            }
            let position = cursor
                .as_ref()
                .filter(|_| index == start)
                .and_then(|cursor| cursor.item_cursor.clone())
                .map(StoreListItemsPosition::Cursor);
            let page = match self
                .thread_store
                .list_items(StoreListItemsParams {
                    thread_id: child_id,
                    turn_id: Some((*turn).clone()),
                    item_ids: None,
                    include_archived: true,
                    position,
                    page_size: remaining,
                    sort_direction: StoreSortDirection::Desc,
                    sort_key: StoreItemSortKey::CreatedAtOrdinal,
                    after_updated_at_ordinal: None,
                })
                .await
            {
                Ok(page) => page,
                Err(ThreadStoreError::Unsupported { .. }) => {
                    limitation = Some("This thread's stored history does not support owned-turn transcript pages.".into());
                    break;
                }
                Err(error) => {
                    return Err(internal_error(format!(
                        "could not read workflow chat: {error}"
                    )));
                }
            };
            let mut items = page
                .items
                .into_iter()
                .map(super::super::thread_processor::stored_thread_item_to_entry)
                .collect::<Result<Vec<_>, _>>()?;
            remaining = remaining.saturating_sub(items.len());
            items.reverse();
            if initial_page {
                match self
                    .active_owned_chat_snapshot(&run, execution_id, entry, child_id, turn)
                    .await
                {
                    Ok(Some(snapshot)) => {
                        let bytes = snapshot
                            .iter()
                            .map(|item| {
                                serde_json::to_vec(item).map_or(usize::MAX, |bytes| bytes.len())
                            })
                            .fold(0usize, usize::saturating_add);
                        if overlay_items.saturating_add(snapshot.len()) <= 1000
                            && overlay_bytes.saturating_add(bytes) <= 16 * 1024 * 1024
                        {
                            overlay_items += snapshot.len();
                            overlay_bytes += bytes;
                            overlays.push(live_snapshot::Overlay {
                                attempt_id: entry.attempt.id.clone(),
                                child_id: child.clone(),
                                items: snapshot,
                            });
                        } else {
                            limitation = Some("The active conversation exceeds the bounded live preview. Its durable page remains available; open the worker to inspect its current output.".into());
                        }
                    }
                    Err(reason) => limitation = Some(reason),
                    Ok(None) => {}
                }
            }
            if let Some(group) = data
                .last_mut()
                .filter(|group| group.attempt_id == entry.attempt.id)
            {
                items.append(&mut group.items);
                group.items = items;
            } else {
                let accepted_result = entry.result.as_ref();
                let final_item_id = if let Some(value) = accepted_result {
                    self.final_item(&run.id, execution_id, entry, child_id, value)
                        .await?
                } else {
                    None
                };
                if let (Some(item_id), Some(value), Some(turn)) = (
                    &final_item_id,
                    accepted_result,
                    &entry.attempt.final_turn_id,
                ) {
                    let proof = api::WorkflowAgentMessageProvenance {
                        run_id: run.id.clone(),
                        execution_id: execution_id.to_owned(),
                        attempt_id: entry.attempt.id.clone(),
                        child_thread_id: child.clone(),
                        turn_id: turn.clone(),
                        item_id: item_id.clone(),
                        format: if value.is_string() {
                            api::WorkflowAgentMessageFormat::Text
                        } else {
                            api::WorkflowAgentMessageFormat::Json
                        },
                    };
                    for item in &mut items {
                        super::super::workflow_presentation::annotate(
                            &proof,
                            &item.turn_id,
                            &mut item.item,
                        );
                    }
                }
                if accepted_result.is_some() && final_item_id.is_none() {
                    limitation = Some("This attempt's exact accepted final message is unavailable; its original chat is shown unchanged.".into());
                }
                data.push(api::WorkflowTranscriptGroup {
                    run_id: run.id.clone(),
                    execution_id: execution_id.to_owned(),
                    step_id: entry.step_id.clone(),
                    attempt_id: entry.attempt.id.clone(),
                    child_thread_id: child.clone(),
                    label: entry.label.clone(),
                    phase: entry.phase.clone(),
                    owned_turn_ids: entry.attempt.owned_turn_ids.clone(),
                    resolved_model: entry.attempt.resolved_model.clone(),
                    reasoning_effort: entry.attempt.reasoning_effort.clone(),
                    final_turn_id: entry.attempt.final_turn_id.clone(),
                    final_item_id,
                    final_result: accepted_result
                        .cloned()
                        .map(|value| api::WorkflowTranscriptResult::Value { value }),
                    cwd: child_directories[&child_id].clone(),
                    items,
                });
            }
            let next = page
                .next_cursor
                .map(|item_cursor| (entry.attempt.id.clone(), (*turn).clone(), Some(item_cursor)))
                .or_else(|| {
                    turns
                        .get(index + 1)
                        .map(|(entry, turn)| (entry.attempt.id.clone(), (*turn).clone(), None))
                });
            if remaining == 0 || data.len() >= MAX_GROUPS || index - start + 1 >= MAX_TURNS_PER_PAGE
            {
                next_cursor = next
                    .map(|(attempt_id, turn_id, item_cursor)| {
                        serde_json::to_string(&Cursor {
                            execution_id: execution_id.to_owned(),
                            workflow_id: run.id.clone(),
                            target: params.target.clone(),
                            attempt_id,
                            turn_id,
                            item_cursor,
                        })
                    })
                    .transpose()
                    .map_err(|error| internal_error(error.to_string()))?;
                break;
            }
        }
        data.reverse();
        let mut response = api::WorkflowTranscriptListResponse {
            data,
            next_cursor,
            limitation,
        };
        live_snapshot::apply(&mut response, overlays)?;
        if serde_json::to_vec(&response)
            .map_err(|error| internal_error(error.to_string()))?
            .len()
            > MAX_RESPONSE_BYTES
        {
            return Err(invalid_request(
                "workflow transcript page exceeds the 8MiB response limit; request fewer items",
            ));
        }
        Ok(response)
    }

    pub(super) async fn persisted_root(
        &self,
        mut id: ThreadId,
    ) -> Result<ThreadId, JSONRPCErrorError> {
        let mut visited = std::collections::HashSet::new();
        for _ in 0..64 {
            if !visited.insert(id) {
                return Err(invalid_request("thread ancestry contains a cycle"));
            }
            let thread = self
                .thread_store
                .read_thread(StoreReadThreadParams {
                    thread_id: id,
                    include_archived: true,
                    include_history: false,
                })
                .await
                .map_err(|error| invalid_request(format!("thread is unavailable: {error}")))?;
            match thread.parent_thread_id {
                Some(parent) => id = parent,
                None => return Ok(id),
            }
        }
        Err(invalid_request(
            "thread ancestry exceeds the supported depth",
        ))
    }

    async fn final_item(
        &self,
        run_id: &str,
        execution_id: &str,
        entry: &OwnedAttempt,
        child: ThreadId,
        value: &serde_json::Value,
    ) -> Result<Option<String>, JSONRPCErrorError> {
        let Some(turn) = entry.attempt.final_turn_id.as_ref() else {
            return Ok(None);
        };
        if !entry.attempt.owned_turn_ids.contains(turn) {
            return Ok(None);
        }
        let key = (
            run_id.to_owned(),
            execution_id.to_owned(),
            entry.attempt.id.clone(),
        );
        if let Some(id) = self
            .final_items
            .lock()
            .map_err(|error| internal_error(error.to_string()))?
            .get(&key)
            .cloned()
        {
            return Ok(Some(id));
        }
        let Some(id) = super::super::workflow_presentation::final_item(
            self.thread_store.as_ref(),
            child,
            turn,
            value,
        )
        .await?
        else {
            return Ok(None);
        };
        let mut cache = self
            .final_items
            .lock()
            .map_err(|error| internal_error(error.to_string()))?;
        if cache.len() >= 128
            && let Some(old) = cache.keys().next().cloned()
        {
            cache.remove(&old);
        }
        cache.insert(key, id.clone());
        Ok(Some(id))
    }
}

fn selected_attempts(
    run: &WorkflowRun,
    target: &api::WorkflowTranscriptTarget,
) -> Result<Vec<OwnedAttempt>, JSONRPCErrorError> {
    use api::WorkflowTranscriptTarget;
    let mut steps = run
        .steps
        .values()
        .filter(|step| match target {
            WorkflowTranscriptTarget::Run => true,
            WorkflowTranscriptTarget::Phase { phase } => step.phase.as_ref() == Some(phase),
            WorkflowTranscriptTarget::Step { step_id }
            | WorkflowTranscriptTarget::Attempt { step_id, .. } => &step.id == step_id,
        })
        .collect::<Vec<_>>();
    if steps.is_empty()
        && matches!(
            target,
            WorkflowTranscriptTarget::Step { .. } | WorkflowTranscriptTarget::Attempt { .. }
        )
    {
        return Err(invalid_request("workflow step does not exist"));
    }
    steps.sort_by_key(|step| (step.worker_sequence, &step.id));
    let attempts = steps
        .into_iter()
        .flat_map(|step| {
            step.attempts
                .iter()
                .filter(|attempt| match target {
                    WorkflowTranscriptTarget::Attempt { attempt_id, .. } => {
                        &attempt.id == attempt_id
                    }
                    WorkflowTranscriptTarget::Run
                    | WorkflowTranscriptTarget::Phase { .. }
                    | WorkflowTranscriptTarget::Step { .. } => true,
                })
                .map(move |attempt| {
                    let result = (step
                        .attempts
                        .last()
                        .is_some_and(|last| last.id == attempt.id)
                        && attempt.admission == codex_core::workflows::AdmissionStatus::Accepted
                        && attempt.status == codex_core::workflows::WorkflowStatus::Succeeded)
                        .then(|| step.result.clone())
                        .flatten();
                    OwnedAttempt {
                        step_id: step.id.clone(),
                        label: step.label.clone(),
                        phase: step.phase.clone(),
                        attempt: attempt.clone(),
                        result,
                    }
                })
        })
        .collect::<Vec<_>>();
    if attempts.is_empty() && matches!(target, WorkflowTranscriptTarget::Attempt { .. }) {
        return Err(invalid_request("workflow attempt does not exist"));
    }
    Ok(attempts)
}

fn historical_attempts(
    root: &codex_utils_absolute_path::AbsolutePathBuf,
    run_id: &str,
    execution_id: &str,
    target: &api::WorkflowTranscriptTarget,
) -> Result<Vec<OwnedAttempt>, JSONRPCErrorError> {
    use codex_core::workflows as engine;
    // A known immutable execution authorizes only its pinned record references.
    // No current step, raw child ID, or later human turn can expand that ownership.
    engine::read_execution_snapshot(root, run_id, execution_id).map_err(invalid_request)?;
    let mut attempts = Vec::new();
    let mut offset = 0;
    let mut found_step = false;
    loop {
        let steps = engine::list_execution_steps(root, run_id, execution_id, offset, 50)
            .map_err(invalid_request)?;
        let count = steps.len();
        for step in steps {
            let selected = match target {
                api::WorkflowTranscriptTarget::Run => true,
                api::WorkflowTranscriptTarget::Phase { phase } => {
                    step.phase.as_ref() == Some(phase)
                }
                api::WorkflowTranscriptTarget::Step { step_id }
                | api::WorkflowTranscriptTarget::Attempt { step_id, .. } => {
                    &step.step_id == step_id
                }
            };
            if !selected {
                continue;
            }
            found_step = true;
            for id in step.attempt_ids {
                if let api::WorkflowTranscriptTarget::Attempt { attempt_id, .. } = target
                    && attempt_id != &id
                {
                    continue;
                }
                let value =
                    engine::read_execution_attempt(root, run_id, execution_id, &step.step_id, &id)
                        .map_err(invalid_request)?;
                let result = match value.result {
                    engine::WorkflowResult::Absent => None,
                    engine::WorkflowResult::Value { value } => Some(value),
                };
                attempts.push(OwnedAttempt {
                    step_id: value.step_id,
                    label: value.label,
                    phase: value.phase,
                    attempt: value.attempt,
                    result,
                });
            }
        }
        if count < 50 {
            break;
        }
        offset += count;
    }
    if !found_step
        && matches!(
            target,
            api::WorkflowTranscriptTarget::Step { .. }
                | api::WorkflowTranscriptTarget::Attempt { .. }
        )
    {
        return Err(invalid_request(
            "workflow step does not exist in this execution",
        ));
    }
    if attempts.is_empty() && matches!(target, api::WorkflowTranscriptTarget::Attempt { .. }) {
        return Err(invalid_request(
            "workflow attempt does not exist in this execution",
        ));
    }
    Ok(attempts)
}
