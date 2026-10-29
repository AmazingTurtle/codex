//! Runtime controls always retain the actual conversation invoking them.

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

impl crate::CodexThread {
    /// Reads availability without creating a workflow service or restoring a worker.
    pub async fn active_workflow_owned_turn(
        &self,
        run_id: &str,
        logical_id: &str,
        expected_attempt_id: &str,
    ) -> Result<Option<String>, String> {
        let runtime = &self.session.services.local_agent_runtime;
        let state = runtime
            .execution_control
            .snapshot()
            .await
            .map_err(|error| error.to_string())?;
        let Ok(root) = runtime.workflow_thread(state.root_thread_id).await else {
            return Ok(None);
        };
        let Some(service) = root.session.services.workflows.service.get() else {
            return Ok(None);
        };
        service
            .active_owned_turn(run_id, logical_id, expected_attempt_id)
            .await
    }

    pub async fn workflow_control(
        &self,
        run_id: &str,
        request: WorkflowControlRequest,
    ) -> Result<WorkflowControlResponse, String> {
        super::service(&self.session)
            .await?
            .control_from(Arc::clone(&self.session), run_id, request)
            .await
    }

    pub async fn workflow_control_state(
        &self,
        run_id: &str,
    ) -> Result<WorkflowControlState, String> {
        super::service(&self.session).await?.control_state(run_id)
    }

    pub async fn workflow_controls(
        &self,
        run_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkflowControlCommand>, String> {
        super::service(&self.session)
            .await?
            .list_controls(run_id, offset, limit)
    }

    pub async fn stage_workflow_revision(
        &self,
        run_id: &str,
        request: RevisionStageRequest,
    ) -> Result<WorkflowRevisionResponse, String> {
        super::service(&self.session)
            .await?
            .stage_revision_from(Arc::clone(&self.session), run_id, request)
            .await
    }

    pub async fn apply_workflow_revision(
        &self,
        run_id: &str,
        request: RevisionApplyRequest,
    ) -> Result<WorkflowRevisionResponse, String> {
        super::service(&self.session)
            .await?
            .apply_revision_from(Arc::clone(&self.session), run_id, request)
            .await
    }

    pub async fn resume_workflow_revision(
        &self,
        run_id: &str,
        request: RevisionResumeRequest,
    ) -> Result<WorkflowRevisionResponse, String> {
        super::service(&self.session)
            .await?
            .resume_revision_from(Arc::clone(&self.session), run_id, request)
            .await
    }

    pub async fn workflow_revisions(
        &self,
        run_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkflowRevision>, String> {
        super::service(&self.session)
            .await?
            .list_revisions(run_id, offset, limit)
    }
}
