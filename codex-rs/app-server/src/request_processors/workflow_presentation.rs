//! Read-only presentation proof shared by native history and scoped workflow chat.
use super::*;
use codex_app_server_protocol as api;
use codex_core::workflows as engine;

#[cfg(test)]
#[path = "workflow_presentation_tests.rs"]
mod tests;

/// Inspect only the last native agent message in the authoritative final turn.
pub(super) async fn final_item(
    store: &dyn ThreadStore,
    child: ThreadId,
    turn: &str,
    value: &serde_json::Value,
) -> Result<Option<String>, JSONRPCErrorError> {
    let page = match store
        .list_items(StoreListItemsParams {
            thread_id: child,
            turn_id: Some(turn.to_owned()),
            item_ids: None,
            include_archived: true,
            position: None,
            page_size: 100,
            sort_direction: StoreSortDirection::Desc,
            sort_key: StoreItemSortKey::CreatedAtOrdinal,
            after_updated_at_ordinal: None,
        })
        .await
    {
        Ok(page) => page,
        Err(ThreadStoreError::Unsupported { .. }) => {
            // Legacy already uses full-history projection for native reads. Reuse
            // that compatibility path; unsupported external stores stay unannotated.
            let metadata = match store
                .read_thread(StoreReadThreadParams {
                    thread_id: child,
                    include_archived: true,
                    include_history: false,
                })
                .await
            {
                Ok(stored) => stored,
                Err(ThreadStoreError::Unsupported { .. }) => return Ok(None),
                Err(error) => return Err(internal_error(error.to_string())),
            };
            if metadata.history_mode != codex_protocol::protocol::ThreadHistoryMode::Legacy {
                return Ok(None);
            }
            let stored = match store
                .read_thread(StoreReadThreadParams {
                    thread_id: child,
                    include_archived: true,
                    include_history: true,
                })
                .await
            {
                Ok(stored) => stored,
                Err(ThreadStoreError::Unsupported { .. }) => return Ok(None),
                Err(error) => return Err(internal_error(error.to_string())),
            };
            let Some(history) = stored.history else {
                return Ok(None);
            };
            let final_message = build_legacy_api_turns_from_rollout_items(&history.items)
                .into_iter()
                .find(|candidate| candidate.id == turn)
                .and_then(|turn| {
                    turn.items
                        .into_iter()
                        .rev()
                        .take(/*n*/ 100)
                        .find_map(|item| match item {
                            api::ThreadItem::AgentMessage { id, text, .. } => Some((id, text)),
                            _ => None,
                        })
                });
            return Ok(
                final_message.and_then(|(id, text)| matches_result(&text, value).then_some(id))
            );
        }
        Err(error) => {
            return Err(internal_error(format!(
                "could not identify the accepted workflow message: {error}"
            )));
        }
    };
    for item in page.items {
        let entry = super::thread_processor::stored_thread_item_to_entry(item)?;
        if let api::ThreadItem::AgentMessage { id, text, .. } = entry.item {
            return Ok(matches_result(&text, value).then_some(id));
        }
    }
    Ok(None)
}

fn matches_result(text: &str, value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|value| text == value)
        || serde_json::from_str::<serde_json::Value>(text)
            .as_ref()
            .ok()
            == Some(value)
}

pub(super) fn annotate(
    proof: &api::WorkflowAgentMessageProvenance,
    turn: &str,
    item: &mut api::ThreadItem,
) {
    if turn == proof.turn_id
        && let api::ThreadItem::AgentMessage { id, workflow, .. } = item
        && id == &proof.item_id
    {
        *workflow = Some(proof.clone());
    }
}

pub(super) async fn lifecycle_proof(
    store: &dyn ThreadStore,
    event: &codex_protocol::workflow::WorkflowLifecycleEvent,
) -> Result<Option<api::WorkflowAgentMessageProvenance>, JSONRPCErrorError> {
    use codex_protocol::workflow::WorkflowLifecycleKind;
    use codex_protocol::workflow::WorkflowLifecycleStatus;
    use codex_protocol::workflow::WorkflowResult;
    let WorkflowLifecycleKind::StepCompleted {
        attempt_id,
        child_thread_id,
        final_turn_id: Some(turn),
        owned_turn_ids,
        status: WorkflowLifecycleStatus::Succeeded,
        result: WorkflowResult::Value { value },
        ..
    } = &event.kind
    else {
        return Ok(None);
    };
    if !owned_turn_ids.contains(turn) {
        return Ok(None);
    }
    let child = ThreadId::from_string(child_thread_id)
        .map_err(|error| invalid_request(error.to_string()))?;
    if persisted_root(store, child).await?.to_string() != event.owner_thread_id {
        return Ok(None);
    }
    proof(
        store,
        &event.run_id,
        &event.execution_id,
        attempt_id,
        child_thread_id,
        turn,
        value,
    )
    .await
}

async fn proof(
    store: &dyn ThreadStore,
    run: &str,
    execution: &str,
    attempt: &str,
    child: &str,
    turn: &str,
    value: &serde_json::Value,
) -> Result<Option<api::WorkflowAgentMessageProvenance>, JSONRPCErrorError> {
    let child_id =
        ThreadId::from_string(child).map_err(|error| invalid_request(error.to_string()))?;
    Ok(final_item(store, child_id, turn, value)
        .await?
        .map(|item_id| api::WorkflowAgentMessageProvenance {
            run_id: run.to_owned(),
            execution_id: execution.to_owned(),
            attempt_id: attempt.to_owned(),
            child_thread_id: child.to_owned(),
            turn_id: turn.to_owned(),
            item_id,
            format: if value.is_string() {
                api::WorkflowAgentMessageFormat::Text
            } else {
                api::WorkflowAgentMessageFormat::Json
            },
        }))
}

/// Membership comes only from persisted attempts or immutable completion facts.
/// Pages and final-turn reads use existing bounds; no worker or service is restored.
async fn proofs(
    home: &AbsolutePathBuf,
    store: &dyn ThreadStore,
    child: ThreadId,
    wanted: &HashSet<(String, String)>,
) -> Result<Vec<api::WorkflowAgentMessageProvenance>, JSONRPCErrorError> {
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let root = persisted_root(store, child).await?;
    if root == child {
        return Ok(Vec::new());
    }
    let runs_root = home.join("workflow-runs");
    let child = child.to_string();
    let mut found = Vec::new();
    let mut offset = 0;
    loop {
        let page = engine::list_persisted_summaries(
            &runs_root, /*project*/ None, offset, /*limit*/ 50,
        )
        .map_err(internal_error)?;
        let count = page.len();
        for summary in page
            .into_iter()
            .filter(|row| row.owner_thread_id == root.to_string())
        {
            let run = engine::read_persisted(&runs_root.join(root.to_string()), &summary.id)
                .map_err(internal_error)?;
            for event in &run.lifecycle.events {
                if let engine::WorkflowLifecycleKind::StepCompleted {
                    child_thread_id,
                    attempt_id,
                    owned_turn_ids,
                    final_turn_id: Some(turn),
                    status: engine::WorkflowStatus::Succeeded,
                    result: engine::WorkflowResult::Value { value },
                    ..
                } = &event.kind
                    && child_thread_id == &child
                    && owned_turn_ids.contains(turn)
                    && wanted.iter().any(|(wanted_turn, _)| wanted_turn == turn)
                    && let Some(proof) = proof(
                        store,
                        &run.id,
                        &event.execution_id,
                        attempt_id,
                        &child,
                        turn,
                        value,
                    )
                    .await?
                    && wanted.contains(&(proof.turn_id.clone(), proof.item_id.clone()))
                {
                    found.push(proof);
                }
            }
            // Older runs may have persisted native ownership without lifecycle cards.
            for step in run.steps.values() {
                let Some(attempt) = step.attempts.last() else {
                    continue;
                };
                if attempt.child_thread_id.as_ref() != Some(&child)
                    || attempt.admission != engine::AdmissionStatus::Accepted
                    || attempt.status != engine::WorkflowStatus::Succeeded
                {
                    continue;
                }
                let (Some(turn), Some(value)) = (&attempt.final_turn_id, &step.result) else {
                    continue;
                };
                if !attempt.owned_turn_ids.contains(turn)
                    || !wanted.iter().any(|(wanted_turn, _)| wanted_turn == turn)
                    || found.iter().any(|proof| proof.attempt_id == attempt.id)
                {
                    continue;
                }
                let execution = if attempt.execution_id.is_empty() {
                    &run.execution_id
                } else {
                    &attempt.execution_id
                };
                if let Some(proof) =
                    proof(store, &run.id, execution, &attempt.id, &child, turn, value).await?
                    && wanted.contains(&(proof.turn_id.clone(), proof.item_id.clone()))
                    && !found.contains(&proof)
                {
                    found.push(proof);
                }
            }
            // A revision may remove a step created before lifecycle outboxes
            // existed. Only its known immutable execution index can prove the
            // old native ownership; current-plan guesses cannot restore it.
            if found.is_empty() {
                let run_root = runs_root.join(root.to_string());
                for execution in run.lifecycle.snapshots.keys() {
                    let mut step_offset = 0;
                    loop {
                        let steps = engine::list_execution_steps(
                            &run_root,
                            &run.id,
                            execution,
                            step_offset,
                            /*limit*/ 50,
                        )
                        .map_err(internal_error)?;
                        let step_count = steps.len();
                        for step in steps {
                            for attempt in step.attempt_ids {
                                let pinned = engine::read_execution_attempt(
                                    &run_root,
                                    &run.id,
                                    execution,
                                    &step.step_id,
                                    &attempt,
                                )
                                .map_err(internal_error)?;
                                let attempt = &pinned.attempt;
                                if attempt.child_thread_id.as_ref() != Some(&child)
                                    || attempt.admission != engine::AdmissionStatus::Accepted
                                    || attempt.status != engine::WorkflowStatus::Succeeded
                                {
                                    continue;
                                }
                                let (Some(turn), engine::WorkflowResult::Value { value }) =
                                    (&attempt.final_turn_id, &pinned.result)
                                else {
                                    continue;
                                };
                                if !attempt.owned_turn_ids.contains(turn)
                                    || !wanted.iter().any(|(wanted_turn, _)| wanted_turn == turn)
                                {
                                    continue;
                                }
                                if let Some(proof) = proof(
                                    store,
                                    &run.id,
                                    execution,
                                    &attempt.id,
                                    &child,
                                    turn,
                                    value,
                                )
                                .await?
                                    && wanted
                                        .contains(&(proof.turn_id.clone(), proof.item_id.clone()))
                                    && !found.contains(&proof)
                                {
                                    found.push(proof);
                                }
                            }
                        }
                        if step_count < 50 {
                            break;
                        }
                        step_offset += step_count;
                    }
                }
            }
        }
        if count < 50 {
            return Ok(found);
        }
        offset += count;
    }
}

async fn persisted_root(
    store: &dyn ThreadStore,
    mut id: ThreadId,
) -> Result<ThreadId, JSONRPCErrorError> {
    let mut visited = HashSet::new();
    for _ in 0..64 {
        if !visited.insert(id) {
            return Err(invalid_request("thread ancestry contains a cycle"));
        }
        let thread = store
            .read_thread(StoreReadThreadParams {
                thread_id: id,
                include_archived: true,
                include_history: false,
            })
            .await
            .map_err(|error| internal_error(error.to_string()))?;
        match thread.parent_thread_id {
            Some(parent) => id = parent,
            None => return Ok(id),
        }
    }
    Err(invalid_request(
        "thread ancestry exceeds the supported depth",
    ))
}

pub(super) async fn entries(
    home: &AbsolutePathBuf,
    store: &dyn ThreadStore,
    child: ThreadId,
    entries: &mut [api::ThreadItemEntry],
) {
    let wanted = entries
        .iter()
        .filter(|entry| matches!(entry.item, api::ThreadItem::AgentMessage { .. }))
        .map(|entry| (entry.turn_id.clone(), entry.item.id().to_owned()))
        .collect();
    match proofs(home, store, child, &wanted).await {
        Ok(proofs) => {
            for proof in proofs {
                for entry in entries.iter_mut() {
                    annotate(&proof, &entry.turn_id, &mut entry.item);
                }
            }
        }
        Err(error) => {
            tracing::debug!(%child, ?error, "workflow message presentation unavailable; preserving native text")
        }
    }
}

pub(super) async fn turns(
    home: &AbsolutePathBuf,
    store: &dyn ThreadStore,
    child: ThreadId,
    turns: &mut [api::Turn],
) {
    let wanted = turns
        .iter()
        .flat_map(|turn| {
            turn.items
                .iter()
                .filter(|item| matches!(item, api::ThreadItem::AgentMessage { .. }))
                .map(|item| (turn.id.clone(), item.id().to_owned()))
        })
        .collect();
    match proofs(home, store, child, &wanted).await {
        Ok(proofs) => {
            for proof in proofs {
                for turn in turns.iter_mut() {
                    for item in &mut turn.items {
                        annotate(&proof, &turn.id, item);
                    }
                }
            }
        }
        Err(error) => {
            tracing::debug!(%child, ?error, "workflow message presentation unavailable; preserving native text")
        }
    }
}

pub(super) async fn timeline(
    home: &AbsolutePathBuf,
    store: &dyn ThreadStore,
    child: ThreadId,
    entries: &mut [api::ThreadTimelineEntry],
) {
    let wanted = entries
        .iter()
        .filter_map(|entry| match entry {
            api::ThreadTimelineEntry::Item { turn_id, item, .. }
                if matches!(item.as_ref(), api::ThreadItem::AgentMessage { .. }) =>
            {
                Some((turn_id.clone(), item.id().to_owned()))
            }
            _ => None,
        })
        .collect();
    match proofs(home, store, child, &wanted).await {
        Ok(proofs) => {
            for proof in proofs {
                for entry in entries.iter_mut() {
                    if let api::ThreadTimelineEntry::Item { turn_id, item, .. } = entry {
                        annotate(&proof, turn_id, item);
                    }
                }
            }
        }
        Err(error) => {
            tracing::debug!(%child, ?error, "workflow message presentation unavailable; preserving native text")
        }
    }
}
