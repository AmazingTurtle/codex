//! A delayed durable page cannot erase a newer native stream or its accepted final proof.
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadItemEntry;
use codex_app_server_protocol::WorkflowTranscriptGroup;

pub(super) fn entry(old: &ThreadItemEntry, new: &ThreadItemEntry) -> ThreadItemEntry {
    if terminal(old) {
        let mut settled = old.clone();
        if let (
            ThreadItem::AgentMessage {
                workflow: previous, ..
            },
            ThreadItem::AgentMessage {
                workflow: Some(proof),
                ..
            },
        ) = (&mut settled.item, &new.item)
            && previous.is_none()
            && proof.turn_id == old.turn_id
            && proof.item_id == old.item.id()
        {
            *previous = Some(proof.clone());
        }
        return settled;
    }
    if terminal(new) {
        return new.clone();
    }
    let texts = match (&old.item, &new.item) {
        (
            ThreadItem::AgentMessage { text: old, .. },
            ThreadItem::AgentMessage { text: new, .. },
        ) => Some((old.as_str(), new.as_str())),
        (
            ThreadItem::CommandExecution {
                aggregated_output: old,
                ..
            },
            ThreadItem::CommandExecution {
                aggregated_output: new,
                ..
            },
        ) => Some((
            old.as_deref().unwrap_or_default(),
            new.as_deref().unwrap_or_default(),
        )),
        _ => None,
    };
    if let Some((old_text, new_text)) = texts
        && (!new_text.starts_with(old_text) || old_text.len() > new_text.len())
    {
        return old.clone();
    }
    new.clone()
}

fn terminal(entry: &ThreadItemEntry) -> bool {
    entry.completed_at_ms.is_some()
        || match &entry.item {
            ThreadItem::AgentMessage {
                workflow: Some(_), ..
            } => true,
            ThreadItem::CommandExecution { status, .. } => {
                *status != codex_app_server_protocol::CommandExecutionStatus::InProgress
            }
            ThreadItem::McpToolCall { status, .. } => {
                *status != codex_app_server_protocol::McpToolCallStatus::InProgress
            }
            _ => false,
        }
}

pub(super) fn origin(old: &WorkflowTranscriptGroup, new: &mut WorkflowTranscriptGroup) {
    if old.final_turn_id.is_some() {
        new.final_turn_id = old.final_turn_id.clone();
        if old.final_item_id.is_some() {
            new.final_item_id = old.final_item_id.clone();
        }
        if old.final_result.is_some() {
            new.final_result = old.final_result.clone();
        }
        new.owned_turn_ids = old.owned_turn_ids.clone();
    } else {
        for turn in &old.owned_turn_ids {
            if !new.owned_turn_ids.contains(turn) {
                new.owned_turn_ids.push(turn.clone());
            }
        }
    }
}
