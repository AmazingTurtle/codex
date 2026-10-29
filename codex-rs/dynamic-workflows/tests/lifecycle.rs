//! Durable event delivery uses real JavaScript and the production immutable-record store.

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
use codex_dynamic_workflows::RunLimits;
use codex_dynamic_workflows::WorkflowCompletionMode;
use codex_dynamic_workflows::WorkflowEngine;
use codex_dynamic_workflows::WorkflowHost;
use codex_dynamic_workflows::WorkflowHostEvent;
use codex_dynamic_workflows::WorkflowLifecycleKind;
use codex_dynamic_workflows::WorkflowObserver;
use codex_dynamic_workflows::WorkflowResult;
use codex_dynamic_workflows::WorkflowStatus;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

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
    observer: Mutex<Option<WorkflowObserver>>,
    fail_once: AtomicBool,
    accepted: Mutex<Vec<String>>,
    invalid_once: AtomicBool,
    hold_finish: AtomicBool,
    finishing: tokio::sync::Notify,
    finish_release: tokio::sync::Notify,
    reject_repair: AtomicBool,
    fail_start: AtomicBool,
    fail_finish: AtomicBool,
    hold_outcome: AtomicBool,
    waiting: tokio::sync::Notify,
    telemetry: AtomicBool,
    uncertain_once: AtomicBool,
    hold_recovery: AtomicBool,
    recovery_waiting: tokio::sync::Notify,
    recovery_release: tokio::sync::Notify,
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
        mode: AgentStartMode,
    ) -> Result<String, AgentStartError> {
        if let AgentStartMode::Recover {
            child_thread_id: Some(child),
        } = mode
        {
            return Ok(child);
        }
        if request.attempt_number > 1 && self.reject_repair.load(Ordering::SeqCst) {
            return Err(AgentStartError::NotStarted(
                "corrective worker denied before acceptance".into(),
            ));
        }
        self.accepted
            .lock()
            .map_err(|error| error.to_string())?
            .push(request.attempt_id.clone());
        Ok(request.attempt_id)
    }
    async fn wait_agent(
        &self,
        request: AgentRequest,
        child: String,
    ) -> Result<AgentOutcome, String> {
        if self.uncertain_once.swap(false, Ordering::SeqCst) {
            return Err(
                "authoritative native final observation was temporarily unavailable".into(),
            );
        }
        if self.hold_recovery.load(Ordering::SeqCst) {
            self.recovery_waiting.notify_one();
            self.recovery_release.notified().await;
        }
        if self.hold_outcome.load(Ordering::SeqCst) {
            self.waiting.notify_one();
            std::future::pending::<()>().await;
        }
        let observer = self
            .observer
            .lock()
            .map_err(|error| error.to_string())?
            .clone()
            .ok_or("observer missing")?;
        if self.telemetry.load(Ordering::SeqCst) {
            observer(WorkflowHostEvent::AttemptClock {
                attempt_id: request.attempt_id.clone(),
                active_duration_ms: if request.attempt_number == 1 {
                    120_000
                } else {
                    100_000
                },
                at_ms: 1_000_000,
            })?;
            observer(WorkflowHostEvent::Usage {
                attempt_id: request.attempt_id.clone(),
                thread_id: child.clone(),
                usage: codex_dynamic_workflows::WorkflowTokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cached_input_tokens: 0,
                    total_tokens: 15,
                },
            })?;
            for _ in 0..2 {
                observer(WorkflowHostEvent::ModelTurns {
                    attempt_id: request.attempt_id.clone(),
                    thread_id: child.clone(),
                    model_turns: Some(1),
                })?;
            }
            observer(WorkflowHostEvent::UsageComplete {
                attempt_id: request.attempt_id.clone(),
                thread_id: child.clone(),
            })?;
            let descendant = format!("private-{child}");
            observer(WorkflowHostEvent::Usage {
                attempt_id: request.attempt_id.clone(),
                thread_id: descendant.clone(),
                usage: codex_dynamic_workflows::WorkflowTokenUsage {
                    input_tokens: 2,
                    output_tokens: 1,
                    cached_input_tokens: 0,
                    total_tokens: 3,
                },
            })?;
            observer(WorkflowHostEvent::ModelTurns {
                attempt_id: request.attempt_id.clone(),
                thread_id: descendant.clone(),
                model_turns: Some(2),
            })?;
            observer(WorkflowHostEvent::UsageComplete {
                attempt_id: request.attempt_id.clone(),
                thread_id: descendant,
            })?;
        }
        observer(WorkflowHostEvent::AttemptContext {
            attempt_id: request.attempt_id,
            owned_turn_ids: vec![format!("turn-{child}")],
            final_turn_id: Some(format!("turn-{child}")),
            resolved_model: Some("resolved-model".into()),
            reasoning_effort: Some("high".into()),
        })?;
        Ok(AgentOutcome {
            result: if self.invalid_once.swap(false, Ordering::SeqCst) {
                json!("invalid")
            } else {
                Value::Null
            },
            usage: json!({}),
            error: self
                .fail_once
                .swap(false, Ordering::SeqCst)
                .then(|| "confirmed worker failure".into()),
        })
    }
    async fn stop_run(&self, _run: String) -> Result<(), String> {
        Ok(())
    }
    async fn finish_run(&self, _run: String) -> Result<(), String> {
        self.finishing.notify_one();
        if self.fail_finish.swap(false, Ordering::SeqCst) {
            return Err("final observation failed".into());
        }
        if self.hold_finish.load(Ordering::SeqCst) {
            self.finishing.notify_one();
            self.finish_release.notified().await;
        }
        Ok(())
    }
    async fn start_run(&self, _run: String) -> Result<(), String> {
        if self.fail_start.swap(false, Ordering::SeqCst) {
            return Err("runtime unavailable before replay".into());
        }
        Ok(())
    }
}

fn engine(path: &std::path::Path, host: Arc<Host>) -> Result<WorkflowEngine<Host>, String> {
    WorkflowEngine::new(
        AbsolutePathBuf::from_absolute_path_checked(path).map_err(|error| error.to_string())?,
        Arc::new(Provider),
        host,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn published_terminal_failure_allows_immediate_recovery_after_cleanup_and_keeps_its_own_result()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    host.uncertain_once.store(true, Ordering::SeqCst);
    host.hold_finish.store(true, Ordering::SeqCst);
    host.hold_recovery.store(true, Ordering::SeqCst);
    let engine = engine(directory.path(), Arc::clone(&host))?;
    let run = engine.create_run("owner".into(),
        "workflow.meta({name:'retirement',description:'Observe and recover the exact native attempt',phases:[]}); return await workflow.agent('Finish the owned task');".into(), json!({}))?;
    let mut events = engine.subscribe();
    let first = {
        let engine = engine.clone();
        let id = run.id.clone();
        tokio::spawn(async move { engine.run(&id).await })
    };
    tokio::time::timeout(Duration::from_secs(20), host.finishing.notified())
        .await
        .map_err(|error| error.to_string())?;
    assert!(
        engine.prepare_recovery(&run.id).is_err(),
        "the held native cleanup must retain its admission fence"
    );
    let runtime = tokio::runtime::Handle::current();
    let recovery = {
        let engine = engine.clone();
        let id = run.id.clone();
        tokio::task::spawn_blocking(move || -> Result<_, String> {
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(20), async {
                    loop {
                        match events.recv().await {
                            Ok(event)
                                if event.status == WorkflowStatus::Failed
                                    && event.lifecycle.terminal_ready =>
                            {
                                break Ok::<_, String>(());
                            }
                            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                            Err(error) => return Err(error.to_string()),
                        }
                    }
                })
                .await
                .map_err(|error| error.to_string())?
            })?;
            let prepared = engine.prepare_recovery(&id)?;
            let worker = runtime.spawn(async move { engine.run(&id).await });
            Ok((prepared, worker))
        })
    };
    host.hold_finish.store(false, Ordering::SeqCst);
    host.finish_release.notify_one();
    let (prepared, second) = tokio::time::timeout(Duration::from_secs(20), recovery)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())??;
    tokio::time::timeout(Duration::from_secs(20), host.recovery_waiting.notified())
        .await
        .map_err(|error| error.to_string())?;
    let failed = tokio::time::timeout(Duration::from_secs(20), first)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())??;
    assert_eq!(
        (failed.status, failed.execution_id.as_str()),
        (WorkflowStatus::Failed, run.execution_id.as_str())
    );
    let current = engine.get(&run.id)?;
    assert_eq!(
        (current.status, current.execution_id.as_str()),
        (WorkflowStatus::Running, prepared.execution_id.as_str())
    );
    assert!(
        engine.prepare_recovery(&run.id).is_err(),
        "the old guard cannot remove the replacement's active fence"
    );
    host.recovery_release.notify_one();
    let recovered = tokio::time::timeout(Duration::from_secs(20), second)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())??;
    assert_eq!(
        (recovered.status, recovered.execution_id),
        (WorkflowStatus::Succeeded, prepared.execution_id)
    );
    assert_eq!(
        recovered.accepted_starts, 1,
        "recovery reconnects the original accepted worker"
    );
    assert_eq!(
        host.accepted
            .lock()
            .map_err(|error| error.to_string())?
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn accepted_schema_repairs_publish_one_immutable_total_elapsed_and_inclusive_metrics()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    host.telemetry.store(true, Ordering::SeqCst);
    host.invalid_once.store(true, Ordering::SeqCst);
    let engine = engine(directory.path(), host)?;
    let run = engine.create_run("owner".into(),
        "workflow.meta({name:'metrics',description:'Retain repair telemetry',phases:[{title:'Review'}]}); await workflow.phase('Review'); return await workflow.agent('Return null',{schema:{type:'null'}});".into(), json!({}))?;
    let complete = engine.run(&run.id).await?;
    assert_eq!(complete.status, WorkflowStatus::Succeeded);
    let step = complete
        .steps
        .values()
        .next()
        .ok_or("accepted logical step")?;
    assert_eq!(step.attempts.len(), 2);
    assert_eq!(
        (
            step.metrics.model_turns,
            step.metrics.agents_used,
            step.metrics.active_duration_ms
        ),
        (Some(6), Some(2), Some(220_000))
    );
    assert_eq!(
        step.metrics
            .usage
            .as_ref()
            .map(|usage| (usage.input_tokens, usage.output_tokens)),
        Some((24, 12))
    );
    assert_eq!(complete.metrics.model_turns, Some(6));
    assert_eq!(complete.metrics.agents_used, Some(2));
    assert_eq!(complete.phase_progress[0].metrics.model_turns, Some(6));
    assert_eq!(complete.phase_progress[0].metrics.agents_used, Some(2));
    assert_eq!(complete.phase_progress[0].metrics.active_duration_ms, None);
    assert!(step.completed_at.is_some() && step.completed_at == step.updated_at);
    assert!(step.attempts.iter().all(
        |attempt| attempt.completed_at.is_some() && attempt.completed_at == attempt.updated_at
    ));
    let events = engine.pending_lifecycle_events(&run.id, 0, 50)?;
    let task_events = events
        .iter()
        .filter(|event| matches!(event.kind, WorkflowLifecycleKind::StepCompleted { .. }))
        .collect::<Vec<_>>();
    assert_eq!(
        task_events.len(),
        1,
        "invalid schema attempts do not create premature logical completion facts"
    );
    assert!(matches!(
        task_events[0].kind,
        WorkflowLifecycleKind::StepCompleted {
            active_duration_ms: Some(220_000),
            ..
        }
    ));
    for event in &events {
        engine.acknowledge_lifecycle_event(&run.id, &event.id)?;
    }
    let mut acknowledged = complete.clone();
    acknowledged
        .lifecycle
        .delivered
        .extend(events.iter().map(|event| event.id.clone()));
    assert_eq!(
        engine.get(&run.id)?,
        acknowledged,
        "delivery bookkeeping changes only its explicit receipts"
    );
    let immutable = engine.get_execution_snapshot(&run.id, &run.execution_id)?;
    assert_eq!(immutable.metrics, complete.metrics);
    Ok(())
}

#[tokio::test]
async fn lifecycle_outbox_replays_without_backfill_and_keeps_prior_execution_results_immutable()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    host.fail_once.store(true, Ordering::SeqCst);
    let first = engine(directory.path(), Arc::clone(&host))?;
    let run = first.create_run_with_options("owner".into(), "workflow.meta({name:'lifecycle',description:'Keep authoritative events',phases:[]}); return await workflow.agent('produce null');".into(), json!({"immutable":true}), RunLimits::default(), WorkflowCompletionMode::Continue)?;
    assert_eq!(first.pending_lifecycle_events(&run.id, 0, 50)?, Vec::new());
    let failed = tokio::time::timeout(Duration::from_secs(20), first.run(&run.id))
        .await
        .map_err(|error| error.to_string())??;
    assert_eq!(failed.status, WorkflowStatus::Failed);
    let events = first.pending_lifecycle_events(&run.id, 0, 50)?;
    assert_eq!(events.len(), 3);
    assert!(matches!(
        events[0].kind,
        WorkflowLifecycleKind::Started { .. }
    ));
    assert!(
        matches!(&events[1].kind, WorkflowLifecycleKind::StepCompleted { result: WorkflowResult::Absent, status: WorkflowStatus::Failed, resolved_model: Some(model), reasoning_effort: Some(effort), owned_turn_ids, .. } if model == "resolved-model" && effort == "high" && owned_turn_ids.len() == 1)
    );
    assert!(matches!(
        events[2].kind,
        WorkflowLifecycleKind::Completed {
            result: WorkflowResult::Absent,
            status: WorkflowStatus::Failed,
            ..
        }
    ));
    assert!(events.iter().all(
        |event| event.completion_mode == WorkflowCompletionMode::Continue && event.id.len() <= 64
    ));
    let snapshot = first.get_execution_snapshot(&run.id, &run.execution_id)?;
    drop(first); // Simulate the state-write → owner-rollout gap: none of the events were acknowledged.
    let reopened = engine(directory.path(), Arc::clone(&host))?;
    assert_eq!(reopened.pending_lifecycle_events(&run.id, 0, 50)?, events);
    reopened.acknowledge_lifecycle_event(&run.id, &events[0].id)?;
    reopened.acknowledge_lifecycle_event(&run.id, &events[0].id)?;
    assert_eq!(
        reopened.pending_lifecycle_events(&run.id, 0, 50)?,
        events[1..]
    );
    let completed = tokio::time::timeout(Duration::from_secs(20), reopened.retry(&run.id))
        .await
        .map_err(|error| error.to_string())??;
    assert_eq!(
        (
            completed.status,
            completed.accepted_starts,
            completed.result
        ),
        (WorkflowStatus::Succeeded, 2, Some(Value::Null))
    );
    assert_eq!(
        reopened.get_execution_snapshot(&run.id, &run.execution_id)?,
        snapshot
    );
    let pending = reopened.pending_lifecycle_events(&run.id, 0, 50)?;
    assert_eq!(pending.len(), 5);
    assert_eq!(
        pending
            .iter()
            .map(|event| event.id.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        5
    );
    assert!(matches!(
        pending[4].kind,
        WorkflowLifecycleKind::Completed {
            result: WorkflowResult::Value { value: Value::Null },
            status: WorkflowStatus::Succeeded,
            ..
        }
    ));
    assert_eq!(
        host.accepted
            .lock()
            .map_err(|error| error.to_string())?
            .len(),
        2
    );
    drop(reopened);
    let persisted = codex_dynamic_workflows::read_persisted(
        &AbsolutePathBuf::from_absolute_path_checked(directory.path())
            .map_err(|error| error.to_string())?,
        &run.id,
    )?;
    assert_eq!(persisted.lifecycle.events.len(), 6);
    assert_eq!(persisted.lifecycle.snapshots.len(), 2);
    Ok(())
}

#[tokio::test]
async fn historical_v2_records_without_lifecycle_opt_in_never_generate_old_events()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    let first = engine(directory.path(), Arc::clone(&host))?;
    let run = first.create_run(
        "owner".into(),
        "workflow.meta({name:'old',description:'Pre-outbox V2',phases:[]}); return null;".into(),
        json!({}),
    )?;
    drop(first);
    let path = directory.path().join(format!("{}.json", run.id));
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    manifest["header"]
        .as_object_mut()
        .ok_or("manifest header missing")?
        .remove("lifecycle");
    manifest["header"]
        .as_object_mut()
        .ok_or("manifest header missing")?
        .remove("lifecycleVersion");
    std::fs::write(
        &path,
        serde_json::to_vec(&manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let reopened = engine(directory.path(), host)?;
    let completed = tokio::time::timeout(Duration::from_secs(20), reopened.recover(&run.id))
        .await
        .map_err(|error| error.to_string())??;
    assert_eq!(
        (completed.status, completed.lifecycle.enabled),
        (WorkflowStatus::Succeeded, false)
    );
    assert_eq!(
        reopened.pending_lifecycle_events(&run.id, 0, 50)?,
        Vec::new()
    );
    Ok(())
}

#[tokio::test]
async fn schema_repairs_do_not_publish_early_task_completion_and_run_completion_waits_for_cleanup()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    host.invalid_once.store(true, Ordering::SeqCst);
    host.hold_finish.store(true, Ordering::SeqCst);
    let engine = engine(directory.path(), Arc::clone(&host))?;
    let run = engine.create_run("owner".into(), "workflow.meta({name:'schema',description:'Announce only accepted results',phases:[]}); return await workflow.agent('produce null',{schema:{type:'null'}});".into(), json!({}))?;
    let worker = {
        let engine = engine.clone();
        let id = run.id.clone();
        tokio::spawn(async move { engine.run(&id).await })
    };
    tokio::time::timeout(Duration::from_secs(20), host.finishing.notified())
        .await
        .map_err(|error| error.to_string())?;
    let events = engine.pending_lifecycle_events(&run.id, 0, 50)?;
    assert_eq!(
        events.len(),
        2,
        "the invalid repair attempt and unsettled run must not announce completion"
    );
    assert!(matches!(
        events[1].kind,
        WorkflowLifecycleKind::StepCompleted {
            status: WorkflowStatus::Succeeded,
            result: WorkflowResult::Value { value: Value::Null },
            ..
        }
    ));
    let ready = engine.get(&run.id)?;
    assert_eq!(
        (
            ready.status,
            ready.accepted_starts,
            ready.steps.values().next().map(|step| step.attempts.len())
        ),
        (WorkflowStatus::Running, 2, Some(2))
    );
    assert!(
        engine
            .get_execution_snapshot(&run.id, &run.execution_id)
            .is_err()
    );
    host.hold_finish.store(false, Ordering::SeqCst);
    host.finish_release.notify_one();
    let completed = tokio::time::timeout(Duration::from_secs(20), worker)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())??;
    assert_eq!(completed.status, WorkflowStatus::Succeeded);
    assert_eq!(engine.pending_lifecycle_events(&run.id, 0, 50)?.len(), 3);
    assert_eq!(
        engine
            .get_execution_snapshot(&run.id, &run.execution_id)?
            .result,
        WorkflowResult::Value { value: Value::Null }
    );
    Ok(())
}

#[tokio::test]
async fn crashed_execution_closes_as_partial_interrupted_without_claiming_its_child_finished()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    host.hold_outcome.store(true, Ordering::SeqCst);
    let first = engine(directory.path(), Arc::clone(&host))?;
    let run = first.create_run("owner".into(), "workflow.meta({name:'crash',description:'Keep interrupted execution facts',phases:[]}); return await workflow.agent('unfinished native work');".into(), json!({}))?;
    let worker = {
        let first = first.clone();
        let id = run.id.clone();
        tokio::spawn(async move { first.run(&id).await })
    };
    tokio::time::timeout(Duration::from_secs(20), host.waiting.notified())
        .await
        .map_err(|error| error.to_string())?;
    worker.abort();
    assert!(worker.await.is_err());
    tokio::time::timeout(Duration::from_secs(20), host.finishing.notified())
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(first.get(&run.id)?.status, WorkflowStatus::Interrupted);
    let snapshot = first.get_execution_snapshot(&run.id, &run.execution_id)?;
    assert_eq!(
        (
            snapshot.status,
            snapshot.partial,
            snapshot.metrics.usage_complete
        ),
        (WorkflowStatus::Interrupted, true, false)
    );
    assert_eq!(snapshot.attempts[0].status, WorkflowStatus::Running);
    let events = first.pending_lifecycle_events(&run.id, 0, 50)?;
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[1].kind,
        WorkflowLifecycleKind::Interrupted {
            native_settled: false,
            ..
        }
    ));
    assert!(snapshot.metrics.usage_incomplete_reason.is_some());
    drop(first);
    host.hold_outcome.store(false, Ordering::SeqCst);
    let reopened = engine(directory.path(), Arc::clone(&host))?;
    let completed = tokio::time::timeout(Duration::from_secs(20), reopened.recover(&run.id))
        .await
        .map_err(|error| error.to_string())??;
    assert_eq!(
        (completed.status, completed.accepted_starts),
        (WorkflowStatus::Succeeded, 1)
    );
    assert_eq!(
        reopened.get_execution_snapshot(&run.id, &run.execution_id)?,
        snapshot
    );
    assert_ne!(completed.execution_id, run.execution_id);
    let events = reopened.pending_lifecycle_events(&run.id, 0, 50)?;
    assert_eq!(events.len(), 5);
    assert!(matches!(
        events[2].kind,
        WorkflowLifecycleKind::Started { .. }
    ));
    assert_eq!(
        host.accepted
            .lock()
            .map_err(|error| error.to_string())?
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn a_new_execution_never_inherits_the_previous_executions_published_reply()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    host.fail_finish.store(true, Ordering::SeqCst);
    let engine = engine(directory.path(), Arc::clone(&host))?;
    let run = engine.create_run("owner".into(), "workflow.meta({name:'reply',description:'Replies belong to one execution',phases:[]}); return {old:true};".into(), json!({}))?;
    assert!(
        tokio::time::timeout(Duration::from_secs(20), engine.run(&run.id))
            .await
            .map_err(|error| error.to_string())?
            .is_err()
    );
    engine.stop(&run.id).await?;
    let snapshot = engine.get_execution_snapshot(&run.id, &run.execution_id)?;
    assert_eq!(
        snapshot.result,
        WorkflowResult::Value {
            value: json!({"old":true})
        }
    );
    host.fail_start.store(true, Ordering::SeqCst);
    let failed = tokio::time::timeout(Duration::from_secs(20), engine.retry(&run.id))
        .await
        .map_err(|error| error.to_string())??;
    assert_eq!(
        (failed.status, failed.result),
        (WorkflowStatus::Failed, None)
    );
    assert_eq!(
        engine
            .get_execution_snapshot(&run.id, &failed.execution_id)?
            .result,
        WorkflowResult::Absent
    );
    assert_eq!(
        engine.get_execution_snapshot(&run.id, &run.execution_id)?,
        snapshot
    );
    Ok(())
}

#[tokio::test]
async fn a_rejected_schema_repair_terminalizes_the_last_real_worker_once() -> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    host.invalid_once.store(true, Ordering::SeqCst);
    host.reject_repair.store(true, Ordering::SeqCst);
    let engine = engine(directory.path(), Arc::clone(&host))?;
    let run = engine.create_run("owner".into(), "workflow.meta({name:'repair',description:'Report a failed logical task',phases:[]}); return await workflow.agent('produce null',{schema:{type:'null'}});".into(), json!({}))?;
    let failed = tokio::time::timeout(Duration::from_secs(20), engine.run(&run.id))
        .await
        .map_err(|error| error.to_string())??;
    assert_eq!(
        (failed.status, failed.accepted_starts),
        (WorkflowStatus::Failed, 1)
    );
    let step = failed.steps.values().next().ok_or("logical task missing")?;
    assert_eq!(step.attempts.len(), 2);
    let events = engine.pending_lifecycle_events(&run.id, 0, 50)?;
    assert_eq!(events.len(), 3);
    assert!(
        matches!(&events[1].kind, WorkflowLifecycleKind::StepCompleted { attempt_id, status: WorkflowStatus::Failed, error: Some(error), .. } if attempt_id == &step.attempts[0].id && error.contains("corrective worker denied"))
    );
    assert_eq!(
        host.accepted
            .lock()
            .map_err(|error| error.to_string())?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn explicit_historical_result_display_is_once_and_never_rewrites_the_historical_run()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let id = uuid::Uuid::new_v4().to_string();
    let execution = uuid::Uuid::new_v4().to_string();
    let path = directory.path().join(format!("{id}.json"));
    let bytes = serde_json::to_vec(&json!({"id":id,"executionId":execution,"ownerThreadId":"owner","source":"await workflow.publish(null);","input":{},"status":"succeeded","phase":null,"steps":{},"result":null,"error":null,"createdAt":1,"updatedAt":2})).map_err(|error| error.to_string())?;
    std::fs::write(&path, &bytes).map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    let first = engine(directory.path(), Arc::clone(&host))?;
    assert_eq!(first.pending_lifecycle_events(&id, 0, 50)?, Vec::new());
    let shown = first.show_result_once(&id, &execution)?;
    assert!(matches!(
        shown.kind,
        WorkflowLifecycleKind::ResultShown {
            result: WorkflowResult::Value { value: Value::Null },
            ..
        }
    ));
    assert_eq!(shown.completion_mode, WorkflowCompletionMode::Display);
    assert_eq!(first.show_result_once(&id, &execution)?, shown);
    drop(first);
    let reopened = engine(directory.path(), host)?;
    assert_eq!(
        reopened.pending_lifecycle_events(&id, 0, 50)?,
        vec![shown.clone()]
    );
    reopened.acknowledge_lifecycle_event(&id, &shown.id)?;
    reopened.acknowledge_lifecycle_event(&id, &shown.id)?;
    assert_eq!(reopened.pending_lifecycle_events(&id, 0, 50)?, Vec::new());
    assert_eq!(reopened.show_result_once(&id, &execution)?, shown);
    assert_eq!(
        std::fs::read(&path).map_err(|error| error.to_string())?,
        bytes
    );
    Ok(())
}
