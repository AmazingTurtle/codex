//! Scoped inspection may attach to an already live worker, but never restores one.
use super::*;
use codex_core::workflows::WorkflowAttempt;
use codex_core::workflows::WorkflowRun;

impl WorkflowRequestProcessor {
    pub(super) async fn subscribe_owned_chat(
        &self,
        run: &WorkflowRun,
        execution_id: &str,
        step_id: &str,
        attempt: &WorkflowAttempt,
        child: ThreadId,
        connection_id: ConnectionId,
    ) {
        if execution_id != run.execution_id || attempt.final_turn_id.is_some() {
            return;
        }
        let Some(step) = run.steps.get(step_id) else {
            return;
        };
        let Ok(root) = ThreadId::from_string(&run.owner_thread_id) else {
            return;
        };
        let Ok(owner) = self.thread_manager.get_thread(root).await else {
            return;
        };
        let Ok(Some(turn)) = owner
            .active_workflow_owned_turn(&run.id, &step.logical_id, &attempt.id)
            .await
        else {
            return;
        };
        if !attempt.owned_turn_ids.contains(&turn)
            || self.thread_manager.get_thread(child).await.is_err()
        {
            return;
        }
        // The existing listener routes native item IDs and turn IDs to this
        // connection. The transcript consumer still filters its pinned ownership.
        // No raw model response stream or implicit thread/resume is requested.
        if let Err(error) = super::super::thread_lifecycle::ensure_workflow_inspection_listener(
            self.listener_task_context.clone(),
            child,
            connection_id,
        )
        .await
        {
            tracing::debug!(%child, ?error, "live workflow chat listener unavailable; durable transcript remains readable");
        }
    }
}
