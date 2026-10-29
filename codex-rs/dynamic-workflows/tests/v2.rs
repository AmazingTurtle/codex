//! Real V8, bootstrap, replay and store; only external agent inference is simulated.
use codex_code_mode_protocol::CodeModeSessionCellExecutionLimits;
use codex_code_mode_protocol::CodeModeSessionProvider;
use codex_code_mode_protocol::CodeModeSessionProviderFuture;
use codex_code_mode_runtime::InProcessCodeModeSession;
use codex_dynamic_workflows::AgentOutcome;
use codex_dynamic_workflows::AgentRequest;
use codex_dynamic_workflows::AgentStartError;
use codex_dynamic_workflows::AgentStartMode;
use codex_dynamic_workflows::ResolvedWorkflow;
use codex_dynamic_workflows::RunLimits;
use codex_dynamic_workflows::WorkflowAdmissionGuard;
use codex_dynamic_workflows::WorkflowEngine;
use codex_dynamic_workflows::WorkflowHost;
use codex_dynamic_workflows::WorkflowHostEvent;
use codex_dynamic_workflows::WorkflowObserver;
use codex_dynamic_workflows::WorkflowStatus;
use codex_dynamic_workflows::WorkflowTokenUsage;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn appended_keyed_tasks_reuse_the_proven_prefix_and_all_revisions_share_the_original_budget()
-> Result<(), String> {
    use codex_dynamic_workflows::RevisionApplyMode;
    use codex_dynamic_workflows::RevisionApplyRequest;
    use codex_dynamic_workflows::RevisionResumeRequest;
    use codex_dynamic_workflows::RevisionStageRequest;
    use codex_dynamic_workflows::WorkflowResult;
    use codex_dynamic_workflows::WorkflowRevisionStatus;
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    let engine = engine(directory.path(), host.clone())?;
    let prefix = "const base = await workflow.agent('base',{key:'base',label:'base'});";
    let source = script(&format!("{prefix} return {{base}};"));
    let run = engine.create_run_with_limits(
        "owner".into(),
        source.clone(),
        json!({"kept":true}),
        RunLimits {
            max_agents: 2,
            max_tokens: Some(100),
        },
    )?;
    let original = engine.run(&run.id).await?;
    assert_eq!(
        (original.status, original.accepted_starts),
        (WorkflowStatus::Succeeded, 1)
    );
    let base = original
        .steps
        .values()
        .next()
        .ok_or("base task missing")?
        .clone();
    let revised = script(&format!(
        "{prefix} const extra = await workflow.agent('extra',{{key:'extra',label:'extra'}}); return {{base,extra}};"
    ));
    let staged = engine.stage_revision(
        &run.id,
        RevisionStageRequest {
            command_id: "stage-append".into(),
            expected_control_revision: 0,
            source: revised.clone(),
            input: WorkflowResult::Absent,
        },
    )?;
    assert_eq!(staged.revision.reusable_statements, 1);
    let apply = RevisionApplyRequest {
        command_id: "apply-append".into(),
        expected_control_revision: staged.state.control_revision,
        revision_id: staged.revision.id.clone(),
        mode: RevisionApplyMode::DrainAccepted,
    };
    engine.apply_revision(&run.id, apply.clone()).await?;
    let ready = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        engine.wait_revision_settlement(&run.id, &staged.revision.id),
    )
    .await
    .map_err(|error| error.to_string())??;
    assert_eq!(ready.revision.status, WorkflowRevisionStatus::Ready);
    assert_eq!(engine.apply_revision(&run.id, apply).await?, ready);
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    engine.prepare_revision_resume(
        &run.id,
        RevisionResumeRequest {
            command_id: "resume-append".into(),
            expected_control_revision: ready.state.control_revision,
            revision_id: staged.revision.id,
        },
    )?;
    let completed = engine.run_prepared_revision(&run.id).await?;
    assert_eq!(
        (
            completed.status,
            completed.accepted_starts,
            completed
                .metrics
                .usage
                .as_ref()
                .map(|usage| usage.total_tokens)
        ),
        (WorkflowStatus::Succeeded, 2, Some(6))
    );
    assert_eq!(
        completed.steps.values().find(|step| step.label == "base"),
        Some(&base)
    );
    assert_eq!(
        engine
            .get_execution_snapshot(&run.id, &original.execution_id)?
            .result,
        WorkflowResult::Value {
            value: json!({"base":{"ok":true,"label":"base"}})
        }
    );
    assert_eq!(
        engine
            .get_execution_snapshot(&run.id, &original.execution_id)?
            .source,
        source
    );
    let state = engine.control_state(&run.id)?;
    let third_source=revised.replace("return {base,extra};","const third = await workflow.agent('third',{key:'third',label:'third'}); return {base,extra,third};");
    let third = engine.stage_revision(
        &run.id,
        RevisionStageRequest {
            command_id: "stage-third".into(),
            expected_control_revision: state.control_revision,
            source: third_source,
            input: WorkflowResult::Absent,
        },
    )?;
    engine
        .apply_revision(
            &run.id,
            RevisionApplyRequest {
                command_id: "apply-third".into(),
                expected_control_revision: third.state.control_revision,
                revision_id: third.revision.id.clone(),
                mode: RevisionApplyMode::DrainAccepted,
            },
        )
        .await?;
    let ready = engine
        .wait_revision_settlement(&run.id, &third.revision.id)
        .await?;
    engine.prepare_revision_resume(
        &run.id,
        RevisionResumeRequest {
            command_id: "resume-third".into(),
            expected_control_revision: ready.state.control_revision,
            revision_id: third.revision.id,
        },
    )?;
    let exhausted = engine.run_prepared_revision(&run.id).await?;
    assert_eq!(
        (
            exhausted.status,
            exhausted.accepted_starts,
            host.calls.lock().unwrap().len()
        ),
        (WorkflowStatus::Failed, 2, 2)
    );
    assert!(
        exhausted
            .error
            .as_ref()
            .is_some_and(|error| error.contains("agent-start limit exhausted"))
    );
    let changed = engine.stage_revision(
        &run.id,
        RevisionStageRequest {
            command_id: "changed-args".into(),
            expected_control_revision: engine.control_state(&run.id)?.control_revision,
            source: revised,
            input: WorkflowResult::Value {
                value: json!({"kept":false}),
            },
        },
    )?;
    assert_eq!(changed.revision.reusable_statements, 0);
    assert_eq!(engine.get(&run.id)?.accepted_starts, 2);
    Ok(())
}

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
    calls: Mutex<Vec<(AgentRequest, AgentStartMode)>>,
    observers: Mutex<HashMap<String, WorkflowObserver>>,
    admissions: Mutex<HashMap<String, WorkflowAdmissionGuard>>,
    invalid_rounds: AtomicUsize,
    nested: Option<String>,
    loads: AtomicUsize,
    hold_stop: AtomicBool,
    stopping: tokio::sync::Notify,
    stop_release: tokio::sync::Notify,
    stop_calls: AtomicUsize,
    stop_failures: AtomicUsize,
}

impl WorkflowHost for Host {
    async fn fence_run(
        &self,
        run: String,
        _revision: String,
    ) -> Result<codex_dynamic_workflows::WorkflowFenceReceipt, String> {
        Ok(codex_dynamic_workflows::WorkflowFenceReceipt {
            accepted_attempt_ids: self
                .calls
                .lock()
                .map_err(|error| error.to_string())?
                .iter()
                .filter(|(request, _)| request.run_id == run)
                .map(|(request, _)| request.attempt_id.clone())
                .collect(),
            uncertain_attempt_ids: Vec::new(),
        })
    }
    async fn register_admission_guard(
        &self,
        run: String,
        guard: WorkflowAdmissionGuard,
    ) -> Result<(), String> {
        self.admissions
            .lock()
            .map_err(|error| error.to_string())?
            .insert(run, guard);
        Ok(())
    }
    async fn register_observer(
        &self,
        run: String,
        observer: WorkflowObserver,
    ) -> Result<(), String> {
        self.observers
            .lock()
            .map_err(|error| error.to_string())?
            .insert(run, observer);
        Ok(())
    }
    async fn schedule_agent(
        &self,
        request: AgentRequest,
        mode: AgentStartMode,
    ) -> Result<String, AgentStartError> {
        if matches!(mode, AgentStartMode::Start) {
            let guard = self
                .admissions
                .lock()
                .map_err(|error| AgentStartError::Uncertain(error.to_string()))?
                .get(&request.run_id)
                .cloned()
                .ok_or("admission guard missing")?;
            guard(&request.attempt_id).map_err(AgentStartError::NotStarted)?;
        }
        let child = request.attempt_id.clone();
        let observer = self
            .observers
            .lock()
            .map_err(|error| AgentStartError::Uncertain(error.to_string()))?
            .get(&request.run_id)
            .cloned()
            .ok_or("observer missing")?;
        observer(WorkflowHostEvent::StartAccepted {
            attempt_id: request.attempt_id.clone(),
            child_thread_id: child.clone(),
        })
        .map_err(AgentStartError::Uncertain)?;
        self.calls
            .lock()
            .map_err(|error| AgentStartError::Uncertain(error.to_string()))?
            .push((request, mode));
        Ok(child)
    }
    async fn wait_agent(
        &self,
        request: AgentRequest,
        child: String,
    ) -> Result<AgentOutcome, String> {
        let observer = self
            .observers
            .lock()
            .map_err(|error| error.to_string())?
            .get(&request.run_id)
            .cloned()
            .ok_or("observer missing")?;
        observer(WorkflowHostEvent::Usage {
            attempt_id: request.attempt_id.clone(),
            thread_id: child.clone(),
            usage: WorkflowTokenUsage {
                input_tokens: 1,
                output_tokens: 2,
                cached_input_tokens: 0,
                total_tokens: 3,
            },
        })?;
        observer(WorkflowHostEvent::UsageComplete {
            attempt_id: request.attempt_id,
            thread_id: child,
        })?;
        let result =
            if usize::from(request.attempt_number) <= self.invalid_rounds.load(Ordering::SeqCst) {
                json!("invalid schema result")
            } else {
                json!({"ok":true,"label":request.label})
            };
        Ok(AgentOutcome {
            result,
            usage: json!({"input_tokens":1,"output_tokens":2,"total_tokens":3}),
            error: (request.label == "bad").then(|| "confirmed agent failure".to_string()),
        })
    }
    async fn load_workflow(&self, _run: String, name: String) -> Result<ResolvedWorkflow, String> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        Ok(ResolvedWorkflow {
            id: format!("project:{name}"),
            source: self.nested.clone().ok_or("no nested fixture")?,
        })
    }
    async fn stop_run(&self, _run: String) -> Result<(), String> {
        self.stop_calls.fetch_add(1, Ordering::SeqCst);
        if self
            .stop_failures
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err("fixture stop did not settle".into());
        }
        if self.hold_stop.load(Ordering::SeqCst) {
            self.stopping.notify_one();
            self.stop_release.notified().await;
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
fn script(body: &str) -> String {
    format!(
        "workflow.meta({{name:'matrix',description:'Runtime integration',phases:[{{title:'Plan'}},{{title:'Unused'}}]}});\n{body}"
    )
}
async fn execute(
    engine: &WorkflowEngine<Host>,
    body: &str,
    limits: RunLimits,
) -> Result<codex_dynamic_workflows::WorkflowRun, String> {
    let run = engine.create_run_with_limits("owner".into(), script(body), json!({}), limits)?;
    tokio::time::timeout(std::time::Duration::from_secs(20), engine.run(&run.id))
        .await
        .map_err(|error| error.to_string())?
}

#[tokio::test]
async fn parallel_failures_are_null_but_unsupported_options_still_fail_the_run()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    let engine = engine(directory.path(), host.clone())?;
    let completed=execute(&engine,"workflow.phase('Plan'); return await workflow.parallel([()=>workflow.agent('good',{label:'good'}),()=>workflow.agent('bad',{label:'bad'})]);",RunLimits::default()).await?;
    assert_eq!(
        (
            completed.status,
            completed.result,
            completed.accepted_starts
        ),
        (
            WorkflowStatus::Succeeded,
            Some(json!([{"ok":true,"label":"good"},null])),
            2
        )
    );
    assert_eq!(
        completed
            .metrics
            .usage
            .as_ref()
            .map(|usage| (usage.input_tokens, usage.output_tokens)),
        Some((2, 4))
    );
    let failed=execute(&engine,"try {await workflow.parallel([()=>workflow.agent('bad option',{unknown:true})]);} catch {} return 'caught';",RunLimits::default()).await?;
    assert_eq!(
        (failed.status, failed.steps.len(), failed.accepted_starts),
        (WorkflowStatus::Failed, 0, 0)
    );
    assert!(
        failed
            .error
            .is_some_and(|error| error.contains("unsupported workflow agent option"))
    );
    assert_eq!(host.calls.lock().unwrap().len(), 2);
    let fallback = execute(
        &engine,
        "return await workflow.agent('\\n  Inspect source\\n');",
        RunLimits::default(),
    )
    .await?;
    assert_eq!(
        (fallback.status, fallback.result),
        (
            WorkflowStatus::Succeeded,
            Some(json!({"ok":true,"label":"Inspect source"}))
        )
    );
    assert_eq!(
        host.calls.lock().unwrap().last().unwrap().0.options["message"],
        json!("\n  Inspect source\n")
    );
    Ok(())
}

#[tokio::test]
async fn total_start_limit_is_shared_by_parallel_reservations() -> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    let engine = engine(directory.path(), host.clone())?;
    let failed=execute(&engine,"return await workflow.parallel([0,1,2].map(index=>()=>workflow.agent(String(index),{label:String(index)})));",RunLimits {max_agents:2,max_tokens:None}).await?;
    assert_eq!(
        (
            failed.status,
            failed.accepted_starts,
            host.calls.lock().unwrap().len()
        ),
        (WorkflowStatus::Failed, 2, 2)
    );
    assert!(
        failed
            .error
            .is_some_and(|error| error.contains("agent-start limit"))
    );
    Ok(())
}

#[tokio::test]
async fn schema_repairs_charge_real_starts_and_stop_after_five_or_the_shared_limit()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host {
        invalid_rounds: AtomicUsize::new(2),
        ..Default::default()
    });
    let engine = engine(directory.path(), host.clone())?;
    let body = "return await workflow.agent('structured',{label:'structured',schema:{type:'object',required:['ok'],properties:{ok:{type:'boolean'},label:{type:'string'}},additionalProperties:false}});";
    let completed = execute(&engine, body, RunLimits::default()).await?;
    let step = completed.steps.values().next().unwrap();
    assert_eq!(
        (
            completed.status,
            completed.accepted_starts,
            step.attempts.len()
        ),
        (WorkflowStatus::Succeeded, 3, 3)
    );
    assert!(
        step.attempts[..2]
            .iter()
            .all(|attempt| attempt.schema_error.is_some())
    );
    assert_eq!(
        completed
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(9)
    );
    assert!(
        host.calls.lock().unwrap()[1]
            .0
            .validation_feedback
            .is_some()
    );
    host.invalid_rounds.store(usize::MAX, Ordering::SeqCst);
    let exhausted = execute(&engine, body, RunLimits::default()).await?;
    assert_eq!(
        (exhausted.status, exhausted.accepted_starts),
        (WorkflowStatus::Failed, 5)
    );
    assert!(
        exhausted
            .error
            .as_ref()
            .is_some_and(|error| error.contains("five attempts"))
    );
    let recovered = engine.recover(&exhausted.id).await?;
    assert_eq!(
        (recovered.status, recovered.accepted_starts),
        (WorkflowStatus::Failed, 5)
    );
    let retried = engine.retry(&exhausted.id).await?;
    assert_eq!(
        (retried.status, retried.accepted_starts),
        (WorkflowStatus::Failed, 10)
    );
    let repair = host
        .calls
        .lock()
        .unwrap()
        .iter()
        .find(|(request, _)| request.run_id == exhausted.id && request.attempt_number == 6)
        .unwrap()
        .0
        .clone();
    assert!(
        repair
            .validation_feedback
            .as_ref()
            .is_some_and(|feedback| feedback.len() <= 2048)
    );
    let bounded = execute(
        &engine,
        body,
        RunLimits {
            max_agents: 2,
            max_tokens: None,
        },
    )
    .await?;
    assert_eq!(
        (bounded.status, bounded.accepted_starts),
        (WorkflowStatus::Failed, 2)
    );
    assert!(
        bounded
            .error
            .is_some_and(|error| error.contains("agent-start limit"))
    );
    Ok(())
}

#[tokio::test]
async fn schemas_reject_external_resources_and_provable_contradictions_before_dispatch()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    let engine = engine(directory.path(), host.clone())?;
    for schema in [
        json!(false),
        json!({"$ref":"https://example.invalid/schema"}),
        json!({"type":"object","required":["missing"],"properties":{},"additionalProperties":false}),
    ] {
        let body = format!("return await workflow.agent('invalid',{{schema:{schema}}});");
        let failed = execute(&engine, &body, RunLimits::default()).await?;
        assert_eq!(
            (failed.status, failed.accepted_starts, failed.steps.len()),
            (WorkflowStatus::Failed, 0, 0)
        );
    }
    assert_eq!(host.calls.lock().unwrap().len(), 0);
    Ok(())
}

#[tokio::test]
async fn nested_sources_are_snapshots_and_share_the_parent_start_budget() -> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host=Arc::new(Host {nested:Some("workflow.meta({name:'nested',description:'Nested fixture',phases:[{title:'Child'}]}); workflow.phase('Child'); await workflow.agent('nested first',{label:'nested-first'}); return await workflow.agent('nested second',{label:'nested-second'});".into()),..Default::default()});
    let engine = engine(directory.path(), host.clone())?;
    let failed = execute(
        &engine,
        "await workflow.agent('outer',{label:'outer'}); return await workflow('nested',{});",
        RunLimits {
            max_agents: 2,
            max_tokens: None,
        },
    )
    .await?;
    assert_eq!(
        (
            failed.status,
            failed.accepted_starts,
            failed.nested_workflows.len()
        ),
        (WorkflowStatus::Failed, 2, 1)
    );
    assert!(
        failed
            .phase_progress
            .iter()
            .any(|phase| phase.title == "nested / Child")
    );
    let retry = engine.retry(&failed.id).await?;
    assert_eq!(
        (
            retry.status,
            retry.accepted_starts,
            host.calls.lock().unwrap().len(),
            host.loads.load(Ordering::SeqCst)
        ),
        (WorkflowStatus::Failed, 2, 2, 1)
    );
    Ok(())
}

#[tokio::test]
async fn legacy_records_remain_read_only_without_starting_children() -> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let id = uuid::Uuid::new_v4().to_string();
    let value = json!({"id":id,"ownerThreadId":"owner","source":"await workflow.publish('legacy');","input":{},"status":"running","phase":null,"steps":{},"result":"old","error":null,"createdAt":1,"updatedAt":2});
    std::fs::write(
        directory.path().join(format!("{id}.json")),
        serde_json::to_vec(&value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    let engine = engine(directory.path(), host.clone())?;
    let snapshot = engine.get(&id)?;
    assert_eq!(
        (snapshot.format_version, snapshot.status),
        (1, WorkflowStatus::Running)
    );
    assert!(engine.run(&id).await.is_err());
    assert!(engine.retry(&id).await.is_err());
    assert!(engine.recover(&id).await.is_err());
    assert!(engine.stop(&id).await.is_err());
    assert_eq!(engine.get(&id)?, snapshot);
    assert_eq!(host.calls.lock().unwrap().len(), 0);
    Ok(())
}

#[tokio::test]
async fn recovery_cannot_reserve_a_new_execution_until_stop_teardown_finishes() -> Result<(), String>
{
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    let engine = engine(directory.path(), host.clone())?;
    let mut events = engine.subscribe();
    let run = engine.create_run(
        "owner".into(),
        script("workflow.phase('Plan'); await new Promise(()=>{}); return null;"),
        json!({}),
    )?;
    let worker = {
        let engine = engine.clone();
        let id = run.id.clone();
        tokio::spawn(async move { engine.run(&id).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if events
                .recv()
                .await
                .map_err(|error| error.to_string())?
                .phase
                .as_deref()
                == Some("Plan")
            {
                break Ok::<_, String>(());
            }
        }
    })
    .await
    .map_err(|error| error.to_string())??;
    host.hold_stop.store(true, Ordering::SeqCst);
    let stop = {
        let engine = engine.clone();
        let id = run.id.clone();
        tokio::spawn(async move { engine.stop(&id).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(20), host.stopping.notified())
        .await
        .map_err(|error| error.to_string())?;
    assert!(engine.prepare_recovery(&run.id).is_err());
    assert_eq!(engine.get(&run.id)?.status, WorkflowStatus::Stopped);
    host.hold_stop.store(false, Ordering::SeqCst);
    host.stop_release.notify_waiters();
    let stopped = tokio::time::timeout(std::time::Duration::from_secs(20), stop)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())??;
    tokio::time::timeout(std::time::Duration::from_secs(20), worker)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())??;
    let recovered = engine.prepare_recovery(&run.id)?;
    assert_eq!(
        (stopped.status, recovered.status),
        (WorkflowStatus::Stopped, WorkflowStatus::Pending)
    );
    assert_ne!(stopped.execution_id, recovered.execution_id);
    engine.stop(&run.id).await?;
    Ok(())
}

#[tokio::test]
async fn failed_cleanup_keeps_recovery_fenced_until_an_explicit_stop_retry_settles()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    let engine = engine(directory.path(), host.clone())?;
    let mut events = engine.subscribe();
    let run = engine.create_run(
        "owner".into(),
        script("workflow.phase('Plan'); await new Promise(()=>{}); return null;"),
        json!({}),
    )?;
    let worker = {
        let engine = engine.clone();
        let id = run.id.clone();
        tokio::spawn(async move { engine.run(&id).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if events
                .recv()
                .await
                .map_err(|error| error.to_string())?
                .phase
                .as_deref()
                == Some("Plan")
            {
                break Ok::<_, String>(());
            }
        }
    })
    .await
    .map_err(|error| error.to_string())??;
    host.stop_failures.store(1, Ordering::SeqCst);
    let error = engine.stop(&run.id).await.unwrap_err();
    assert!(error.contains("fixture stop did not settle"));
    let result = tokio::time::timeout(std::time::Duration::from_secs(20), worker)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    assert!(result.is_err());
    assert_eq!(
        host.stop_calls.load(Ordering::SeqCst),
        1,
        "the failed cleanup must not be silently retried by another observer"
    );
    assert!(engine.prepare_recovery(&run.id).is_err());
    assert!(engine.prepare_retry(&run.id).is_err());
    let stopped = engine.stop(&run.id).await?;
    assert_eq!(
        (
            stopped.status,
            stopped.error,
            host.stop_calls.load(Ordering::SeqCst)
        ),
        (WorkflowStatus::Stopped, None, 2)
    );
    let prepared = engine.prepare_recovery(&run.id)?;
    assert_eq!(prepared.status, WorkflowStatus::Pending);
    engine.stop(&run.id).await?;
    Ok(())
}
