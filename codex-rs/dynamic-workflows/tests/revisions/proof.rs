//! Real compiler/V8/engine revisions with a controlled native acceptance boundary.

use codex_code_mode_protocol::CellId;
use codex_code_mode_protocol::CodeModeSession;
use codex_code_mode_protocol::CodeModeSessionCellExecutionLimits;
use codex_code_mode_protocol::CodeModeSessionDelegate;
use codex_code_mode_protocol::CodeModeSessionProvider;
use codex_code_mode_protocol::CodeModeSessionProviderFuture;
use codex_code_mode_protocol::CodeModeSessionResultFuture;
use codex_code_mode_protocol::ExecuteRequest;
use codex_code_mode_protocol::StartedCell;
use codex_code_mode_protocol::WaitOutcome;
use codex_code_mode_protocol::WaitRequest;
use codex_code_mode_runtime::InProcessCodeModeSession;
use codex_dynamic_workflows::AdmissionStatus;
use codex_dynamic_workflows::AgentOutcome;
use codex_dynamic_workflows::AgentRequest;
use codex_dynamic_workflows::AgentStartError;
use codex_dynamic_workflows::AgentStartMode;
use codex_dynamic_workflows::RevisionApplyMode;
use codex_dynamic_workflows::RevisionApplyRequest;
use codex_dynamic_workflows::RevisionResumeRequest;
use codex_dynamic_workflows::RevisionStageRequest;
use codex_dynamic_workflows::WorkflowEngine;
use codex_dynamic_workflows::WorkflowFenceReceipt;
use codex_dynamic_workflows::WorkflowHost;
use codex_dynamic_workflows::WorkflowHostEvent;
use codex_dynamic_workflows::WorkflowObserver;
use codex_dynamic_workflows::WorkflowResult;
use codex_dynamic_workflows::WorkflowRevisionStatus;
use codex_dynamic_workflows::WorkflowTokenUsage;
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
use tokio_util::sync::CancellationToken;

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
struct StaleBodyProvider {
    omit_cached_call: Arc<AtomicBool>,
    replacements: Arc<AtomicUsize>,
}

impl CodeModeSessionProvider for StaleBodyProvider {
    fn create_session(&self) -> CodeModeSessionProviderFuture<'_> {
        self.create_session_with_limits(CodeModeSessionCellExecutionLimits::default())
    }
    fn create_session_with_limits<'a>(
        &'a self,
        limits: CodeModeSessionCellExecutionLimits,
    ) -> CodeModeSessionProviderFuture<'a> {
        Box::pin(async move {
            Ok(Arc::new(StaleBodySession {
                runtime: InProcessCodeModeSession::with_limits(limits),
                omit_cached_call: Arc::clone(&self.omit_cached_call),
                replacements: Arc::clone(&self.replacements),
            }) as _)
        })
    }
}

struct StaleBodySession {
    runtime: InProcessCodeModeSession,
    omit_cached_call: Arc<AtomicBool>,
    replacements: Arc<AtomicUsize>,
}

impl CodeModeSession for StaleBodySession {
    fn execute<'a>(
        &'a self,
        mut request: ExecuteRequest,
        delegate: Arc<dyn CodeModeSessionDelegate>,
        preempt: Option<CancellationToken>,
    ) -> CodeModeSessionResultFuture<'a, StartedCell> {
        Box::pin(async move {
            if self.omit_cached_call.swap(/*val*/ false, Ordering::SeqCst) {
                let authored_call = "workflow.agent('base',{label:'base'})";
                if request.source.matches(authored_call).count() != 1 {
                    return Err("stale-body fixture must replace exactly one cached call".into());
                }
                request.source = request
                    .source
                    .replace(authored_call, "Promise.resolve('base')");
                self.replacements.fetch_add(1, Ordering::SeqCst);
            }
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
    fn terminate<'a>(&'a self, cell_id: CellId) -> CodeModeSessionResultFuture<'a, WaitOutcome> {
        Box::pin(async move { self.runtime.terminate(cell_id).await })
    }
    fn shutdown<'a>(&'a self) -> CodeModeSessionResultFuture<'a, ()> {
        Box::pin(async move { self.runtime.shutdown().await })
    }
}

#[derive(Default)]
struct Host {
    calls: Mutex<Vec<AgentRequest>>,
    observer: Mutex<Option<WorkflowObserver>>,
}

impl WorkflowHost for Host {
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
        self.calls
            .lock()
            .map_err(|error| AgentStartError::Uncertain(error.to_string()))?
            .push(request.clone());
        let observer = self
            .observer
            .lock()
            .map_err(|error| AgentStartError::Uncertain(error.to_string()))?
            .clone()
            .ok_or_else(|| AgentStartError::Uncertain("workflow observer missing".into()))?;
        observer(WorkflowHostEvent::StartUncertain {
            attempt_id: request.attempt_id.clone(),
            revision_id: request.revision_id.clone(),
        })?;
        if request.label == "uncertain" {
            return Err(AgentStartError::Uncertain(
                "native acceptance receipt was lost".into(),
            ));
        }
        let child = format!("child-{}", request.attempt_id);
        observer(WorkflowHostEvent::StartAccepted {
            attempt_id: request.attempt_id,
            child_thread_id: child.clone(),
        })?;
        Ok(child)
    }
    async fn wait_agent(
        &self,
        request: AgentRequest,
        child: String,
    ) -> Result<AgentOutcome, String> {
        if let Some(observer) = self
            .observer
            .lock()
            .map_err(|error| error.to_string())?
            .clone()
        {
            observer(WorkflowHostEvent::Usage {
                attempt_id: request.attempt_id.clone(),
                thread_id: child.clone(),
                usage: WorkflowTokenUsage {
                    input_tokens: 5,
                    output_tokens: 2,
                    cached_input_tokens: 0,
                    total_tokens: 7,
                },
            })?;
            observer(WorkflowHostEvent::UsageComplete {
                attempt_id: request.attempt_id,
                thread_id: child,
            })?;
        }
        Ok(AgentOutcome {
            result: json!(request.label),
            usage: Value::Null,
            error: None,
        })
    }
    async fn fence_run(
        &self,
        _run: String,
        _expected_revision: String,
    ) -> Result<WorkflowFenceReceipt, String> {
        let calls = self.calls.lock().map_err(|error| error.to_string())?;
        Ok(WorkflowFenceReceipt {
            accepted_attempt_ids: calls
                .iter()
                .filter(|request| request.label != "uncertain")
                .map(|request| request.attempt_id.clone())
                .collect(),
            uncertain_attempt_ids: calls
                .iter()
                .filter(|request| request.label == "uncertain")
                .map(|request| request.attempt_id.clone())
                .collect(),
        })
    }
    async fn stop_run(&self, _run: String) -> Result<(), String> {
        Ok(())
    }
}

fn script(body: &str) -> String {
    format!(
        "workflow.meta({{name:'revision-proof',description:'Exercise native acceptance uncertainty'}});\n{body}"
    )
}

#[tokio::test]
async fn uncertain_native_acceptance_blocks_revision_without_dispatching_or_resetting_usage()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    let engine = WorkflowEngine::new(
        AbsolutePathBuf::from_absolute_path_checked(directory.path())
            .map_err(|error| error.to_string())?,
        Arc::new(Provider),
        host.clone(),
    )?;
    let run = engine.create_run(
        "owner".into(),
        script(
            r#"
await workflow.agent('complete the base',{label:'base'});
await workflow.agent('acceptance is uncertain',{label:'uncertain'});
return workflow.agent('old suffix must not start',{label:'old-suffix'});
"#,
        ),
        Value::Null,
    )?;
    let original = engine.run(&run.id).await?;
    let uncertain = original
        .steps
        .values()
        .find(|step| step.label == "uncertain")
        .and_then(|step| step.attempts.last())
        .ok_or("uncertain native attempt missing")?;
    assert_eq!(
        (
            original.accepted_starts,
            original
                .metrics
                .usage
                .as_ref()
                .map(|usage| usage.total_tokens),
            uncertain.admission
        ),
        (1, Some(7), AdmissionStatus::Uncertain)
    );
    let original_attempts = original.steps.clone();
    let state = engine.control_state(&run.id)?;
    let staged = engine.stage_revision(&run.id, RevisionStageRequest {
        command_id:"stage-after-uncertain".into(), expected_control_revision:state.control_revision,
        source:script("await workflow.agent('complete the base',{label:'base'}); return workflow.agent('new suffix must not start',{label:'new-suffix'});"),
        input:WorkflowResult::Absent,
    })?;
    engine
        .apply_revision(
            &run.id,
            RevisionApplyRequest {
                command_id: "apply-after-uncertain".into(),
                expected_control_revision: staged.state.control_revision,
                revision_id: staged.revision.id.clone(),
                mode: RevisionApplyMode::DrainAccepted,
            },
        )
        .await?;
    let state = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
        loop {
            let state = engine.control_state(&run.id)?;
            if state.status == WorkflowRevisionStatus::Blocked {
                return Ok::<_, String>(state);
            }
            ticks.tick().await;
        }
    })
    .await
    .map_err(|error| format!("uncertain fence did not become blocked: {error}"))??;
    assert!(
        engine
            .prepare_revision_resume(
                &run.id,
                RevisionResumeRequest {
                    command_id: "resume-blocked".into(),
                    expected_control_revision: state.control_revision,
                    revision_id: staged.revision.id,
                }
            )
            .is_err()
    );
    let after = engine.get(&run.id)?;
    assert_eq!(
        (
            after.accepted_starts,
            after.metrics.usage.as_ref().map(|usage| usage.total_tokens)
        ),
        (1, Some(7))
    );
    assert_eq!(after.steps, original_attempts);
    assert_eq!(
        host.calls
            .lock()
            .map_err(|error| error.to_string())?
            .iter()
            .map(|call| call.label.as_str())
            .collect::<Vec<_>>(),
        vec!["base", "uncertain"]
    );
    Ok(())
}

#[tokio::test]
async fn a_stale_vm_body_cannot_omit_a_cached_prefix_and_dispatch_fresh_work() -> Result<(), String>
{
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    let provider = Arc::new(StaleBodyProvider::default());
    let engine = WorkflowEngine::new(
        AbsolutePathBuf::from_absolute_path_checked(directory.path())
            .map_err(|error| error.to_string())?,
        provider.clone(),
        host.clone(),
    )?;
    let original = engine.create_run(
        "owner".into(),
        script("const base=await workflow.agent('base',{label:'base'}); return base;"),
        Value::Null,
    )?;
    let original = engine.run(&original.id).await?;
    assert_eq!(original.result, Some(json!("base")));
    let staged = engine.stage_revision(&original.id,RevisionStageRequest {
        command_id:"stage-stale-body".into(),expected_control_revision:engine.control_state(&original.id)?.control_revision,
        source:script("const base=await workflow.agent('base',{label:'base'}); const extra=await workflow.agent('extra',{label:'extra'}); return [base,extra];"),
        input:WorkflowResult::Absent,
    })?;
    assert_eq!(staged.revision.reusable_statements, 1);
    engine
        .apply_revision(
            &original.id,
            RevisionApplyRequest {
                command_id: "apply-stale-body".into(),
                expected_control_revision: staged.state.control_revision,
                revision_id: staged.revision.id.clone(),
                mode: RevisionApplyMode::DrainAccepted,
            },
        )
        .await?;
    let ready = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
        loop {
            let state = engine.control_state(&original.id)?;
            if state.status == WorkflowRevisionStatus::Ready {
                return Ok::<_, String>(state);
            }
            if state.status == WorkflowRevisionStatus::Blocked {
                return Err("completed prefix unexpectedly blocked the fixture fence".into());
            }
            ticks.tick().await;
        }
    })
    .await
    .map_err(|error| error.to_string())??;
    engine.prepare_revision_resume(
        &original.id,
        RevisionResumeRequest {
            command_id: "resume-stale-body".into(),
            expected_control_revision: ready.control_revision,
            revision_id: staged.revision.id,
        },
    )?;
    provider
        .omit_cached_call
        .store(/*val*/ true, Ordering::SeqCst);
    let rejected = engine.run_prepared_revision(&original.id).await?;
    assert!(
        rejected
            .error
            .as_deref()
            .is_some_and(|error| error.contains("omitted a recorded workflow effect")),
        "{:?}",
        rejected.error
    );
    assert_eq!(
        host.calls
            .lock()
            .map_err(|error| error.to_string())?
            .iter()
            .map(|call| call.label.as_str())
            .collect::<Vec<_>>(),
        vec!["base"]
    );
    assert_eq!(
        (
            rejected.accepted_starts,
            rejected
                .metrics
                .usage
                .as_ref()
                .map(|usage| usage.total_tokens)
        ),
        (1, Some(7))
    );
    assert_eq!(provider.replacements.load(Ordering::SeqCst), 1);
    Ok(())
}
