//! A real pre-instrumentation VM must obey durable control fencing before its drainer runs.

use codex_code_mode_protocol::CellId;
use codex_code_mode_protocol::CodeModeNestedToolCall;
use codex_code_mode_protocol::CodeModeSession;
use codex_code_mode_protocol::CodeModeSessionCellExecutionLimits;
use codex_code_mode_protocol::CodeModeSessionDelegate;
use codex_code_mode_protocol::CodeModeSessionProvider;
use codex_code_mode_protocol::CodeModeSessionProviderFuture;
use codex_code_mode_protocol::CodeModeSessionResultFuture;
use codex_code_mode_protocol::CodeModeToolKind;
use codex_code_mode_protocol::ExecuteRequest;
use codex_code_mode_protocol::StartedCell;
use codex_code_mode_protocol::WaitOutcome;
use codex_code_mode_protocol::WaitRequest;
use codex_code_mode_runtime::InProcessCodeModeSession;
use codex_dynamic_workflows::AgentOutcome;
use codex_dynamic_workflows::AgentRequest;
use codex_dynamic_workflows::AgentStartError;
use codex_dynamic_workflows::AgentStartMode;
use codex_dynamic_workflows::RevisionApplyMode;
use codex_dynamic_workflows::RevisionApplyRequest;
use codex_dynamic_workflows::RevisionStageRequest;
use codex_dynamic_workflows::WorkflowAdmissionGuard;
use codex_dynamic_workflows::WorkflowEngine;
use codex_dynamic_workflows::WorkflowFenceReceipt;
use codex_dynamic_workflows::WorkflowHost;
use codex_dynamic_workflows::WorkflowHostEvent;
use codex_dynamic_workflows::WorkflowObserver;
use codex_dynamic_workflows::WorkflowResult;
use codex_dynamic_workflows::WorkflowRevisionStatus;
use codex_protocol::ToolName;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Provider {
    delegate: Arc<Mutex<Option<Arc<dyn CodeModeSessionDelegate>>>>,
    old_renderer: Arc<AtomicBool>,
}
impl CodeModeSessionProvider for Provider {
    fn create_session(&self) -> CodeModeSessionProviderFuture<'_> {
        self.create_session_with_limits(CodeModeSessionCellExecutionLimits::default())
    }
    fn create_session_with_limits<'a>(
        &'a self,
        limits: CodeModeSessionCellExecutionLimits,
    ) -> CodeModeSessionProviderFuture<'a> {
        Box::pin(async move {
            Ok(Arc::new(Session {
                runtime: InProcessCodeModeSession::with_limits(limits),
                delegate: self.delegate.clone(),
                old_renderer: self.old_renderer.clone(),
            }) as _)
        })
    }
}
struct Session {
    runtime: InProcessCodeModeSession,
    delegate: Arc<Mutex<Option<Arc<dyn CodeModeSessionDelegate>>>>,
    old_renderer: Arc<AtomicBool>,
}
impl CodeModeSession for Session {
    fn execute<'a>(
        &'a self,
        request: ExecuteRequest,
        delegate: Arc<dyn CodeModeSessionDelegate>,
        preempt: Option<CancellationToken>,
    ) -> CodeModeSessionResultFuture<'a, StartedCell> {
        Box::pin(async move {
            self.old_renderer.store(
                !request.source.contains("const __workflowRevisionRuntime ="),
                Ordering::SeqCst,
            );
            *self.delegate.lock().map_err(|error| error.to_string())? = Some(delegate.clone());
            self.runtime.execute(request, delegate, preempt).await
        })
    }
    fn wait<'a>(
        &'a self,
        request: WaitRequest,
        preempt: Option<CancellationToken>,
    ) -> CodeModeSessionResultFuture<'a, WaitOutcome> {
        Box::pin(async move { self.runtime.wait(request, preempt).await })
    }
    fn terminate<'a>(&'a self, cell: CellId) -> CodeModeSessionResultFuture<'a, WaitOutcome> {
        Box::pin(async move { self.runtime.terminate(cell).await })
    }
    fn shutdown<'a>(&'a self) -> CodeModeSessionResultFuture<'a, ()> {
        Box::pin(async move { self.runtime.shutdown().await })
    }
}
#[derive(Default)]
struct Host {
    guard: Mutex<Option<WorkflowAdmissionGuard>>,
    observer: Mutex<Option<WorkflowObserver>>,
    request: Mutex<Option<AgentRequest>>,
    queued: Notify,
    release: Notify,
    paid: AtomicUsize,
}
impl WorkflowHost for Host {
    async fn register_admission_guard(
        &self,
        _run: String,
        guard: WorkflowAdmissionGuard,
    ) -> Result<(), String> {
        *self.guard.lock().map_err(|error| error.to_string())? = Some(guard);
        Ok(())
    }
    async fn register_observer(
        &self,
        _run: String,
        observer: WorkflowObserver,
    ) -> Result<(), String> {
        *self.observer.lock().map_err(|error| error.to_string())? = Some(observer);
        Ok(())
    }
    async fn schedule_agent(
        &self,
        request: AgentRequest,
        _mode: AgentStartMode,
    ) -> Result<String, AgentStartError> {
        *self.request.lock().map_err(|error| error.to_string())? = Some(request.clone());
        self.queued.notify_one();
        self.release.notified().await;
        let guard = self
            .guard
            .lock()
            .map_err(|error| error.to_string())?
            .clone()
            .ok_or("native guard missing")?;
        guard(&request.attempt_id).map_err(AgentStartError::NotStarted)?;
        self.paid.fetch_add(1, Ordering::SeqCst);
        Ok(request.attempt_id)
    }
    async fn wait_agent(
        &self,
        _request: AgentRequest,
        _child: String,
    ) -> Result<AgentOutcome, String> {
        Ok(AgentOutcome {
            result: Value::Null,
            usage: Value::Null,
            error: None,
        })
    }
    async fn fence_run(
        &self,
        _run: String,
        _revision: String,
    ) -> Result<WorkflowFenceReceipt, String> {
        let request = self
            .request
            .lock()
            .map_err(|error| error.to_string())?
            .clone()
            .ok_or("prepared native request missing")?;
        let observer = self
            .observer
            .lock()
            .map_err(|error| error.to_string())?
            .clone()
            .ok_or("native observer missing")?;
        observer(WorkflowHostEvent::StartRejected {
            attempt_id: request.attempt_id,
            reason: "known preacceptance refusal at the revision fence".into(),
        })?;
        self.release.notify_one();
        Ok(WorkflowFenceReceipt::default())
    }
    async fn stop_run(&self, _run: String) -> Result<(), String> {
        self.release.notify_one();
        Ok(())
    }
}

#[tokio::test]
async fn a_preinstrumentation_vm_cannot_admit_or_publish_after_apply_is_durable()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let path = AbsolutePathBuf::from_absolute_path_checked(directory.path())
        .map_err(|error| error.to_string())?;
    let provider = Arc::new(Provider::default());
    let host = Arc::new(Host::default());
    let first = WorkflowEngine::new(path.clone(), provider.clone(), host.clone())?;
    let source = "workflow.meta({name:'old-fence',description:'Retain the original renderer',phases:[]}); return await workflow.agent('prepared but not accepted',{label:'queued'});";
    let run = first.create_run("owner".into(), source.into(), Value::Null)?;
    drop(first);
    // This is the valid V2 manifest shape that predates compiler/control receipts.
    let file = directory.path().join(format!("{}.json", run.id));
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&file).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    manifest["steering"] = Value::Null;
    manifest["header"]
        .as_object_mut()
        .ok_or("header missing")?
        .remove("steering");
    std::fs::write(
        &file,
        serde_json::to_vec(&manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let engine = WorkflowEngine::new(path, provider.clone(), host.clone())?;
    engine.prepare_recovery(&run.id)?;
    let worker = {
        let engine = engine.clone();
        let id = run.id.clone();
        tokio::spawn(async move { engine.run(&id).await })
    };
    tokio::time::timeout(Duration::from_secs(20), host.queued.notified())
        .await
        .map_err(|error| error.to_string())?;
    assert!(provider.old_renderer.load(Ordering::SeqCst));
    let staged = engine.stage_revision(
        &run.id,
        RevisionStageRequest {
            command_id: "stage-old".into(),
            expected_control_revision: 0,
            source: source.replace("return await", "const old = await"),
            input: WorkflowResult::Absent,
        },
    )?;
    let applied = engine
        .apply_revision(
            &run.id,
            RevisionApplyRequest {
                command_id: "apply-old".into(),
                expected_control_revision: staged.state.control_revision,
                revision_id: staged.revision.id.clone(),
                mode: RevisionApplyMode::DrainAccepted,
            },
        )
        .await?;
    assert_eq!(applied.state.status, WorkflowRevisionStatus::Fencing);
    // No await before these checks: on this current-thread runtime the background
    // drainer has not yet closed supervision or called the host fence.
    let request = host
        .request
        .lock()
        .map_err(|error| error.to_string())?
        .clone()
        .ok_or("prepared attempt missing")?;
    let guard = host
        .guard
        .lock()
        .map_err(|error| error.to_string())?
        .clone()
        .ok_or("guard missing")?;
    assert!(
        guard(&request.attempt_id).is_err(),
        "durable Apply must reject native admission before background fencing runs"
    );
    let delegate = provider
        .delegate
        .lock()
        .map_err(|error| error.to_string())?
        .clone()
        .ok_or("actual VM delegate missing")?;
    let published = delegate
        .invoke_tool(
            CodeModeNestedToolCall {
                cell_id: CellId::new("old-cell".into()),
                runtime_tool_call_id: "late-publish".into(),
                tool_name: ToolName::plain("workflow_host"),
                tool_kind: CodeModeToolKind::Function,
                input: Some(json!({"operation":"publish","value":"must not escape"})),
            },
            CancellationToken::new(),
        )
        .await;
    assert!(
        published.is_err(),
        "the original VM must not publish beyond its durable control fence"
    );
    assert_eq!(engine.get(&run.id)?.result, None);
    let ready = tokio::time::timeout(
        Duration::from_secs(20),
        engine.wait_revision_settlement(&run.id, &staged.revision.id),
    )
    .await
    .map_err(|error| error.to_string())??;
    assert_eq!(ready.state.status, WorkflowRevisionStatus::Ready);
    tokio::time::timeout(Duration::from_secs(20), worker)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())??;
    assert_eq!(
        (
            host.paid.load(Ordering::SeqCst),
            engine.get(&run.id)?.accepted_starts
        ),
        (0, 0)
    );
    Ok(())
}
