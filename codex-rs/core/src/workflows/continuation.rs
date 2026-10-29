//! Explicit continuation is a bounded reference for one captured parent task.

use crate::context::ContextualUserFragment;
use crate::context::WorkflowCompletion;
use crate::session::session::Session;
use codex_dynamic_workflows::WorkflowCompletionMode;
use codex_dynamic_workflows::WorkflowRun;
use codex_dynamic_workflows::WorkflowStatus;
use codex_protocol::ThreadId;
use codex_protocol::turn_input::NotSubmittedReason;
use codex_protocol::turn_input::TurnInput;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::turn_input::TurnInputSubmission;
use serde::Deserialize;
use serde::Serialize;
use std::sync::atomic::Ordering;

#[derive(Deserialize, Serialize)]
pub(super) struct ContinuationOrigin {
    thread_id: ThreadId,
    turn_id: String,
    wake_generation: u64,
}

impl ContinuationOrigin {
    pub(super) async fn capture(caller: &Session, mode: WorkflowCompletionMode) -> Option<Self> {
        if mode != WorkflowCompletionMode::Continue {
            return None;
        }
        let (turn, _, _, _) = caller.active_turn_context_and_strict_auto_review().await?;
        Some(Self {
            thread_id: caller.thread_id,
            turn_id: turn.sub_id.clone(),
            wake_generation: caller.wake_generation.load(Ordering::Acquire),
        })
    }
}

/// False retains the durable notification until the root gate reopens.
pub(super) async fn deliver(
    owner: &Session,
    run: &WorkflowRun,
    origin: ContinuationOrigin,
) -> Result<bool, String> {
    if owner
        .services
        .local_agent_runtime
        .execution_control
        .is_paused()
    {
        return Ok(false);
    }
    if !matches!(
        run.status,
        WorkflowStatus::Succeeded | WorkflowStatus::Failed
    ) {
        return Ok(true);
    }
    let thread = owner
        .services
        .local_agent_runtime
        .workflow_thread(origin.thread_id)
        .await
        .map_err(|error| error.to_string())?;
    if thread.session.wake_generation.load(Ordering::Acquire) != origin.wake_generation {
        return Ok(true);
    }
    let status = if run.status == WorkflowStatus::Succeeded {
        "succeeded"
    } else {
        "failed"
    };
    let mut item = ContextualUserFragment::into(WorkflowCompletion::new(
        run.id.clone(),
        status.into(),
        run.steps
            .values()
            .filter(|step| step.result.is_some())
            .count(),
        run.steps.len(),
        run.error.clone(),
    ));
    let legacy_id = codex_protocol::ResponseItemId::with_suffix(
        "workflow",
        format!("continue-{}-{}", run.id, run.execution_id),
    );
    let identity = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, legacy_id.as_bytes());
    let id = codex_protocol::ResponseItemId::with_suffix("msg", identity);
    let mut receipt_ids = vec![id.clone()];
    receipt_ids.extend(super::notification_id::terminal_notice_ids(&legacy_id));
    item.set_id(Some(id.clone()));
    thread
        .flush_rollout()
        .await
        .map_err(|error| error.to_string())?;
    for receipt_id in &receipt_ids {
        if thread
            .session
            .services
            .local_agent_runtime
            .execution_control
            .has_delivery(origin.thread_id, receipt_id)
            .await
        {
            return Ok(true);
        }
    }
    if !thread
        .session
        .services
        .thread_store
        .find_persisted_input_ids(codex_thread_store::FindPersistedInputIdsParams {
            thread_id: origin.thread_id,
            ids: receipt_ids
                .into_iter()
                .map(codex_thread_store::PersistedInputId::ResponseItem)
                .collect(),
        })
        .await
        .map_err(|error| error.to_string())?
        .is_empty()
    {
        return Ok(true);
    }
    let request = TurnInputRequest::new(TurnInput::ResponseItem(item)).from_agent();
    let result = thread
        .continue_workflow_turn_if_idle(request, origin.turn_id, origin.wake_generation)
        .await
        .map_err(|error| error.to_string())?;
    Ok(!matches!(
        result,
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::ConversationPaused | NotSubmittedReason::NotIdle
        }
    ))
}
