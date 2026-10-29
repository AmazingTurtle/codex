//! Durable JavaScript workflows run ordinary child agents in the existing conversation tree.

mod attempt_context;
mod completion_ownership;
mod continuation;
mod host;
mod lifecycle_delivery;
mod lifecycle_watcher;
mod notification;
mod notification_id;
mod outcome;
mod receipt;
mod service;
mod source;
mod step;
mod stop;
mod task_input;
mod thread_control;
mod usage;
mod usage_ownership;
mod usage_recovery;
mod worker_name;
mod worktree;

pub use codex_dynamic_workflows::AdmissionStatus;
pub use codex_dynamic_workflows::PhaseProgress;
pub use codex_dynamic_workflows::PhaseStatus;
pub use codex_dynamic_workflows::RevisionApplyMode;
pub use codex_dynamic_workflows::RevisionApplyRequest;
pub use codex_dynamic_workflows::RevisionResumeRequest;
pub use codex_dynamic_workflows::RevisionStageRequest;
pub use codex_dynamic_workflows::RunLimits;
pub use codex_dynamic_workflows::SavedWorkflow;
pub use codex_dynamic_workflows::WorkflowAttempt;
pub use codex_dynamic_workflows::WorkflowCompletionMode;
pub use codex_dynamic_workflows::WorkflowControlCommand;
pub use codex_dynamic_workflows::WorkflowControlOperation;
pub use codex_dynamic_workflows::WorkflowControlRequest;
pub use codex_dynamic_workflows::WorkflowControlResponse;
pub use codex_dynamic_workflows::WorkflowControlState;
pub use codex_dynamic_workflows::WorkflowControlStatus;
pub use codex_dynamic_workflows::WorkflowControlTarget;
pub use codex_dynamic_workflows::WorkflowExecutionAttempt;
pub use codex_dynamic_workflows::WorkflowExecutionSnapshot;
pub use codex_dynamic_workflows::WorkflowExecutionStepIndex;
pub use codex_dynamic_workflows::WorkflowLibrary;
pub use codex_dynamic_workflows::WorkflowLifecycleKind;
pub use codex_dynamic_workflows::WorkflowLog;
pub use codex_dynamic_workflows::WorkflowMetadata;
pub use codex_dynamic_workflows::WorkflowMetrics;
pub use codex_dynamic_workflows::WorkflowResult;
pub use codex_dynamic_workflows::WorkflowRevision;
pub use codex_dynamic_workflows::WorkflowRevisionResponse;
pub use codex_dynamic_workflows::WorkflowRevisionStatus;
pub use codex_dynamic_workflows::WorkflowRun;
pub use codex_dynamic_workflows::WorkflowScope;
pub use codex_dynamic_workflows::WorkflowStatus;
pub use codex_dynamic_workflows::WorkflowStep;
pub use codex_dynamic_workflows::WorkflowStepControlOutcome;
pub use codex_dynamic_workflows::WorkflowStepDisposition;
pub use codex_dynamic_workflows::WorkflowSummary;
pub use codex_dynamic_workflows::WorkflowTokenUsage;
pub use codex_dynamic_workflows::execution_transcript_limitation;
pub use codex_dynamic_workflows::list_execution_steps;
pub use codex_dynamic_workflows::list_persisted_summaries;
pub use codex_dynamic_workflows::read_execution_attempt;
pub use codex_dynamic_workflows::read_execution_snapshot;
pub use codex_dynamic_workflows::read_persisted;
pub(crate) use completion_ownership::owns_turn;
pub(crate) use notification::NotificationSource;
pub(crate) use notification_id::legacy_notice_id_for_request;
pub use service::DynamicWorkflowService;
pub(crate) use service::service;
pub(crate) use task_input::communication as workflow_task_communication;
pub(crate) use task_input::project as project_workflow_task_input;
pub(crate) use task_input::validate as validate_workflow_task;

use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use codex_history::UserInputOrigin;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use tokio::sync::OnceCell;

impl crate::CodexThread {
    /// Displays a selected immutable result without submitting model input.
    pub async fn display_workflow_result(
        &self,
        workflow_id: &str,
        execution_id: Option<&str>,
    ) -> Result<codex_protocol::workflow::WorkflowLifecycleEvent, String> {
        service(&self.session)
            .await?
            .display_result(workflow_id, execution_id)
            .await
    }

    /// Launches with this thread's native agent policy and records its identity for recovery.
    pub async fn start_workflow(
        &self,
        source: String,
        input: serde_json::Value,
    ) -> Result<WorkflowRun, String> {
        self.start_workflow_with_completion_mode(source, input, WorkflowCompletionMode::Display)
            .await
    }

    pub async fn start_workflow_with_completion_mode(
        &self,
        source: String,
        input: serde_json::Value,
        completion_mode: WorkflowCompletionMode,
    ) -> Result<WorkflowRun, String> {
        service(&self.session)
            .await?
            .start_from_with_completion_mode(
                Arc::clone(&self.session),
                source,
                input,
                NotificationSource::User,
                completion_mode,
            )
            .await
    }

    /// Reads a saved script under this caller's current permissions before launching it.
    pub async fn start_saved_workflow(
        &self,
        name: String,
        input: serde_json::Value,
    ) -> Result<WorkflowRun, String> {
        self.start_saved_workflow_with_completion_mode(name, input, WorkflowCompletionMode::Display)
            .await
    }

    pub async fn start_saved_workflow_with_completion_mode(
        &self,
        name: String,
        input: serde_json::Value,
        completion_mode: WorkflowCompletionMode,
    ) -> Result<WorkflowRun, String> {
        let turn = self.session.new_default_turn().await;
        let captured = self
            .session
            .capture_speculative_step_context(
                turn,
                &self.session.services.local_agent_runtime.shutdown,
            )
            .await
            .map_err(|error| error.to_string())?;
        step::validate_origin(&captured)?;
        drop(captured);
        let service = service(&self.session).await?;
        let saved = service.load_saved_from(&self.session, &name).await?;
        service
            .start_from_with_completion_mode(
                Arc::clone(&self.session),
                saved.source,
                input,
                NotificationSource::User,
                completion_mode,
            )
            .await
    }
}

/// Session-local activation is derived only from the current human input's resolved skills.
#[derive(Default)]
pub(crate) struct WorkflowServices {
    service: OnceCell<Arc<DynamicWorkflowService>>,
    origin_runtime_recorded: std::sync::OnceLock<()>,
    origin_runtime_persisted: OnceCell<()>,
    activated_turn: Mutex<Option<String>>,
}

impl WorkflowServices {
    pub(super) async fn persist_origin_runtime(
        &self,
        caller: &Session,
        turn: &TurnContext,
    ) -> Result<(), String> {
        self.origin_runtime_persisted
            .get_or_try_init(|| async {
                if caller.live_thread().is_none() {
                    return Err("workflow caller has no persistent thread".to_string());
                }
                if self.origin_runtime_recorded.get().is_none() {
                    if caller
                        .reference_context_item()
                        .await
                        .and_then(|context| context.multi_agent_version)
                        != Some(turn.multi_agent_version)
                    {
                        // An API-only launch has no normal turn context yet. Persist the
                        // validated caller's runtime without inventing a user turn or
                        // changing the model-visible reference context/world-state baseline.
                        let mut context = turn.to_turn_context_item();
                        context.turn_id = None;
                        context.root_turn_id = None;
                        if !caller
                            .persist_rollout_items(&[codex_history::RolloutItem::TurnContext(
                                context,
                            )])
                            .await
                        {
                            return Err("failed to persist workflow caller runtime".to_string());
                        }
                    }
                    // A failed durability barrier may be retried without appending again.
                    self.origin_runtime_recorded.get_or_init(|| ());
                }
                caller
                    .try_ensure_rollout_materialized(
                        codex_thread_store::PersistContext::ThreadPreparation,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                caller
                    .flush_rollout()
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub(crate) fn has_pending_work(&self) -> Result<bool, String> {
        let Some(service) = self.service.get() else {
            return Ok(false);
        };
        let mut offset = 0;
        loop {
            let runs = service.list_summaries(offset, codex_dynamic_workflows::MAX_PAGE_SIZE)?;
            if runs.iter().any(|run| {
                run.format_version == codex_dynamic_workflows::WORKFLOW_FORMAT_VERSION
                    && matches!(
                        run.status,
                        WorkflowStatus::Pending | WorkflowStatus::Running
                    )
            }) {
                return Ok(true);
            }
            if runs.len() < codex_dynamic_workflows::MAX_PAGE_SIZE {
                return Ok(false);
            }
            offset = offset.saturating_add(runs.len());
        }
    }

    pub(crate) fn activated(&self, turn: &str) -> bool {
        self.activated_turn
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_deref()
            == Some(turn)
    }
}

pub(crate) fn activate_turn(session: &Session, turn: &TurnContext, input: &[TurnInput]) {
    let human_input = input
        .iter()
        .filter_map(|item| match item {
            TurnInput::UserInput {
                content, metadata, ..
            } if metadata.origin == UserInputOrigin::User => Some(content.as_slice()),
            _ => None,
        })
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    let skills = turn.skills_snapshot();
    let activated = codex_skills::collect_explicit_skill_mentions(
        &human_input,
        skills.outcome(),
        &HashMap::new(),
    )
    .iter()
    .any(|skill| skill.name == "dynamic-workflow");
    let mut active = session
        .services
        .workflows
        .activated_turn
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if activated {
        *active = Some(turn.sub_id.clone());
    } else if active.as_deref() != Some(&turn.sub_id) {
        *active = None;
    }
}

/// Deterministic names reconcile attempts; completion routing verifies recorded child identity.
pub(crate) const CHILD_NAME_PREFIX: &str = "workflow_";

pub(crate) async fn is_workflow_child(
    session: &Session,
    source: &codex_protocol::protocol::SessionSource,
) -> bool {
    let runtime = &session.services.local_agent_runtime;
    let Ok(state) = runtime.execution_control.snapshot().await else {
        return false;
    };
    if state.root_thread_id == session.thread_id {
        return false;
    }
    let Ok(root) = runtime.workflow_thread(state.root_thread_id).await else {
        return false;
    };
    if let Some(service) = root.session.services.workflows.service.get()
        && let Some(path) = source.get_agent_path()
        && service.owns_child(session.thread_id, &path).await
    {
        return true;
    }
    // A resumed child can precede the root's first workflow-service access. Read the
    // persisted run records directly; names alone never grant direct human input.
    let config = root.session.get_config().await;
    let directory = config.codex_home.join("workflow-runs");
    let child_id = session.thread_id.to_string();
    let root_id = state.root_thread_id.to_string();
    let mut offset = 0;
    loop {
        let Ok(runs) = codex_dynamic_workflows::list_persisted(
            &directory,
            None,
            offset,
            codex_dynamic_workflows::MAX_PAGE_SIZE,
        ) else {
            return false;
        };
        if runs.iter().any(|run| {
            run.owner_thread_id == root_id
                && run
                    .steps
                    .values()
                    .flat_map(|step| &step.attempts)
                    .any(|attempt| attempt.child_thread_id.as_deref() == Some(child_id.as_str()))
        }) {
            return true;
        }
        if runs.len() < codex_dynamic_workflows::MAX_PAGE_SIZE {
            return false;
        }
        offset = offset.saturating_add(runs.len());
    }
}
