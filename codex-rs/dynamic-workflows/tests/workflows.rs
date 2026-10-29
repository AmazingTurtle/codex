use std::collections::HashSet;
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
use codex_dynamic_workflows::WorkflowLibrary;
use codex_dynamic_workflows::WorkflowScope;
use codex_dynamic_workflows::WorkflowStatus;
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

/// Only the external agent boundary is simulated. JavaScript, callback
/// serialization, replay and disk persistence use the production implementations.
#[derive(Default)]
struct Host {
    calls: Mutex<Vec<(AgentRequest, AgentStartMode)>>,
    fail_once: AtomicBool,
    uncertain_once: AtomicBool,
    rejected_once: AtomicBool,
    null_first: AtomicBool,
    block: AtomicBool,
    stop_aborts: AtomicBool,
    waiting: Notify,
    stopped: Notify,
    release: Notify,
    stopped_attempts: Mutex<HashSet<String>>,
}

impl WorkflowHost for Host {
    async fn schedule_agent(
        &self,
        request: AgentRequest,
        mode: AgentStartMode,
    ) -> Result<String, AgentStartError> {
        let child = format!("child-{}", request.attempt_id);
        if let AgentStartMode::Recover {
            child_thread_id: Some(existing),
        } = &mode
        {
            assert_eq!(existing, &child);
        }
        self.calls
            .lock()
            .map_err(|error| AgentStartError::Uncertain(error.to_string()))?
            .push((request, mode));
        if self.uncertain_once.swap(/*val*/ false, Ordering::SeqCst) {
            return Err(AgentStartError::Uncertain(
                "transport lost after spawn acceptance".into(),
            ));
        }
        if self.rejected_once.swap(/*val*/ false, Ordering::SeqCst) {
            return Err(AgentStartError::NotStarted(
                "agent preparation rejected before acceptance".into(),
            ));
        }
        Ok(child)
    }

    async fn wait_agent(
        &self,
        request: AgentRequest,
        _child_thread_id: String,
    ) -> Result<AgentOutcome, String> {
        if request.label == "second" && self.block.load(Ordering::SeqCst) {
            self.waiting.notify_one();
            self.release.notified().await;
        }
        let error = if self
            .stopped_attempts
            .lock()
            .map_err(|error| error.to_string())?
            .contains(&request.attempt_id)
        {
            Some("agent interrupted by stop".into())
        } else if request.label == "second"
            && self.fail_once.swap(/*val*/ false, Ordering::SeqCst)
        {
            Some("agent failed".into())
        } else {
            None
        };
        let result = if request.label == "first" && self.null_first.load(Ordering::SeqCst) {
            Value::Null
        } else {
            json!({"step":request.label})
        };
        Ok(AgentOutcome {
            result,
            usage: json!({"tokens":7}),
            error,
        })
    }

    async fn stop_run(&self, run_id: String) -> Result<(), String> {
        let calls = self.calls.lock().map_err(|error| error.to_string())?;
        let mut stopped = self
            .stopped_attempts
            .lock()
            .map_err(|error| error.to_string())?;
        for (request, _) in calls
            .iter()
            .filter(|(request, _)| request.run_id == run_id && request.label == "second")
        {
            // Some scenarios hold result observation after inference succeeded;
            // only the explicit stop scenario models a still-running child.
            if self.stop_aborts.load(Ordering::SeqCst) {
                stopped.insert(request.attempt_id.clone());
            }
        }
        self.block.store(/*val*/ false, Ordering::SeqCst);
        self.stopped.notify_one();
        Ok(())
    }
}

fn absolute(path: &std::path::Path) -> AbsolutePathBuf {
    // Every caller supplies an OS-created temporary directory or its descendants.
    match AbsolutePathBuf::from_absolute_path_checked(path) {
        Ok(path) => path,
        Err(error) => panic!("temporary workflow fixture path must be absolute: {error}"),
    }
}

fn engine(path: &std::path::Path, host: Arc<Host>) -> WorkflowEngine<Host> {
    match WorkflowEngine::new(absolute(path), Arc::new(Provider), host) {
        Ok(engine) => engine,
        Err(error) => panic!("temporary workflow fixture must initialize: {error}"),
    }
}

fn script(body: &str) -> String {
    format!(
        "workflow.meta({{name:'fixture',description:'Engine regression',phases:[{{title:'research'}},{{title:'dispatched'}}]}});\n{body}"
    )
}

fn step<'a>(
    run: &'a codex_dynamic_workflows::WorkflowRun,
    label: &str,
) -> &'a codex_dynamic_workflows::WorkflowStep {
    match run.steps.values().find(|step| step.label == label) {
        Some(step) => step,
        None => panic!("workflow fixture has no step labeled {label}"),
    }
}

fn assert_live_snapshot(
    mut actual: codex_dynamic_workflows::WorkflowRun,
    expected: &codex_dynamic_workflows::WorkflowRun,
) {
    assert!(matches!(
        expected.status,
        WorkflowStatus::Pending | WorkflowStatus::Running
    ));
    assert_eq!(actual.status, expected.status);
    // Reads project elapsed wall time for live runs without changing durable state.
    assert!(actual.metrics.wall_duration_ms >= expected.metrics.wall_duration_ms);
    actual.metrics.wall_duration_ms = expected.metrics.wall_duration_ms;
    assert_eq!(actual, *expected);
}

async fn timeout<T>(future: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(Duration::from_secs(/*secs*/ 20), future).await {
        Ok(result) => result,
        Err(error) => panic!("workflow fixture exceeded its deadline: {error}"),
    }
}

#[tokio::test]
async fn real_javascript_branches_loops_parallel_and_publishes_without_general_tools() {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    let engine = engine(directory.path(), host.clone());
    let mut events = engine.subscribe();
    let run = engine
        .create_run(
            "owner".into(),
            script(
                r#"
await workflow.phase("research");
const results = await workflow.parallel(workflow.args.names.map(name => async () => {
  if (workflow.args.enabled) return await workflow.agent(name, {label: name});
  return null;
}));
return {results, tools: typeof tools, allTools: typeof ALL_TOOLS,
  process: typeof process, fetch: typeof fetch, require: typeof require,
  immutable: Object.isFrozen(workflow.args) && Object.isFrozen(workflow.args.names)};
"#,
            ),
            json!({"names":["one","two","three"],"enabled":true}),
        )
        .unwrap();
    assert_eq!(events.recv().await.unwrap(), run);
    let completed = timeout(engine.run(&run.id)).await.unwrap();
    assert_eq!(completed.status, WorkflowStatus::Succeeded);
    assert_eq!(completed.phase, Some("research".into()));
    assert_eq!(
        completed.result,
        Some(
            json!({"results":[{"step":"one"},{"step":"two"},{"step":"three"}],
        "tools":"undefined","allTools":"undefined","process":"undefined",
        "fetch":"undefined","require":"undefined","immutable":true})
        )
    );
    assert_eq!(engine.get(&run.id).unwrap(), completed);
    assert_eq!(host.calls.lock().unwrap().len(), 3);
}

const TWO_STEPS: &str = r#"
workflow.meta({name:'two-steps',description:'Sequential cache and recovery',phases:[{title:'first phase'},{title:'second phase'}]});
await workflow.phase("first phase");
const first = await workflow.agent("one", {label: "first"});
await workflow.phase("second phase");
const second = await workflow.agent("two", {label: "second"});
return [first, second];
"#;

#[tokio::test]
async fn retry_keeps_successful_results_and_allocates_only_confirmed_failed_attempt() {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    host.fail_once.store(/*val*/ true, Ordering::SeqCst);
    let engine = engine(directory.path(), host.clone());
    let run = engine
        .create_run("owner".into(), TWO_STEPS.into(), Value::Null)
        .unwrap();
    let failed = timeout(engine.run(&run.id)).await.unwrap();
    assert_eq!(failed.status, WorkflowStatus::Failed);
    let first = step(&failed, "first").clone();
    let completed = timeout(engine.retry(&run.id)).await.unwrap();
    assert_eq!(completed.status, WorkflowStatus::Succeeded);
    assert_eq!(step(&completed, "first"), &first);
    assert_eq!(completed.phases, vec!["first phase", "second phase"]);
    assert_eq!(
        step(&completed, "first").phase.as_deref(),
        Some("first phase")
    );
    assert_eq!(
        step(&completed, "second").phase.as_deref(),
        Some("second phase")
    );
    assert_eq!(step(&completed, "second").attempts.len(), 2);
    let calls = host.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls
            .iter()
            .map(|(request, _)| (
                request.run_sequence,
                request.step_sequence,
                request.attempt_number
            ))
            .collect::<Vec<_>>(),
        vec![(1, 1, 1), (1, 2, 1), (1, 2, 2)]
    );
    assert_eq!(
        engine
            .list("other-owner", /*offset*/ 0, /*limit*/ 50)
            .unwrap(),
        Vec::new()
    );
}

#[tokio::test]
async fn restart_requires_explicit_recovery_and_reconnects_the_same_unfinished_child() {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    host.block.store(/*val*/ true, Ordering::SeqCst);
    let first_engine = engine(directory.path(), host.clone());
    let run = first_engine
        .create_run("owner".into(), TWO_STEPS.into(), json!({"snapshot":1}))
        .unwrap();
    let worker = {
        let engine = first_engine.clone();
        let id = run.id.clone();
        tokio::spawn(async move { engine.run(&id).await })
    };
    timeout(host.waiting.notified()).await;
    let interrupted = first_engine.get(&run.id).unwrap();
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    timeout(host.stopped.notified()).await;
    drop(first_engine);
    host.block.store(/*val*/ false, Ordering::SeqCst);
    let restarted = engine(directory.path(), host.clone());
    assert_eq!(
        restarted.get(&run.id).unwrap().status,
        WorkflowStatus::Interrupted
    );
    assert!(restarted.run(&run.id).await.is_err());
    let completed = timeout(restarted.recover(&run.id)).await.unwrap();
    assert_eq!(completed.status, WorkflowStatus::Succeeded);
    assert_eq!(step(&completed, "first"), step(&interrupted, "first"));
    assert_eq!(step(&completed, "second").attempts.len(), 1);
    let calls = host.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[1].0, calls[2].0);
    assert_eq!(
        calls[2].1,
        AgentStartMode::Recover {
            child_thread_id: step(&interrupted, "second").attempts[0]
                .child_thread_id
                .clone()
        }
    );
}

#[tokio::test]
async fn stop_preserves_completed_steps_and_retry_reconciles_unfinished_attempt() {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    host.block.store(/*val*/ true, Ordering::SeqCst);
    host.stop_aborts.store(/*val*/ true, Ordering::SeqCst);
    let engine = engine(directory.path(), host.clone());
    let run = engine
        .create_run("owner".into(), TWO_STEPS.into(), Value::Null)
        .unwrap();
    let worker = {
        let engine = engine.clone();
        let id = run.id.clone();
        tokio::spawn(async move { engine.run(&id).await })
    };
    timeout(host.waiting.notified()).await;
    let stopped = engine.stop(&run.id).await.unwrap();
    assert_eq!(stopped.status, WorkflowStatus::Stopped);
    let completed = timeout(engine.retry(&run.id)).await.unwrap();
    timeout(worker).await.unwrap().unwrap();
    assert_eq!(completed.status, WorkflowStatus::Succeeded);
    assert_eq!(step(&completed, "first"), step(&stopped, "first"));
    assert_eq!(step(&completed, "second").attempts.len(), 2);
    let calls = host.calls.lock().unwrap();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[1].0, calls[2].0);
    assert_ne!(calls[2].0.attempt_id, calls[3].0.attempt_id);
}

#[tokio::test]
async fn unsupported_options_remain_visible_when_script_catches_them() {
    let directory = tempfile::tempdir().unwrap();
    let engine = engine(directory.path(), Arc::new(Host::default()));
    let run = engine
        .create_run(
            "owner".into(),
            script(
                r#"
await workflow.agent("first", {label:"same"});
try { await workflow.agent("second", {unknownOption:true}); } catch {}
return "caught";
"#,
            ),
            Value::Null,
        )
        .unwrap();
    let failed = timeout(engine.run(&run.id)).await.unwrap();
    assert_eq!(failed.status, WorkflowStatus::Failed);
    assert!(
        failed
            .error
            .unwrap()
            .contains("unsupported workflow agent option")
    );
}

#[tokio::test]
async fn unknown_spawn_outcome_recovers_original_attempt_without_creating_another() {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    host.uncertain_once.store(/*val*/ true, Ordering::SeqCst);
    let engine = engine(directory.path(), host.clone());
    let run = engine
        .create_run(
            "owner".into(),
            script("return await workflow.agent('one',{label:'first'});"),
            Value::Null,
        )
        .unwrap();
    let interrupted = timeout(engine.run(&run.id)).await.unwrap();
    assert_eq!(interrupted.status, WorkflowStatus::Failed);
    let completed = timeout(engine.recover(&run.id)).await.unwrap();
    assert_eq!(completed.status, WorkflowStatus::Succeeded);
    assert_eq!(step(&completed, "first").attempts.len(), 1);
    let calls = host.calls.lock().unwrap();
    assert_eq!(calls[0].0, calls[1].0);
    assert_eq!(
        calls[1].1,
        AgentStartMode::Recover {
            child_thread_id: None
        }
    );
}

#[tokio::test]
async fn json_prototype_keys_keep_their_shape_and_are_deeply_frozen() {
    let directory = tempfile::tempdir().unwrap();
    let engine = engine(directory.path(), Arc::new(Host::default()));
    let run = engine
        .create_run(
            "owner".into(),
            script(
                r#"
let immutable = false;
try { workflow.args.__proto__.value = 2; } catch { immutable = true; }
return {own:Object.hasOwn(workflow.args,'__proto__'),
  value:workflow.args.__proto__.value, frozen:Object.isFrozen(workflow.args.__proto__), immutable};
"#,
            ),
            serde_json::from_str(r#"{"__proto__":{"value":1}}"#).unwrap(),
        )
        .unwrap();
    let completed = timeout(engine.run(&run.id)).await.unwrap();
    assert_eq!(completed.status, WorkflowStatus::Succeeded);
    assert_eq!(
        completed.result,
        Some(json!({"own":true,"value":1,"frozen":true,"immutable":true}))
    );
}

struct HeldProvider(Arc<Notify>);

impl CodeModeSessionProvider for HeldProvider {
    fn create_session(&self) -> CodeModeSessionProviderFuture<'_> {
        Box::pin(async {
            self.0.notify_one();
            std::future::pending().await
        })
    }

    fn create_session_with_limits<'a>(
        &'a self,
        _limits: CodeModeSessionCellExecutionLimits,
    ) -> CodeModeSessionProviderFuture<'a> {
        self.create_session()
    }
}

#[tokio::test]
async fn stop_cancels_a_provider_open_that_never_completes() {
    let directory = tempfile::tempdir().unwrap();
    let opened = Arc::new(Notify::new());
    let project = absolute(&directory.path().join("project"));
    let engine = WorkflowEngine::new(
        absolute(&directory.path().join("owner")),
        Arc::new(HeldProvider(opened.clone())),
        Arc::new(Host::default()),
    )
    .unwrap()
    .with_project_root(project.clone());
    let run = engine
        .create_run("owner".into(), script("return 'done';"), Value::Null)
        .unwrap();
    let worker = {
        let engine = engine.clone();
        let id = run.id.clone();
        tokio::spawn(async move { engine.run(&id).await })
    };
    timeout(opened.notified()).await;
    let running = engine.get(&run.id).unwrap();
    assert_eq!(running.status, WorkflowStatus::Running);
    let mut persisted = codex_dynamic_workflows::list_persisted(
        &absolute(directory.path()),
        Some(&project),
        /*offset*/ 0,
        /*limit*/ 50,
    )
    .unwrap();
    assert_eq!(persisted.len(), 1);
    // Disk lists retain checkpoint time; a live read projects elapsed time forward.
    assert_live_snapshot(running.clone(), &persisted.remove(0));
    assert_live_snapshot(engine.get(&run.id).unwrap(), &running);
    assert_eq!(
        codex_dynamic_workflows::list_persisted(
            &absolute(directory.path()),
            Some(&absolute(&directory.path().join("other"))),
            /*offset*/ 0,
            /*limit*/ 50
        )
        .unwrap(),
        Vec::new()
    );
    let stopped = timeout(engine.stop(&run.id)).await.unwrap();
    assert_eq!(stopped.status, WorkflowStatus::Stopped);
    assert_eq!(timeout(worker).await.unwrap().unwrap(), stopped);
}

#[tokio::test]
async fn unused_engine_creates_no_artifacts_and_terminal_snapshots_survive_restart_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let anchor = directory.path().join("workflow-runs");
    let state_root = anchor.join("owner");
    let host = Arc::new(Host::default());
    let first = engine(&state_root, host.clone());
    let _events = first.subscribe();
    assert_eq!(
        first.list("owner", /*offset*/ 0, /*limit*/ 50).unwrap(),
        Vec::new()
    );
    assert!(first.get(&uuid::Uuid::new_v4().to_string()).is_err());
    assert!(
        !anchor.exists(),
        "constructing and reading an unused workflow service must not create local persistence"
    );
    let run = first
        .create_run("owner".into(), script("return 'done';"), Value::Null)
        .unwrap();
    assert!(
        state_root.is_dir(),
        "the first accepted run must create its durable store"
    );
    assert_live_snapshot(first.get(&run.id).unwrap(), &run);
    let completed = timeout(first.run(&run.id)).await.unwrap();
    drop(first);
    let restarted = engine(&state_root, host);
    assert_eq!(restarted.get(&run.id).unwrap(), completed);
    assert_eq!(
        restarted.list("owner", /*offset*/ 0, /*limit*/ 50).unwrap(),
        vec![completed]
    );
}

#[test]
fn saved_project_scripts_override_user_scripts_and_run_snapshots_are_immutable() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let home = directory.path().join("home");
    let library = WorkflowLibrary::new(absolute(&project), absolute(&home));
    let user_saved = library
        .save(
            "sample",
            "workflow.meta({name:'sample',description:'Saved sample',phases:[]}); return 'user';",
            WorkflowScope::User,
        )
        .unwrap();
    let saved = library
        .save(
            "sample",
            "workflow.meta({name:'sample',description:'Saved sample',phases:[]}); return 'project';",
            WorkflowScope::Project,
        )
        .unwrap();
    assert_eq!(library.resolve("sample").unwrap(), saved);
    assert_eq!(
        library.list(/*offset*/ 0, /*limit*/ 50).unwrap(),
        vec![saved.clone(), user_saved]
    );
    let engine = engine(&directory.path().join("runs"), Arc::new(Host::default()));
    let run = engine
        .create_run("owner".into(), saved.source, json!({"input":"snapshot"}))
        .unwrap();
    std::fs::write(
        project.join(".codex/workflows/sample.js"),
        "workflow.meta({name:'sample',description:'Edited sample',phases:[]}); return 'edited';",
    )
    .unwrap();
    assert_live_snapshot(engine.get(&run.id).unwrap(), &run);
    assert!(
        library
            .save("../escape", "workflow.meta({name:'sample',description:'Saved sample',phases:[]}); return 'bad';", WorkflowScope::Project)
            .is_err()
    );
    assert!(
        library
            .save("sample", "workflow.meta({name:'sample',description:'Saved sample',phases:[]}); return 'overwrite';", WorkflowScope::Project)
            .is_err()
    );
    library.delete("project:sample").unwrap();
    assert_eq!(
        library.resolve("sample").unwrap().scope,
        WorkflowScope::User
    );
}

#[tokio::test]
async fn queued_run_interrupted_before_launch_requires_explicit_recovery_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    let first = engine(directory.path(), host.clone());
    let run = first
        .create_run("owner".into(), script("return 'recovered';"), Value::Null)
        .unwrap();
    drop(first);
    let restarted = engine(directory.path(), host);
    assert_eq!(
        restarted.get(&run.id).unwrap().status,
        WorkflowStatus::Interrupted
    );
    assert!(restarted.run(&run.id).await.is_err());
    let completed = timeout(restarted.recover(&run.id)).await.unwrap();
    assert_eq!(completed.status, WorkflowStatus::Succeeded);
    assert_eq!(completed.result, Some(json!("recovered")));
}

#[tokio::test]
async fn changed_retry_branch_cannot_reuse_a_stable_step_with_different_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    host.fail_once.store(/*val*/ true, Ordering::SeqCst);
    let engine = engine(directory.path(), host.clone());
    let run = engine
        .create_run(
            "owner".into(),
            script(
                r#"
let message;
try { await workflow.agent("two", {label:"second"}); message="success"; }
catch { message="failure"; }
try { await workflow.agent(message, {label:"first"}); } catch {}
if (message === "failure") throw new Error("retry after failed child");
return "caught inconsistent step";
"#,
            ),
            Value::Null,
        )
        .unwrap();
    let failed = timeout(engine.run(&run.id)).await.unwrap();
    assert_eq!(failed.status, WorkflowStatus::Failed);
    let preserved = step(&failed, "first").clone();
    let rejected = timeout(engine.retry(&run.id)).await.unwrap();
    assert_eq!(rejected.status, WorkflowStatus::Failed);
    assert!(
        rejected
            .error
            .as_ref()
            .unwrap()
            .contains("inconsistent replay arguments")
    );
    assert_eq!(step(&rejected, "first"), &preserved);
    assert_eq!(host.calls.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn successful_json_null_is_cached_and_published_as_a_present_value_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    host.fail_once.store(/*val*/ true, Ordering::SeqCst);
    host.null_first.store(/*val*/ true, Ordering::SeqCst);
    let first = engine(directory.path(), host.clone());
    let run = first
        .create_run(
            "owner".into(),
            script(
                r#"
const first = await workflow.agent("one", {label:"first"});
await workflow.agent("two", {label:"second"});
return first;
"#,
            ),
            Value::Null,
        )
        .unwrap();
    let failed = timeout(first.run(&run.id)).await.unwrap();
    assert_eq!(failed.status, WorkflowStatus::Failed);
    assert_eq!(step(&failed, "first").result, Some(Value::Null));
    drop(first);
    let restarted = engine(directory.path(), host.clone());
    let completed = timeout(restarted.retry(&run.id)).await.unwrap();
    assert_eq!(completed.status, WorkflowStatus::Succeeded);
    assert_eq!(step(&completed, "first"), step(&failed, "first"));
    assert_eq!(completed.result, Some(Value::Null));
    assert_eq!(restarted.get(&run.id).unwrap(), completed);
    assert_eq!(host.calls.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn fire_and_forget_agent_cannot_hide_unfinished_work_in_a_successful_run() {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    host.block.store(/*val*/ true, Ordering::SeqCst);
    let engine = engine(directory.path(), host);
    let run = engine
        .create_run(
            "owner".into(),
            script(
                r#"
void workflow.agent("still running", {label:"second"});
await workflow.phase("dispatched");
return "main script finished";
"#,
            ),
            Value::Null,
        )
        .unwrap();
    let completed = timeout(engine.run(&run.id)).await.unwrap();
    assert_eq!(completed.status, WorkflowStatus::Failed);
    assert!(
        completed
            .error
            .as_ref()
            .unwrap()
            .contains("unfinished agent or combinator operations")
    );
    assert_eq!(
        step(&completed, "second").attempts[0].status,
        WorkflowStatus::Running
    );
}

#[tokio::test]
async fn known_preacceptance_rejection_is_retryable_without_recovering_a_nonexistent_child() {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    host.rejected_once.store(/*val*/ true, Ordering::SeqCst);
    let engine = engine(directory.path(), host.clone());
    let run = engine
        .create_run(
            "owner".into(),
            script("return await workflow.agent('one',{label:'first'});"),
            Value::Null,
        )
        .unwrap();
    let rejected = timeout(engine.run(&run.id)).await.unwrap();
    assert_eq!(rejected.status, WorkflowStatus::Failed);
    assert_eq!(
        step(&rejected, "first").attempts[0].status,
        WorkflowStatus::Failed
    );
    let completed = timeout(engine.retry(&run.id)).await.unwrap();
    assert_eq!(completed.status, WorkflowStatus::Succeeded);
    assert_eq!(step(&completed, "first").attempts.len(), 2);
    let calls = host.calls.lock().unwrap();
    assert_eq!(calls[0].1, AgentStartMode::Start);
    assert_eq!(calls[1].1, AgentStartMode::Start);
    assert_ne!(calls[0].0.attempt_id, calls[1].0.attempt_id);
}

#[tokio::test]
async fn terminal_notification_receipts_are_idempotent_and_do_not_acknowledge_a_new_retry_revision()
{
    let directory = tempfile::tempdir().unwrap();
    let engine = engine(directory.path(), Arc::new(Host::default()));
    let run = engine
        .create_run(
            "owner".into(),
            script("throw new Error('retry this execution');"),
            Value::Null,
        )
        .unwrap();
    let failed = timeout(engine.run(&run.id)).await.unwrap();
    assert_eq!(failed.status, WorkflowStatus::Failed);
    engine
        .acknowledge_notification(&run.id, &failed.execution_id)
        .unwrap();
    let acknowledged = engine.get(&run.id).unwrap();
    assert!(acknowledged.notification_delivered);
    assert_eq!(acknowledged.updated_at, failed.updated_at);
    engine
        .acknowledge_notification(&run.id, &failed.execution_id)
        .unwrap();
    assert_eq!(engine.get(&run.id).unwrap(), acknowledged);
    let prepared = engine.prepare_retry(&run.id).unwrap();
    assert_ne!(prepared.execution_id, failed.execution_id);
    assert!(!prepared.notification_delivered);
    engine
        .acknowledge_notification(&run.id, &failed.execution_id)
        .unwrap();
    assert_eq!(engine.get(&run.id).unwrap(), prepared);
    let retried = timeout(engine.run_prepared_retry(&run.id)).await.unwrap();
    engine
        .acknowledge_notification(&run.id, &retried.execution_id)
        .unwrap();
    let mut expected = retried;
    expected.notification_delivered = true;
    assert_eq!(engine.get(&run.id).unwrap(), expected);
}

#[tokio::test]
async fn concurrent_run_namespaces_and_step_creation_ordinals_remain_stable_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::default());
    let engine = engine(directory.path(), host.clone());
    let source = "workflow.meta({name:'ordinals',description:'Stable worker names',phases:[]}); for (const id of ['AI Infrastructure','AI/Infrastructure']) { await workflow.agent(id,{label:id}); } return 'done';";
    let mut workers = Vec::new();
    for _ in 0..8 {
        let engine = engine.clone();
        workers.push(tokio::spawn(async move {
            engine.create_run("owner".into(), source.into(), Value::Null)
        }));
    }
    let mut runs = Vec::new();
    for worker in workers {
        runs.push(worker.await.unwrap().unwrap());
    }
    let mut sequences = runs
        .iter()
        .map(|run| run.worker_sequence)
        .collect::<Vec<_>>();
    sequences.sort();
    assert_eq!(sequences, (1..=8).collect::<Vec<_>>());
    let completed = timeout(engine.run(&runs[0].id)).await.unwrap();
    assert_eq!(completed.status, WorkflowStatus::Succeeded);
    assert_eq!(step(&completed, "AI Infrastructure").worker_sequence, 1);
    assert_eq!(step(&completed, "AI/Infrastructure").worker_sequence, 2);
    let requests = host
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(request, _)| {
            (
                request.run_sequence,
                request.step_sequence,
                request.attempt_number,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        requests,
        vec![
            (completed.worker_sequence, 1, 1),
            (completed.worker_sequence, 2, 1)
        ]
    );
    drop(engine);
    let reopened =
        WorkflowEngine::new(absolute(directory.path()), Arc::new(Provider), host).unwrap();
    assert_eq!(reopened.get(&completed.id).unwrap(), completed);
    let next = reopened
        .create_run("owner".into(), source.into(), Value::Null)
        .unwrap();
    assert_eq!(next.worker_sequence, 9);
}

#[cfg(unix)]
#[test]
fn saved_library_rejects_file_and_managed_ancestor_symlinks_but_accepts_configured_anchor_aliases()
{
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let home = directory.path().join("home");
    let external = directory.path().join("external");
    std::fs::create_dir_all(&external).unwrap();
    let secret = "workflow.meta({name:'escape',description:'External source',phases:[]}); return 'external secret';";
    std::fs::write(external.join("escape.js"), secret).unwrap();
    let library = WorkflowLibrary::new(absolute(&project), absolute(&home));
    let user_saved = library
        .save(
            "example",
            "workflow.meta({name:'example',description:'Saved example',phases:[]}); return 'user';",
            WorkflowScope::User,
        )
        .unwrap();
    let preferred = library
        .save(
            "example",
            "workflow.meta({name:'example',description:'Saved example',phases:[]}); return 'project';",
            WorkflowScope::Project,
        )
        .unwrap();
    let file_link = project.join(".codex/workflows/escape.js");
    symlink(external.join("escape.js"), &file_link).unwrap();
    assert!(
        library
            .read("project:escape")
            .unwrap_err()
            .contains("symlinks")
    );
    assert!(library.resolve("escape").is_err());
    assert!(library.list(/*offset*/ 0, /*limit*/ 50).is_err());
    assert!(library.delete("project:escape").is_err());
    assert!(
        library
            .save_target("escape", WorkflowScope::Project)
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(external.join("escape.js")).unwrap(),
        secret
    );
    std::fs::remove_file(file_link).unwrap();
    assert_eq!(
        library.list(/*offset*/ 0, /*limit*/ 50).unwrap(),
        vec![preferred.clone(), user_saved]
    );

    for relative in [".codex", ".codex/workflows"] {
        let alias_project = directory.path().join(relative.replace('/', "-"));
        let link = alias_project.join(relative);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&external, &link).unwrap();
        let unsafe_library = WorkflowLibrary::new(absolute(&alias_project), absolute(&home));
        assert!(unsafe_library.read("project:escape").is_err());
        assert!(unsafe_library.list(/*offset*/ 0, /*limit*/ 50).is_err());
        assert!(
            unsafe_library
                .save(
                    "new",
                    "workflow.meta({name:'new',description:'Saved new source',phases:[]}); return 'new';",
                    WorkflowScope::Project
                )
                .is_err()
        );
    }
    let alias_home = directory.path().join("unsafe-home");
    std::fs::create_dir_all(&alias_home).unwrap();
    symlink(&external, alias_home.join("workflows")).unwrap();
    let unsafe_user = WorkflowLibrary::new(absolute(&project), absolute(&alias_home));
    assert!(unsafe_user.read("user:escape").is_err());
    assert!(unsafe_user.list(/*offset*/ 0, /*limit*/ 50).is_err());

    let project_anchor = directory.path().join("project-anchor");
    let home_anchor = directory.path().join("home-anchor");
    symlink(&project, &project_anchor).unwrap();
    symlink(&home, &home_anchor).unwrap();
    let aliased_anchors = WorkflowLibrary::new(absolute(&project_anchor), absolute(&home_anchor));
    assert_eq!(aliased_anchors.resolve("example").unwrap(), preferred);
}
