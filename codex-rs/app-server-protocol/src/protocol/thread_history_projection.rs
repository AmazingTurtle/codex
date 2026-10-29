//! Stateless projection from canonical paginated rollout records to thread-history changes.
//!
//! This module is only for the new paginated rollout format that persists canonical
//! `ItemCompleted(TurnItem)` records, not legacy event-only rollouts.

use codex_protocol::protocol::EventMsg;
use codex_rollout::RolloutItem;
use codex_rollout::RolloutLine;

use crate::protocol::thread_history::ThreadHistoryChangeSet;
use crate::protocol::thread_history::ThreadHistoryItemChange;
use crate::protocol::thread_history::ThreadHistoryTurnMetadata;
use crate::protocol::v2::ThreadItem;
use crate::protocol::v2::TurnError;
use crate::protocol::v2::TurnStatus;

/// Projects a real native input without treating its delivery as a new human submission.
/// Identity and turn provenance must come from the stored native item, never surrounding text.
pub fn project_native_agent_input(
    item: &codex_protocol::models::ResponseItem,
) -> Option<ThreadHistoryItemChange> {
    use codex_protocol::models::AgentMessageInputContent;
    use codex_protocol::models::ResponseItem;
    let ResponseItem::AgentMessage {
        id: Some(id),
        content,
        ..
    } = item
    else {
        return None;
    };
    let turn_id = item.turn_id()?;
    let content = content
        .iter()
        .map(|content| match content {
            AgentMessageInputContent::InputText { text } => {
                Some(crate::protocol::v2::UserInput::Text {
                    text: text.clone(),
                    text_elements: Vec::new(),
                })
            }
            AgentMessageInputContent::EncryptedContent { .. } => None,
        })
        .collect::<Option<Vec<_>>>()?;
    if content.is_empty() {
        return None;
    }
    Some(ThreadHistoryItemChange {
        turn_id: turn_id.into(),
        item: ThreadItem::UserMessage {
            id: id.to_string(),
            client_id: None,
            content,
        },
        started_at_ms: None,
        completed_at_ms: None,
    })
}

/// Project one durable rollout line without reconstructing earlier history.
///
/// Callers that replay a JSONL suffix should invoke it once per line, in ordinal order, so storage
/// can preserve the first and latest timestamps for repeated item snapshots independently.
pub fn project_rollout_line(line: &RolloutLine) -> ThreadHistoryChangeSet {
    match &line.item {
        RolloutItem::ResponseItem(item) => ThreadHistoryChangeSet {
            changed_items: project_native_agent_input(&item.item).into_iter().collect(),
            ..Default::default()
        },
        RolloutItem::EventMsg(EventMsg::TurnStarted(event)) => ThreadHistoryChangeSet {
            changed_turns: vec![ThreadHistoryTurnMetadata {
                turn_id: event.turn_id.clone(),
                root_turn_id: event.root_turn_id.clone(),
                status: TurnStatus::InProgress,
                error: None,
                started_at: event.started_at,
                completed_at: None,
                duration_ms: None,
            }],
            ..Default::default()
        },
        RolloutItem::EventMsg(EventMsg::TurnComplete(event)) => ThreadHistoryChangeSet {
            changed_turns: vec![ThreadHistoryTurnMetadata {
                turn_id: event.turn_id.clone(),
                root_turn_id: event.root_turn_id.clone(),
                status: if event.error.is_some() {
                    TurnStatus::Failed
                } else {
                    TurnStatus::Completed
                },
                error: event.error.as_ref().map(|error| TurnError {
                    misalignment: error.misalignment.clone().map(Into::into),
                    message: error.message.clone(),
                    codex_error_info: error.codex_error_info.clone().map(Into::into),
                    additional_details: None,
                }),
                started_at: event.started_at,
                completed_at: event.completed_at,
                duration_ms: event.duration_ms,
            }],
            ..Default::default()
        },
        RolloutItem::EventMsg(EventMsg::TurnAborted(event)) => {
            let Some(turn_id) = event.turn_id.as_ref() else {
                return ThreadHistoryChangeSet::default();
            };
            ThreadHistoryChangeSet {
                changed_turns: vec![ThreadHistoryTurnMetadata {
                    turn_id: turn_id.clone(),
                    root_turn_id: event.root_turn_id.clone(),
                    status: TurnStatus::Interrupted,
                    error: event.error.as_ref().map(|error| TurnError {
                        message: error.message.clone(),
                        codex_error_info: error.codex_error_info.clone().map(Into::into),
                        misalignment: error.misalignment.clone().map(Into::into),
                        additional_details: None,
                    }),
                    started_at: event.started_at,
                    completed_at: event.completed_at,
                    duration_ms: event.duration_ms,
                }],
                ..Default::default()
            }
        }
        RolloutItem::EventMsg(EventMsg::ItemCompleted(event)) => ThreadHistoryChangeSet {
            changed_items: vec![ThreadHistoryItemChange {
                turn_id: event.turn_id.clone(),
                item: ThreadItem::from(event.item.clone()),
                started_at_ms: event.started_at_ms,
                completed_at_ms: (event.completed_at_ms != 0).then_some(event.completed_at_ms),
            }],
            ..Default::default()
        },
        RolloutItem::SessionMeta(_)
        | RolloutItem::InterAgentCommunication(_)
        | RolloutItem::InterAgentCommunicationMetadata { .. }
        | RolloutItem::Compacted(_)
        | RolloutItem::TurnContext(_)
        | RolloutItem::TokenUsageRecord(_)
        | RolloutItem::WorldState(_)
        | RolloutItem::RealtimeItem(_)
        | RolloutItem::RetainedContext(_)
        | RolloutItem::SecurityRiskScore(_)
        | RolloutItem::EventMsg(_) => ThreadHistoryChangeSet::default(),
    }
}

#[cfg(test)]
#[path = "thread_history_projection_tests.rs"]
mod tests;
