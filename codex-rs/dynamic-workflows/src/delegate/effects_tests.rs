use super::Delegate;
use crate::AgentOutcome;
use crate::AgentRequest;
use crate::AgentStartError;
use crate::AgentStartMode;
use crate::WorkflowHost;
use crate::WorkflowRun;
use crate::WorkflowStatus;
use crate::store::Store;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

struct Host;
impl WorkflowHost for Host {
    async fn schedule_agent(
        &self,
        _request: AgentRequest,
        _mode: AgentStartMode,
    ) -> Result<String, AgentStartError> {
        Err(AgentStartError::NotStarted(
            "log fixture cannot dispatch agents".into(),
        ))
    }
    async fn wait_agent(
        &self,
        _request: AgentRequest,
        _child: String,
    ) -> Result<AgentOutcome, String> {
        Err("log fixture has no native outcome".into())
    }
    async fn stop_run(&self, _run: String) -> Result<(), String> {
        Ok(())
    }
}

fn fixture(path: &std::path::Path) -> Result<(Arc<Store>, WorkflowRun), String> {
    let store = Arc::new(Store::new(
        AbsolutePathBuf::from_absolute_path_checked(path).map_err(|error| error.to_string())?,
    )?);
    let source = "workflow.meta({name:'logs',description:'Keep a log and its receipt atomic',phases:[]}); await workflow.log('durable message'); return null;";
    let mut run:WorkflowRun=serde_json::from_value(json!({"id":uuid::Uuid::new_v4().to_string(),"executionId":uuid::Uuid::new_v4().to_string(),"formatVersion":2,"ownerThreadId":"owner","source":source,"input":{},"status":"pending","phase":null,"steps":{},"error":null,"createdAt":1,"updatedAt":1})).map_err(|error|error.to_string())?;
    run.source_hash = crate::orchestration::source_hash(source);
    run.metadata = Some(crate::compile_script(source)?.metadata);
    run.steering = crate::steering::WorkflowSteeringState::initialize(&run)?;
    store.insert(&mut run)?;
    store.update(&run.id, |run| {
        run.status = WorkflowStatus::Running;
        run.steering.suffix_open = true;
        Ok(())
    })?;
    Ok((store, run))
}

fn delegate(store: Arc<Store>, run: &WorkflowRun) -> Arc<Delegate<Host>> {
    Arc::new(Delegate {
        store,
        host: Arc::new(Host),
        run_id: run.id.clone(),
        seen: Arc::new(Mutex::new(HashSet::new())),
        seen_nested: Arc::new(Mutex::new(HashSet::new())),
        pending: Arc::new(AtomicUsize::new(0)),
        retry_recovered_failure: false,
        seen_effects: Arc::new(Mutex::new(HashSet::new())),
        checkpoint_index: Arc::new(Mutex::new(0)),
        execution_id: run.execution_id.clone(),
        revision_id: run.steering.state.active_revision_id.clone(),
        supervision: Arc::default(),
        cancellation: Default::default(),
    })
}

fn input() -> Value {
    json!({"operation":"log","effectId":"root/effect0","scope":"root","statementIndex":0,"message":"durable message","phase":null})
}

#[test]
fn a_log_receipt_and_its_entry_are_already_durable_before_replay_credit_is_published()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let (store, run) = fixture(directory.path())?;
    let delegate = delegate(store.clone(), &run);
    let mut events = store.events.subscribe();
    // Hold only the post-commit replay-credit lock, creating the original crash window.
    let credit = delegate.seen_effects.lock().unwrap();
    let captured = delegate.clone();
    let worker = std::thread::spawn(move || captured.log(&input()));
    let committed = events.blocking_recv().unwrap();
    assert_eq!(
        committed
            .logs
            .iter()
            .map(|log| log.message.as_str())
            .collect::<Vec<_>>(),
        vec!["durable message"]
    );
    assert!(committed.steering.effects.contains_key("root/effect0"));
    let manifest = directory.path().join(format!("{}.json", run.id));
    let backup = directory.path().join("committed.backup");
    std::fs::rename(&manifest, &backup).map_err(|error| error.to_string())?;
    std::fs::create_dir(&manifest).map_err(|error| error.to_string())?;
    drop(credit);
    // No second state commit is needed after the receipt becomes visible.
    worker.join().unwrap()?;
    std::fs::remove_dir(&manifest).map_err(|error| error.to_string())?;
    std::fs::rename(&backup, &manifest).map_err(|error| error.to_string())?;
    drop(delegate);
    drop(store);
    let reopened = Store::new(
        AbsolutePathBuf::from_absolute_path_checked(directory.path())
            .map_err(|error| error.to_string())?,
    )?;
    let durable = reopened.get(&run.id)?;
    assert_eq!(
        (&durable.logs, &durable.steering.effects),
        (&committed.logs, &committed.steering.effects)
    );
    Ok(())
}

#[test]
fn a_preexisting_receipt_without_its_log_cannot_authorize_cached_prefix_replay()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let (store, run) = fixture(directory.path())?;
    delegate(store.clone(), &run).log(&input())?;
    drop(store);
    // This is the reachable durable shape produced by the former two-commit implementation.
    let path = directory.path().join(format!("{}.json", run.id));
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    manifest["header"]["logs"] = json!([]);
    std::fs::write(
        &path,
        serde_json::to_vec(&manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let store = Arc::new(Store::new(
        AbsolutePathBuf::from_absolute_path_checked(directory.path())
            .map_err(|error| error.to_string())?,
    )?);
    store.update(&run.id, |run| {
        run.status = WorkflowStatus::Running;
        run.steering.suffix_open = false;
        run.steering.replay_frontier = 1;
        Ok(())
    })?;
    let delegate = delegate(store.clone(), &run);
    assert!(
        delegate
            .log(&input())
            .unwrap_err()
            .contains("log is missing")
    );
    assert!(delegate.seen_effects.lock().unwrap().is_empty());
    delegate
        .checkpoint(&json!({"statementIndex":0,"scope":"root","complete":false,"pendingWork":0}))?;
    assert!(
        delegate
            .checkpoint(
                &json!({"statementIndex":1,"scope":"root","complete":false,"pendingWork":0})
            )
            .unwrap_err()
            .contains("omitted")
    );
    assert!(!store.get(&run.id)?.steering.suffix_open);
    Ok(())
}
