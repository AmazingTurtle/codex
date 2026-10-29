//! Known native refusals restore reservations without relaxing uncertain recovery.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use codex_code_mode_protocol::CodeModeSessionCellExecutionLimits;
use codex_code_mode_protocol::CodeModeSessionProvider;
use codex_code_mode_protocol::CodeModeSessionProviderFuture;
use codex_code_mode_runtime::InProcessCodeModeSession;
use codex_dynamic_workflows::AdmissionStatus;
use codex_dynamic_workflows::AgentOutcome;
use codex_dynamic_workflows::AgentRequest;
use codex_dynamic_workflows::AgentStartError;
use codex_dynamic_workflows::AgentStartMode;
use codex_dynamic_workflows::RunLimits;
use codex_dynamic_workflows::WorkflowAdmissionGuard;
use codex_dynamic_workflows::WorkflowEngine;
use codex_dynamic_workflows::WorkflowHost;
use codex_dynamic_workflows::WorkflowHostEvent;
use codex_dynamic_workflows::WorkflowObserver;
use codex_dynamic_workflows::WorkflowStatus;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::Notify;
use tokio::sync::mpsc;

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

struct Host {
    guard: Mutex<Option<WorkflowAdmissionGuard>>,
    observer: Mutex<Option<WorkflowObserver>>,
    requests: mpsc::UnboundedSender<AgentRequest>,
    release: Notify,
}

impl WorkflowHost for Host {
    async fn register_admission_guard(
        &self,
        _run_id: String,
        guard: WorkflowAdmissionGuard,
    ) -> Result<(), String> {
        *self.guard.lock().map_err(|error| error.to_string())? = Some(guard);
        Ok(())
    }

    async fn register_observer(
        &self,
        _run_id: String,
        observer: WorkflowObserver,
    ) -> Result<(), String> {
        *self.observer.lock().map_err(|error| error.to_string())? = Some(observer);
        Ok(())
    }

    async fn schedule_agent(
        &self,
        request: AgentRequest,
        mode: AgentStartMode,
    ) -> Result<String, AgentStartError> {
        if !matches!(mode, AgentStartMode::Start) {
            return Err(AgentStartError::Uncertain(
                "unexpected child recovery".into(),
            ));
        }
        let child = request.attempt_id.clone();
        self.requests
            .send(request)
            .map_err(|error| AgentStartError::NotStarted(error.to_string()))?;
        self.release.notified().await;
        Ok(child)
    }

    async fn wait_agent(
        &self,
        _request: AgentRequest,
        _child: String,
    ) -> Result<AgentOutcome, String> {
        Ok(AgentOutcome {
            result: json!({"accepted":true}),
            usage: json!({}),
            error: None,
        })
    }

    async fn stop_run(&self, _run_id: String) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test]
async fn known_native_refusal_restores_only_the_same_unaccepted_reservation() -> Result<(), String>
{
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let (requests, mut received) = mpsc::unbounded_channel();
    let host = Arc::new(Host {
        guard: Mutex::new(None),
        observer: Mutex::new(None),
        requests,
        release: Notify::new(),
    });
    let engine = WorkflowEngine::new(
        AbsolutePathBuf::from_absolute_path_checked(directory.path().join("owner"))
            .map_err(|error| error.to_string())?,
        Arc::new(Provider),
        Arc::clone(&host),
    )?;
    let run = engine.create_run_with_limits("owner".into(),
        "workflow.meta({name:'admission',description:'Retry one known native refusal',phases:[]}); return await workflow.agent('one worker');".into(),
        json!({}), RunLimits { max_agents: 1, max_tokens: None })?;
    let worker = {
        let engine = engine.clone();
        let id = run.id.clone();
        tokio::spawn(async move { engine.run(&id).await })
    };
    let request = tokio::time::timeout(Duration::from_secs(20), received.recv())
        .await
        .map_err(|error| error.to_string())?
        .ok_or("missing native request")?;
    let guard = host
        .guard
        .lock()
        .map_err(|error| error.to_string())?
        .clone()
        .ok_or("missing admission guard")?;
    let observer = host
        .observer
        .lock()
        .map_err(|error| error.to_string())?
        .clone()
        .ok_or("missing observer")?;
    guard(&request.attempt_id)?;
    observer(WorkflowHostEvent::StartUncertain {
        attempt_id: request.attempt_id.clone(),
        revision_id: request.revision_id.clone(),
    })?;
    assert!(
        guard(&request.attempt_id).is_err(),
        "uncertainty alone must not permit another native start"
    );
    let uncertain = engine.get(&run.id)?;
    assert_eq!(
        (
            uncertain.accepted_starts,
            uncertain.steps[&request.step_id].attempts[0].admission
        ),
        (0, AdmissionStatus::Uncertain)
    );

    observer(WorkflowHostEvent::StartRetryable {
        attempt_id: request.attempt_id.clone(),
    })?;
    let reserved = engine.get(&run.id)?;
    assert_eq!(
        (
            reserved.accepted_starts,
            reserved.steps[&request.step_id].attempts.len(),
            reserved.steps[&request.step_id].attempts[0].id.as_str(),
            reserved.steps[&request.step_id].attempts[0].admission
        ),
        (0, 1, request.attempt_id.as_str(), AdmissionStatus::Reserved)
    );
    let persisted = codex_dynamic_workflows::list_persisted(
        &AbsolutePathBuf::from_absolute_path_checked(directory.path())
            .map_err(|error| error.to_string())?,
        /*project*/ None,
        /*offset*/ 0,
        /*limit*/ 1,
    )?;
    assert_eq!(persisted.len(), 1);
    assert_eq!(
        (&persisted[0].steps, persisted[0].accepted_starts),
        (&reserved.steps, 0)
    );
    guard(&request.attempt_id)?;
    // Repeated known-refusal receipts are harmless until a child is actually accepted.
    observer(WorkflowHostEvent::StartRetryable {
        attempt_id: request.attempt_id.clone(),
    })?;
    observer(WorkflowHostEvent::StartUncertain {
        attempt_id: request.attempt_id.clone(),
        revision_id: request.revision_id.clone(),
    })?;
    observer(WorkflowHostEvent::StartAccepted {
        attempt_id: request.attempt_id.clone(),
        child_thread_id: request.attempt_id.clone(),
    })?;
    let accepted = engine.get(&run.id)?;
    assert!(
        observer(WorkflowHostEvent::StartRetryable {
            attempt_id: request.attempt_id.clone()
        })
        .is_err()
    );
    let unchanged = engine.get(&run.id)?;
    assert_eq!(
        (&unchanged.steps, unchanged.accepted_starts),
        (&accepted.steps, 1)
    );
    assert!(
        guard(&request.attempt_id).is_err(),
        "an accepted child cannot reuse the fresh-start reservation"
    );
    host.release.notify_one();
    let completed = tokio::time::timeout(Duration::from_secs(20), worker)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())??;
    assert_eq!(
        (
            completed.status,
            completed.accepted_starts,
            completed.steps[&request.step_id].attempts.len(),
            completed.result
        ),
        (
            WorkflowStatus::Succeeded,
            1,
            1,
            Some(json!({"accepted":true}))
        )
    );
    assert!(
        received.try_recv().is_err(),
        "the known refusal must not allocate another attempt or child request"
    );
    Ok(())
}
