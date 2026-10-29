//! Caller policy precedes durable workflow control and revision orchestration.
//!
//! This adapter is registered with the native steering implementation.

use super::DynamicWorkflowService;
use crate::session::session::Session;
use codex_dynamic_workflows::RevisionApplyRequest;
use codex_dynamic_workflows::RevisionResumeRequest;
use codex_dynamic_workflows::RevisionStageRequest;
use codex_dynamic_workflows::WorkflowControlCommand;
use codex_dynamic_workflows::WorkflowControlRequest;
use codex_dynamic_workflows::WorkflowControlResponse;
use codex_dynamic_workflows::WorkflowControlState;
use codex_dynamic_workflows::WorkflowRevision;
use codex_dynamic_workflows::WorkflowRevisionResponse;
use std::sync::Arc;

impl DynamicWorkflowService {
    async fn authorize_control(&self, caller: &Arc<Session>, run_id: &str) -> Result<(), String> {
        self.get(run_id)?;
        let owner = self
            .owner
            .upgrade()
            .ok_or("workflow owner is unavailable")?;
        if !Arc::ptr_eq(
            &caller.services.local_agent_runtime.execution_control,
            &owner.services.local_agent_runtime.execution_control,
        ) {
            return Err("workflow caller belongs to another conversation".into());
        }
        if self.host.authorizing_thread_id(run_id)? != caller.thread_id {
            return Err("cross-caller workflow mutations are unsupported; use the original authorizing conversation".into());
        }
        let turn = caller.new_default_turn().await;
        let capture: futures::future::BoxFuture<'_, _> =
            Box::pin(caller.capture_speculative_step_context(
                turn,
                &caller.services.local_agent_runtime.shutdown,
            ));
        let step = capture.await.map_err(|error| error.to_string())?;
        super::super::step::validate_origin(&step)
    }

    pub fn control_state(&self, run_id: &str) -> Result<WorkflowControlState, String> {
        self.get(run_id)?;
        self.engine.control_state(run_id)
    }

    pub fn read_execution(
        &self,
        run_id: &str,
        execution_id: &str,
    ) -> Result<codex_dynamic_workflows::WorkflowExecutionSnapshot, String> {
        self.get(run_id)?;
        self.engine.read_execution_snapshot(run_id, execution_id)
    }

    pub fn list_controls(
        &self,
        run_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkflowControlCommand>, String> {
        self.get(run_id)?;
        self.engine.list_controls(run_id, offset, limit)
    }

    pub(crate) async fn control_from(
        &self,
        caller: Arc<Session>,
        run_id: &str,
        request: WorkflowControlRequest,
    ) -> Result<WorkflowControlResponse, String> {
        self.authorize_control(&caller, run_id).await?;
        self.engine.control(run_id, request).await
    }

    pub(crate) async fn stage_revision_from(
        &self,
        caller: Arc<Session>,
        run_id: &str,
        request: RevisionStageRequest,
    ) -> Result<WorkflowRevisionResponse, String> {
        self.authorize_control(&caller, run_id).await?;
        self.engine.stage_revision(run_id, request)
    }

    pub(crate) async fn apply_revision_from(
        &self,
        caller: Arc<Session>,
        run_id: &str,
        request: RevisionApplyRequest,
    ) -> Result<WorkflowRevisionResponse, String> {
        self.authorize_control(&caller, run_id).await?;
        let owner = self
            .owner
            .upgrade()
            .ok_or("workflow owner is unavailable")?;
        let membership = owner
            .services
            .local_agent_runtime
            .track_tree_member()
            .map_err(|error| error.to_string())?;
        let response = self.engine.apply_revision(run_id, request).await?;
        let engine = self.engine.clone();
        let run_id = run_id.to_string();
        let revision_id = response.revision.id.clone();
        tokio::spawn(async move {
            let _membership = membership;
            let _caller = caller;
            let result = tokio::select! {
                result = engine.wait_revision_settlement(&run_id, &revision_id) => result,
                _ = owner.services.local_agent_runtime.shutdown.cancelled() => {
                    match engine.interrupt(&run_id).await {
                        Ok(_) => engine.wait_revision_settlement(&run_id, &revision_id).await,
                        Err(error) => Err(error),
                    }
                },
            };
            if let Err(error) = result {
                tracing::warn!(%run_id, %revision_id, %error, "workflow revision settlement did not complete cleanly");
            }
        });
        Ok(response)
    }

    pub(crate) async fn resume_revision_from(
        &self,
        caller: Arc<Session>,
        run_id: &str,
        request: RevisionResumeRequest,
    ) -> Result<WorkflowRevisionResponse, String> {
        self.authorize_control(&caller, run_id).await?;
        let response = self.engine.prepare_revision_resume(run_id, request)?;
        let owner = self
            .owner
            .upgrade()
            .ok_or("workflow owner is unavailable")?;
        let run = self.get(run_id)?;
        self.host.record_children(&run);
        self.notifications
            .record(
                &owner,
                &caller,
                &run,
                super::super::notification::NotificationSource::User,
            )
            .await?;
        self.launch(
            owner,
            Some(caller),
            run_id.to_string(),
            super::Launch::Revision,
        )?;
        Ok(response)
    }

    pub fn list_revisions(
        &self,
        run_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkflowRevision>, String> {
        self.get(run_id)?;
        self.engine.list_revisions(run_id, offset, limit)
    }

    pub async fn active_owned_turn(
        &self,
        run_id: &str,
        logical_id: &str,
        expected_attempt_id: &str,
    ) -> Result<Option<String>, String> {
        let run = self.get(run_id)?;
        let step = run
            .steps
            .values()
            .find(|step| step.logical_id == logical_id)
            .ok_or("workflow logical step not found")?;
        let attempt = step
            .attempts
            .iter()
            .find(|attempt| attempt.id == expected_attempt_id)
            .ok_or("workflow attempt not found")?;
        if attempt.final_turn_id.is_some() {
            return Ok(None);
        }
        let Some(child) = &attempt.child_thread_id else {
            return Ok(None);
        };
        let child =
            codex_protocol::ThreadId::from_string(child).map_err(|error| error.to_string())?;
        let owner = self
            .owner
            .upgrade()
            .ok_or("workflow owner is unavailable")?;
        let Ok(thread) = owner
            .services
            .local_agent_runtime
            .workflow_thread(child)
            .await
        else {
            return Ok(None);
        };
        let snapshot = thread.session.workflow_usage_snapshot().await;
        if snapshot.restored || snapshot.ownership_retired || snapshot.terminal.is_some() {
            return Ok(None);
        }
        Ok(snapshot
            .active_turn
            .map(|turn| turn.turn_id)
            .filter(|turn| snapshot.owned_turns.contains(turn)))
    }
}
