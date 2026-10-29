//! Production JavaScript execution with only the external agent boundary simulated.
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use codex_code_mode_protocol::CodeModeSessionCellExecutionLimits;
use codex_code_mode_protocol::CodeModeSessionProvider;
use codex_code_mode_protocol::CodeModeSessionProviderFuture;
use codex_code_mode_runtime::InProcessCodeModeSession;
use codex_dynamic_workflows::AgentOutcome;
use codex_dynamic_workflows::AgentRequest;
use codex_dynamic_workflows::AgentStartError;
use codex_dynamic_workflows::AgentStartMode;
use codex_dynamic_workflows::WorkflowEngine;
use codex_dynamic_workflows::WorkflowHost;
use codex_dynamic_workflows::WorkflowHostEvent;
use codex_dynamic_workflows::WorkflowObserver;
use codex_dynamic_workflows::WorkflowStatus;
use codex_dynamic_workflows::WorkflowTokenUsage;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tokio::sync::Notify;

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
    calls: Mutex<Vec<AgentRequest>>,
    fast_second_stage: Notify,
    fail_once: AtomicBool,
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
        let child = request.attempt_id.clone();
        self.calls
            .lock()
            .map_err(|error| AgentStartError::Uncertain(error.to_string()))?
            .push(request);
        Ok(child)
    }
    async fn wait_agent(
        &self,
        request: AgentRequest,
        _child: String,
    ) -> Result<AgentOutcome, String> {
        if request.label == "slow-first" {
            self.fast_second_stage.notified().await;
        }
        if request.label == "fast-second" {
            self.fast_second_stage.notify_one();
        }
        let observer = self
            .observer
            .lock()
            .map_err(|error| error.to_string())?
            .clone();
        if let Some(observer) = observer {
            observer(WorkflowHostEvent::Usage {
                attempt_id: request.attempt_id.clone(),
                thread_id: request.attempt_id.clone(),
                usage: WorkflowTokenUsage {
                    input_tokens: 5,
                    output_tokens: 2,
                    cached_input_tokens: 0,
                    total_tokens: 7,
                },
            })?;
            observer(WorkflowHostEvent::UsageComplete {
                attempt_id: request.attempt_id.clone(),
                thread_id: request.attempt_id.clone(),
            })?;
        }
        Ok(AgentOutcome {
            result: json!(request.label),
            usage: json!({}),
            error: (request.label == "confirmed-failure"
                || (request.label == "retry-final"
                    && self.fail_once.swap(/*val*/ false, Ordering::SeqCst)))
            .then(|| "confirmed child failure".into()),
        })
    }
    async fn stop_run(&self, _run_id: String) -> Result<(), String> {
        Ok(())
    }
}

fn engine(path: &std::path::Path, host: Arc<Host>) -> Result<WorkflowEngine<Host>, String> {
    let path =
        AbsolutePathBuf::from_absolute_path_checked(path).map_err(|error| error.to_string())?;
    WorkflowEngine::new(path, Arc::new(Provider), host)
}

fn script(body: &str) -> String {
    format!(
        "workflow.meta({{name:'scopes',description:'Scoped async regression',phases:[{{title:'left'}},{{title:'right'}}]}});\n{body}"
    )
}

#[tokio::test]
async fn native_await_preserves_parallel_phase_and_identity_across_replay() -> Result<(), String> {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    host.fail_once.store(/*val*/ true, Ordering::SeqCst);
    let engine = engine(directory.path(), host.clone())?;
    let run = engine
        .create_run(
            "owner".into(),
            script(
                r#"
const results = await workflow.parallel(['left','right'].map((phase,index) => async () => {
  workflow.phase(phase);
  await Promise.resolve();
  await new Promise(resolve => setTimeout(resolve, index === 0 ? 10 : 0));
  await {then(resolve) { resolve(); }};
  return workflow.agent(phase, {label:phase});
}));
await workflow.agent('retry the final step', {label:'retry-final'});
return {results, hidden:typeof __workflowContextGet, nativeTools:typeof tools};
"#,
            ),
            Value::Null,
        )
        .unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), engine.run(&run.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        completed.status,
        WorkflowStatus::Failed,
        "{:?}",
        completed.error
    );
    let mut phases = completed
        .steps
        .values()
        .filter(|step| step.label != "retry-final")
        .map(|step| (step.label.as_str(), step.phase.as_deref()))
        .collect::<Vec<_>>();
    phases.sort();
    assert_eq!(
        phases,
        vec![("left", Some("left")), ("right", Some("right"))]
    );
    let replayed = engine.recover(&run.id).await.unwrap();
    assert_eq!(
        replayed.status,
        WorkflowStatus::Succeeded,
        "{:?}",
        replayed.error
    );
    assert_eq!(
        replayed.result,
        Some(json!({"results":["left","right"],"hidden":"undefined","nativeTools":"undefined"}))
    );
    for (id, step) in completed
        .steps
        .iter()
        .filter(|(_, step)| step.label != "retry-final")
    {
        assert_eq!(replayed.steps.get(id), Some(step));
    }
    assert_eq!(host.calls.lock().unwrap().len(), 4);
    Ok(())
}

#[tokio::test]
async fn pipeline_streams_items_and_only_confirmed_failures_become_null() -> Result<(), String> {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    let engine = engine(directory.path(), host.clone())?;
    let run = engine.create_run("owner".into(), script(r#"
return workflow.pipeline(['slow','fast','failed'],
  async item => workflow.agent(item, {label:item === 'failed' ? 'confirmed-failure' : `${item}-first`}),
  async (_value,item) => workflow.agent(item, {label:`${item}-second`}));
"#), Value::Null).unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), engine.run(&run.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        completed.status,
        WorkflowStatus::Succeeded,
        "{:?}",
        completed.error
    );
    assert_eq!(
        completed.result,
        Some(json!(["slow-second", "fast-second", null]))
    );
    let labels = host
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call.label.clone())
        .collect::<Vec<_>>();
    assert!(!labels.iter().any(|label| label == "failed-second"));
    assert!(
        labels
            .iter()
            .position(|label| label == "fast-second")
            .unwrap()
            < labels
                .iter()
                .position(|label| label == "slow-second")
                .unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn workflow_guards_clock_random_and_private_scope_without_changing_explicit_dates()
-> Result<(), String> {
    let directory = tempfile::tempdir().unwrap();
    let engine = engine(directory.path(), Arc::new(Host::default()))?;
    let run = engine
        .create_run(
            "owner".into(),
            script(
                r#"
const blocked = [];
for (const read of [() => Date.now(), () => new Date(), () => Date(), () => Math.random()]) {
  try { read(); blocked.push(false); } catch { blocked.push(true); }
}
return {blocked, year:new Date('2020-01-01T00:00:00Z').getUTCFullYear(),
  hidden:(new Function('return typeof __workflowContextSet'))()};
"#,
            ),
            Value::Null,
        )
        .unwrap();
    let completed = engine.run(&run.id).await.unwrap();
    assert_eq!(
        completed.status,
        WorkflowStatus::Succeeded,
        "{:?}",
        completed.error
    );
    assert_eq!(
        completed.result,
        Some(json!({"blocked":[true,true,true,true],"year":2020,"hidden":"undefined"}))
    );
    Ok(())
}

#[tokio::test]
async fn synchronous_budget_branches_replay_after_paid_failure_and_process_restart()
-> Result<(), String> {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    host.fail_once.store(/*val*/ true, Ordering::SeqCst);
    let first_engine = engine(directory.path(), host.clone())?;
    let run = first_engine
        .create_run_with_limits(
            "owner".into(),
            script(
                r#"
if (workflow.budget.spent() !== 0) throw new Error('initial budget branch changed');
await workflow.agent('paid first step', {label:'paid-first'});
const remaining = workflow.budget.remaining();
if (remaining !== 93) throw new Error('remaining budget branch changed');
await workflow.agent('retry final step', {label:'retry-final'});
return {remaining};
"#,
            ),
            Value::Null,
            codex_dynamic_workflows::RunLimits {
                max_agents: 32,
                max_tokens: Some(100),
            },
        )
        .unwrap();
    let failed = first_engine.run(&run.id).await.unwrap();
    assert_eq!(failed.status, WorkflowStatus::Failed, "{:?}", failed.error);
    assert_eq!(failed.metrics.usage.as_ref().unwrap().total_tokens, 14);
    assert_eq!(host.calls.lock().unwrap().len(), 2);
    let first_step = failed
        .steps
        .values()
        .find(|step| step.label == "paid-first")
        .unwrap()
        .clone();
    drop(first_engine);
    let recovered_host = Arc::new(Host::default());
    let recovered_engine = engine(directory.path(), recovered_host.clone())?;
    let recovered = recovered_engine.recover(&run.id).await.unwrap();
    assert_eq!(
        recovered.status,
        WorkflowStatus::Succeeded,
        "{:?}",
        recovered.error
    );
    assert_eq!(recovered.result, Some(json!({"remaining":93})));
    assert_eq!(recovered.steps.get(&first_step.id), Some(&first_step));
    assert_eq!(
        recovered_host
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.label.as_str())
            .collect::<Vec<_>>(),
        vec!["retry-final"]
    );
    assert_eq!(recovered.accepted_starts, 3);
    assert_eq!(recovered.metrics.usage.as_ref().unwrap().total_tokens, 21);
    Ok(())
}

#[tokio::test]
async fn unbounded_budget_reads_round_trip_through_effects_and_cached_replay() -> Result<(), String>
{
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    host.fail_once.store(/*val*/ true, Ordering::SeqCst);
    let first_engine = engine(directory.path(), host.clone())?;
    let run = first_engine
        .create_run(
            "owner".into(),
            script(
                r#"
if (workflow.budget.total !== Infinity) throw new Error('default total must be unbounded');
const initial = workflow.budget.remaining();
if (initial !== Infinity) throw new Error('initial remaining must be unbounded');
await workflow.agent('paid first step', {label:'paid-first'});
const afterPaid = workflow.budget.remaining();
if (afterPaid !== Infinity) throw new Error('paid remaining must stay unbounded');
await workflow.agent('retry final step', {label:'retry-final'});
return {initialUnbounded:initial === Infinity, paidUnbounded:afterPaid === Infinity};
"#,
            ),
            Value::Null,
        )
        .unwrap();
    let failed = first_engine.run(&run.id).await.unwrap();
    assert_eq!(failed.status, WorkflowStatus::Failed, "{:?}", failed.error);
    let serialized = serde_json::to_value(&failed).unwrap();
    let observations = serialized["budgetObservations"].as_object().unwrap();
    assert_eq!(observations.len(), 2);
    assert!(
        observations
            .values()
            .all(|observation| observation["total"].is_null())
    );
    let first_step = failed
        .steps
        .values()
        .find(|step| step.label == "paid-first")
        .unwrap()
        .clone();
    drop(first_engine);
    let recovered_host = Arc::new(Host::default());
    let recovered_engine = engine(directory.path(), recovered_host.clone())?;
    let recovered = recovered_engine.recover(&run.id).await.unwrap();
    assert_eq!(
        recovered.status,
        WorkflowStatus::Succeeded,
        "{:?}",
        recovered.error
    );
    assert_eq!(
        recovered.result,
        Some(json!({"initialUnbounded":true,"paidUnbounded":true}))
    );
    assert_eq!(recovered.steps.get(&first_step.id), Some(&first_step));
    assert_eq!(
        recovered_host
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.label.as_str())
            .collect::<Vec<_>>(),
        vec!["retry-final"]
    );
    assert_eq!(recovered.metrics.usage.as_ref().unwrap().total_tokens, 21);
    Ok(())
}

#[tokio::test]
async fn implicit_and_explicit_undefined_returns_complete_with_null() -> Result<(), String> {
    let directory = tempfile::tempdir().unwrap();
    let engine = engine(directory.path(), Arc::new(Host::default()))?;
    for body in [
        "workflow.log('implicit result');",
        "workflow.log('explicit result'); return undefined;",
    ] {
        let run = engine
            .create_run("owner".into(), script(body), Value::Null)
            .unwrap();
        let completed = engine.run(&run.id).await.unwrap();
        assert_eq!(
            completed.status,
            WorkflowStatus::Succeeded,
            "{:?}",
            completed.error
        );
        assert_eq!(completed.result, Some(Value::Null));
        assert_eq!(completed.logs.len(), 1);
        assert_eq!(completed.accepted_starts, 0);
    }
    Ok(())
}

#[tokio::test]
async fn optional_agent_keys_are_unique_per_scope_without_shifting_automatic_ids()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    let engine = engine(directory.path(), host.clone())?;
    let run = engine.create_run(
        "owner".into(),
        script(
            r#"
await workflow.agent('automatic first',{label:'first'});
await workflow.agent('keyed work',{label:'keyed',key:'stable/研究'});
await workflow.agent('automatic second',{label:'second'});
return workflow.parallel([
  () => workflow.agent('left scoped key',{label:'left',key:'same'}),
  () => workflow.agent('right scoped key',{label:'right',key:'same'})
]);
"#,
        ),
        Value::Null,
    )?;
    let completed = engine.run(&run.id).await?;
    assert_eq!(
        completed.status,
        WorkflowStatus::Succeeded,
        "{:?}",
        completed.error
    );
    let mut identities = completed
        .steps
        .values()
        .map(|step| (step.label.as_str(), step.logical_id.as_str()))
        .collect::<Vec<_>>();
    identities.sort();
    assert_eq!(
        identities,
        vec![
            ("first", "root/a0"),
            ("keyed", "root/key:stable%2F%E7%A0%94%E7%A9%B6"),
            ("left", "root/p2/item0/key:same"),
            ("right", "root/p2/item1/key:same"),
            ("second", "root/a1"),
        ]
    );
    assert_eq!(
        host.calls.lock().map_err(|error| error.to_string())?.len(),
        5
    );
    Ok(())
}

#[tokio::test]
async fn duplicate_and_oversized_keys_fail_before_accepting_another_child() -> Result<(), String> {
    for (body, accepted, message) in [
        (
            "await workflow.agent('one',{key:'same'}); return workflow.agent('two',{key:'same'});"
                .to_string(),
            1,
            "duplicate workflow agent key",
        ),
        (
            format!(
                "return workflow.agent('oversized',{{key:{}}});",
                serde_json::to_string(&"é".repeat(65)).map_err(|error| error.to_string())?
            ),
            0,
            "128 UTF-8 bytes",
        ),
    ] {
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let host = Arc::new(Host::default());
        let engine = engine(directory.path(), host.clone())?;
        let run = engine.create_run("owner".into(), script(&body), Value::Null)?;
        let completed = engine.run(&run.id).await?;
        assert_eq!(completed.status, WorkflowStatus::Failed);
        assert!(
            completed
                .error
                .as_deref()
                .is_some_and(|error| error.contains(message)),
            "{:?}",
            completed.error
        );
        assert_eq!(completed.accepted_starts, accepted);
        assert_eq!(
            host.calls.lock().map_err(|error| error.to_string())?.len(),
            accepted as usize
        );
    }
    Ok(())
}
