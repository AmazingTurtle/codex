//! The production engine writer establishes pre-existing work for discovery tests.
use codex_dynamic_workflows::AgentOutcome;
use codex_dynamic_workflows::AgentRequest;
use codex_dynamic_workflows::AgentStartError;
use codex_dynamic_workflows::AgentStartMode;
use codex_dynamic_workflows::WorkflowEngine;
use codex_dynamic_workflows::WorkflowHost;
use codex_dynamic_workflows::WorkflowRun;
use codex_protocol::ThreadId;
use std::path::Path;
use std::sync::Arc;

struct NoAgentExecution;

impl WorkflowHost for NoAgentExecution {
    async fn schedule_agent(
        &self,
        _request: AgentRequest,
        _mode: AgentStartMode,
    ) -> Result<String, AgentStartError> {
        panic!("discovery fixture never schedules agents")
    }
    async fn wait_agent(
        &self,
        _request: AgentRequest,
        _child_thread_id: String,
    ) -> Result<AgentOutcome, String> {
        panic!("discovery fixture never waits for agents")
    }
    async fn stop_run(&self, _run_id: String) -> Result<(), String> {
        panic!("discovery fixture never controls agents")
    }
}

pub(super) fn create(
    home: &Path,
    owner: ThreadId,
    project: &Path,
) -> color_eyre::Result<WorkflowRun> {
    let provider = codex_code_mode::ProcessOwnedCodeModeSessionProvider::with_host_program(
        codex_utils_cargo_bin::cargo_bin("codex-code-mode-host")?,
    );
    let state_root = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
        home.join("workflow-runs").join(owner.to_string()),
    )?;
    let engine = WorkflowEngine::new(state_root, Arc::new(provider), Arc::new(NoAgentExecution))
        .map_err(color_eyre::eyre::Error::msg)?
        .with_project_root(
            codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(project)?,
        );
    engine
        .create_run(
            owner.to_string(),
            "workflow.meta({name: 'existing-workflow', description: 'Discover persisted work', phases: []});\nreturn {answer: 42};".into(),
            serde_json::Value::Null,
        )
        .map_err(color_eyre::eyre::Error::msg)
}
