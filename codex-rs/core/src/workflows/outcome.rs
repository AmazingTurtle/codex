//! Reconciles native terminal results and own usage through supported retained history.

use crate::session::session::Session;
use codex_dynamic_workflows::AgentOutcome;
use codex_history::RolloutItem;
use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_thread_store::LoadThreadHistoryParams;
use serde_json::Value;

pub(super) async fn persisted(
    owner: &Session,
    child: ThreadId,
    expected_final: Option<&str>,
) -> Result<Option<AgentOutcome>, String> {
    // A terminal event is newer than the last compaction. Paginated children
    // therefore need the supported resumable suffix, not legacy full history.
    let history = owner
        .services
        .thread_store
        .load_latest_model_context(LoadThreadHistoryParams {
            thread_id: child,
            include_archived: true,
        })
        .await
        .map_err(|error| error.to_string())?;
    // Typed usage is reconciled by the ownership-aware observer/admission
    // barrier. A newer independent human turn must not expand this result.
    if expected_final.is_none()
        && history
            .items
            .iter()
            .any(|item| matches!(item, RolloutItem::Compacted(_)))
    {
        return Err("historical workflow outcome ownership is unknown after compaction".into());
    }
    let mut interrupted = None;
    let mut usage = Value::Null;
    for item in history.items {
        match item {
            RolloutItem::TokenUsageRecord(record) if record.thread_id == child => {
                usage = serde_json::to_value(record.thread_token_usage)
                    .map_err(|error| error.to_string())?;
            }
            RolloutItem::EventMsg(EventMsg::TurnComplete(event)) => {
                if expected_final.is_some_and(|expected| event.turn_id != expected) {
                    continue;
                }
                let mut outcome = AgentOutcome {
                    result: event
                        .last_agent_message
                        .map(|text| serde_json::from_str(&text).unwrap_or(Value::String(text)))
                        .unwrap_or(Value::Null),
                    usage,
                    error: event.error.map(|error| error.message),
                };
                if outcome.result.to_string().len() > codex_dynamic_workflows::MAX_VALUE_BYTES {
                    outcome.result = Value::Null;
                    outcome.error =
                        Some("workflow agent result exceeds the 16KiB result limit".into());
                }
                return Ok(Some(outcome));
            }
            RolloutItem::EventMsg(EventMsg::TurnAborted(event)) => {
                if expected_final.is_some_and(|expected| event.turn_id.as_deref() != Some(expected))
                {
                    continue;
                }
                let resumable = matches!(
                    event.reason,
                    codex_protocol::protocol::TurnAbortReason::Interrupted
                        | codex_protocol::protocol::TurnAbortReason::BudgetLimited
                );
                let outcome = AgentOutcome {
                    result: Value::Null,
                    usage: usage.clone(),
                    error: Some(event.error.map(|error| error.message).unwrap_or_else(|| {
                        format!("workflow agent interrupted: {:?}", event.reason)
                    })),
                };
                if !resumable {
                    return Ok(Some(outcome));
                }
                interrupted = Some(outcome);
            }
            RolloutItem::EventMsg(EventMsg::TurnStarted(_)) => interrupted = None,
            _ => {}
        }
    }
    Ok(interrupted)
}
