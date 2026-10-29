//! Real terminal executions must not accept commands that no VM can discover.
use codex_code_mode_protocol::CodeModeSessionCellExecutionLimits;
use codex_code_mode_protocol::CodeModeSessionProvider;
use codex_code_mode_protocol::CodeModeSessionProviderFuture;
use codex_code_mode_runtime::InProcessCodeModeSession;
use codex_dynamic_workflows::AgentOutcome;
use codex_dynamic_workflows::AgentRequest;
use codex_dynamic_workflows::AgentStartError;
use codex_dynamic_workflows::AgentStartMode;
use codex_dynamic_workflows::WorkflowControlOperation;
use codex_dynamic_workflows::WorkflowControlRequest;
use codex_dynamic_workflows::WorkflowControlStatus;
use codex_dynamic_workflows::WorkflowControlTarget;
use codex_dynamic_workflows::WorkflowEngine;
use codex_dynamic_workflows::WorkflowHost;
use codex_dynamic_workflows::WorkflowResult;
use codex_dynamic_workflows::WorkflowStatus;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

#[path = "controls/race.rs"]
mod race;

struct Provider;
impl CodeModeSessionProvider for Provider {
    fn create_session(&self) -> CodeModeSessionProviderFuture<'_> {
        self.create_session_with_limits(CodeModeSessionCellExecutionLimits::default())
    }
    fn create_session_with_limits<'a>(
        &'a self,
        limits: CodeModeSessionCellExecutionLimits,
    ) -> CodeModeSessionProviderFuture<'a> {
        Box::pin(async move { Ok(Arc::new(InProcessCodeModeSession::with_limits(limits)) as _) })
    }
}

#[derive(Default)]
struct Host {
    native_calls: AtomicUsize,
}
impl WorkflowHost for Host {
    async fn schedule_agent(
        &self,
        _request: AgentRequest,
        _mode: AgentStartMode,
    ) -> Result<String, AgentStartError> {
        self.native_calls.fetch_add(1, Ordering::SeqCst);
        Err(AgentStartError::NotStarted(
            "this zero-agent fixture must never schedule a child".into(),
        ))
    }
    async fn wait_agent(
        &self,
        _request: AgentRequest,
        _child: String,
    ) -> Result<AgentOutcome, String> {
        Err("this zero-agent fixture must never wait for a child".into())
    }
    async fn stop_run(&self, _run: String) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test]
async fn terminal_executions_reject_undiscoverable_future_commands_without_changing_their_result()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    let root = AbsolutePathBuf::from_absolute_path_checked(directory.path())
        .map_err(|error| error.to_string())?;
    let engine = WorkflowEngine::new(root, Arc::new(Provider), Arc::clone(&host))?;
    for (body, status) in [
        ("return {kept:true};", WorkflowStatus::Succeeded),
        (
            "throw new Error('expected terminal failure');",
            WorkflowStatus::Failed,
        ),
    ] {
        let source = format!(
            "workflow.meta({{name:'terminal-controls',description:'Keep terminal history immutable',phases:[]}});\n{body}"
        );
        let started = engine.create_run("owner".into(), source, json!({}))?;
        let terminal = tokio::time::timeout(
            std::time::Duration::from_secs(/*secs*/ 20),
            engine.run(&started.id),
        )
        .await
        .map_err(|error| error.to_string())??;
        assert_eq!(terminal.status, status);
        let snapshot = engine.get_execution_snapshot(&terminal.id, &terminal.execution_id)?;
        for (id, operation) in [
            (
                "skip-future",
                WorkflowControlOperation::Skip {
                    replacement: WorkflowResult::Absent,
                },
            ),
            (
                "edit-future",
                WorkflowControlOperation::EditQueued {
                    prompt: "This task cannot be reached by the terminal VM".into(),
                },
            ),
        ] {
            let request = WorkflowControlRequest {
                command_id: id.into(),
                expected_control_revision: engine.control_state(&terminal.id)?.control_revision,
                target: WorkflowControlTarget::FutureStep {
                    scope: "root".into(),
                    key: "undiscovered".into(),
                },
                operation,
            };
            let rejected = engine.control(&terminal.id, request.clone()).await?;
            assert_eq!(rejected.command.status, WorkflowControlStatus::Rejected);
            assert!(
                rejected
                    .command
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("stage and resume a new revision"))
            );
            assert_eq!(
                engine.control(&terminal.id, request).await?,
                rejected,
                "same command must not be re-enqueued or dispatched"
            );
            assert_eq!(
                engine.get_execution_snapshot(&terminal.id, &terminal.execution_id)?,
                snapshot
            );
            let unchanged = engine.get(&terminal.id)?;
            assert_eq!(
                (
                    unchanged.status,
                    unchanged.steps,
                    unchanged.result,
                    unchanged.accepted_starts,
                    unchanged.metrics
                ),
                (
                    terminal.status,
                    terminal.steps.clone(),
                    terminal.result.clone(),
                    terminal.accepted_starts,
                    terminal.metrics.clone()
                )
            );
        }
    }
    assert_eq!(host.native_calls.load(Ordering::SeqCst), 0);
    Ok(())
}
