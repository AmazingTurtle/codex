//! Initial exact-owned live items supplement a durable page without changing its cursor.
use super::*;

pub(super) struct Overlay {
    pub attempt_id: String,
    pub child_id: String,
    pub items: Vec<api::ThreadItemEntry>,
}

impl WorkflowRequestProcessor {
    pub(super) async fn active_owned_chat_snapshot(
        &self,
        run: &WorkflowRun,
        execution_id: &str,
        entry: &OwnedAttempt,
        child: ThreadId,
        turn: &str,
    ) -> Result<Option<Vec<api::ThreadItemEntry>>, String> {
        if execution_id != run.execution_id
            || entry.attempt.final_turn_id.is_some()
            || !entry
                .attempt
                .owned_turn_ids
                .iter()
                .any(|owned| owned == turn)
        {
            return Ok(None);
        }
        let Ok(root) = ThreadId::from_string(&run.owner_thread_id) else {
            return Ok(None);
        };
        let Ok(owner) = self.thread_manager.get_thread(root).await else {
            return Ok(None);
        };
        if self.thread_manager.get_thread(child).await.is_err() {
            return Ok(None);
        }
        let Some(step) = run.steps.get(&entry.step_id) else {
            return Ok(None);
        };
        let Ok(Some(active)) = owner
            .active_workflow_owned_turn(&run.id, &step.logical_id, &entry.attempt.id)
            .await
        else {
            return Ok(None);
        };
        if active != turn {
            return Ok(None);
        }
        let state = self
            .listener_task_context
            .thread_state_manager
            .thread_state(child)
            .await;
        let snapshot = state.lock().await.inspection_active_turn_snapshot()?;
        Ok(snapshot
            .filter(|snapshot| snapshot.turn_id == turn)
            .map(|snapshot| snapshot.items))
    }
}

fn unsettled(entry: &api::ThreadItemEntry) -> bool {
    if entry.completed_at_ms.is_some() {
        return false;
    }
    match &entry.item {
        api::ThreadItem::CommandExecution { status, .. } => {
            *status == api::CommandExecutionStatus::InProgress
        }
        api::ThreadItem::McpToolCall { status, .. } => {
            *status == api::McpToolCallStatus::InProgress
        }
        api::ThreadItem::DynamicToolCall { status, .. } => {
            *status == api::DynamicToolCallStatus::InProgress
        }
        api::ThreadItem::FileChange { status, .. } => *status == api::PatchApplyStatus::InProgress,
        _ => false,
    }
}

fn merge(
    durable: &[api::ThreadItemEntry],
    snapshot: &[api::ThreadItemEntry],
) -> Vec<api::ThreadItemEntry> {
    let mut merged = durable.to_vec();
    for (index, item) in snapshot.iter().enumerate() {
        if let Some(existing) = merged
            .iter()
            .position(|entry| entry.turn_id == item.turn_id && entry.item.id() == item.item.id())
        {
            if unsettled(&merged[existing]) {
                let previous = &merged[existing];
                let mut live = item.clone();
                live.started_at_ms = live.started_at_ms.or(previous.started_at_ms);
                if let (
                    api::ThreadItem::CommandExecution {
                        aggregated_output: Some(old),
                        ..
                    },
                    api::ThreadItem::CommandExecution {
                        aggregated_output: Some(new),
                        ..
                    },
                ) = (&previous.item, &mut live.item)
                    && old.len() > new.len()
                    && old.starts_with(new.as_str())
                {
                    *new = old.clone();
                }
                merged[existing] = live;
            }
            continue;
        }
        let next = snapshot.iter().skip(index + 1).find_map(|next| {
            merged.iter().position(|entry| {
                entry.turn_id == next.turn_id && entry.item.id() == next.item.id()
            })
        });
        merged.insert(next.unwrap_or(merged.len()), item.clone());
    }
    merged
}

pub(super) fn apply(
    response: &mut api::WorkflowTranscriptListResponse,
    overlays: Vec<Overlay>,
) -> Result<(), JSONRPCErrorError> {
    let original_limitation = response.limitation.clone();
    // Reserve the actual serialized notice before any overlay can consume its space.
    response.limitation.get_or_insert_with(||"The active conversation exceeds the bounded live preview. Its durable page remains available; open the worker to inspect its current output.".into());
    let mut rejected = false;
    for overlay in overlays {
        let Some(index) = response.data.iter().position(|group| {
            group.attempt_id == overlay.attempt_id && group.child_thread_id == overlay.child_id
        }) else {
            continue;
        };
        let durable = std::mem::take(&mut response.data[index].items);
        response.data[index].items = merge(&durable, &overlay.items);
        let count = response
            .data
            .iter()
            .map(|group| group.items.len())
            .sum::<usize>();
        let bytes = serde_json::to_vec(response)
            .map_err(|error| internal_error(error.to_string()))?
            .len();
        if count > 1000 || bytes > MAX_RESPONSE_BYTES {
            response.data[index].items = durable;
            rejected = true;
        }
    }
    if !rejected {
        response.limitation = original_limitation;
    }
    Ok(())
}

#[cfg(test)]
#[path = "workflow_transcript_live_tests.rs"]
mod tests;
