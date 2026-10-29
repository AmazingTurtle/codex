//! Real V8 and engine supervision; the native adapter holds its canonical abort receipt.
use super::Provider;
use codex_dynamic_workflows::AgentOutcome;
use codex_dynamic_workflows::AgentRequest;
use codex_dynamic_workflows::AgentStartError;
use codex_dynamic_workflows::AgentStartMode;
use codex_dynamic_workflows::RevisionApplyMode;
use codex_dynamic_workflows::RevisionApplyRequest;
use codex_dynamic_workflows::RevisionStageRequest;
use codex_dynamic_workflows::WorkflowControlOperation;
use codex_dynamic_workflows::WorkflowControlRequest;
use codex_dynamic_workflows::WorkflowControlStatus;
use codex_dynamic_workflows::WorkflowControlTarget;
use codex_dynamic_workflows::WorkflowEngine;
use codex_dynamic_workflows::WorkflowFenceReceipt;
use codex_dynamic_workflows::WorkflowHost;
use codex_dynamic_workflows::WorkflowHostEvent;
use codex_dynamic_workflows::WorkflowInterruptReceipt;
use codex_dynamic_workflows::WorkflowObserver;
use codex_dynamic_workflows::WorkflowResult;
use codex_dynamic_workflows::WorkflowRevisionStatus;
use codex_dynamic_workflows::WorkflowStatus;
use codex_dynamic_workflows::WorkflowStepDisposition;
use codex_dynamic_workflows::WorkflowTokenUsage;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Default)]
struct Host {
    observer: Mutex<Option<WorkflowObserver>>,
    request: Mutex<Option<AgentRequest>>,
    accepted: Notify,
    abort_entered: Notify,
    release_receipt: Notify,
    native_stopped: AtomicBool,
    native_calls: AtomicUsize,
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
        let observer = self
            .observer
            .lock()
            .map_err(|error| AgentStartError::Uncertain(error.to_string()))?
            .clone()
            .ok_or_else(|| AgentStartError::Uncertain("observer not registered".into()))?;
        let child = format!("child-{}", request.attempt_id);
        observer(WorkflowHostEvent::StartUncertain {
            attempt_id: request.attempt_id.clone(),
            revision_id: request.revision_id.clone(),
        })?;
        observer(WorkflowHostEvent::StartAccepted {
            attempt_id: request.attempt_id.clone(),
            child_thread_id: child.clone(),
        })?;
        observer(WorkflowHostEvent::AttemptContext {
            attempt_id: request.attempt_id.clone(),
            owned_turn_ids: vec![format!("turn-{}", request.attempt_id)],
            final_turn_id: None,
            resolved_model: Some("controlled-native-model".into()),
            reasoning_effort: None,
        })?;
        observer(WorkflowHostEvent::Usage {
            attempt_id: request.attempt_id.clone(),
            thread_id: child.clone(),
            usage: WorkflowTokenUsage {
                input_tokens: 2,
                output_tokens: 1,
                cached_input_tokens: 0,
                total_tokens: 3,
            },
        })?;
        *self
            .request
            .lock()
            .map_err(|error| AgentStartError::Uncertain(error.to_string()))? = Some(request);
        self.native_calls.fetch_add(1, Ordering::SeqCst);
        self.accepted.notify_one();
        Ok(child)
    }

    async fn wait_agent(
        &self,
        _request: AgentRequest,
        _child: String,
    ) -> Result<AgentOutcome, String> {
        // A resumable native Interrupted state is not a successful/final outcome.
        // Production engine supervision must consume the durable control settlement.
        std::future::pending().await
    }

    async fn interrupt_attempt(
        &self,
        request: AgentRequest,
        child: String,
        turn: String,
    ) -> Result<WorkflowInterruptReceipt, String> {
        let accepted = self
            .request
            .lock()
            .map_err(|error| error.to_string())?
            .clone()
            .ok_or("native request missing")?;
        if request != accepted
            || child != format!("child-{}", accepted.attempt_id)
            || turn != format!("turn-{}", accepted.attempt_id)
        {
            return Err("the adapter must receive the exact accepted native identity".into());
        }
        self.native_stopped.store(true, Ordering::SeqCst);
        self.observer
            .lock()
            .map_err(|error| error.to_string())?
            .clone()
            .ok_or("observer missing")?(WorkflowHostEvent::UsageIncomplete {
            reason: "the interrupted sample has only its proven partial usage".into(),
        })?;
        self.abort_entered.notify_one();
        self.release_receipt.notified().await;
        Ok(WorkflowInterruptReceipt {
            child_thread_id: child,
            turn_id: turn,
            settled: true,
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
            .ok_or("accepted native request missing")?;
        Ok(WorkflowFenceReceipt {
            accepted_attempt_ids: vec![request.attempt_id],
            uncertain_attempt_ids: Vec::new(),
        })
    }

    async fn stop_run(&self, _run: String) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test]
async fn apply_during_an_accepted_skip_receipt_keeps_native_settlement_and_drains_without_applying_the_replacement()
-> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let host = Arc::new(Host::default());
    let root = AbsolutePathBuf::from_absolute_path_checked(directory.path())
        .map_err(|error| error.to_string())?;
    let engine = WorkflowEngine::new(root, Arc::new(Provider), Arc::clone(&host))?;
    let source = "workflow.meta({name:'skip-apply-race',description:'Keep exact native settlement across a plan fence',phases:[]});\nconst base = await workflow.agent('Hold the accepted native turn.',{key:'base',label:'base'});\nreturn base;";
    let run = engine.create_run("owner".into(), source.into(), json!({}))?;
    let running = {
        let engine = engine.clone();
        let id = run.id.clone();
        tokio::spawn(async move { engine.run(&id).await })
    };
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), host.accepted.notified())
        .await
        .map_err(|error| error.to_string())?;
    let active = engine.get(&run.id)?;
    let step = active
        .steps
        .values()
        .next()
        .ok_or("accepted step missing")?;
    let attempt = step.attempts.last().ok_or("accepted attempt missing")?;
    let staged = engine.stage_revision(
        &run.id,
        RevisionStageRequest {
            command_id: "race-stage".into(),
            expected_control_revision: engine.control_state(&run.id)?.control_revision,
            source: source.replace("return base;", "return {revised:base};"),
            input: WorkflowResult::Absent,
        },
    )?;
    let command = WorkflowControlRequest {
        command_id: "race-skip".into(),
        expected_control_revision: staged.state.control_revision,
        target: WorkflowControlTarget::Step {
            logical_id: step.logical_id.clone(),
            expected_attempt_id: Some(attempt.id.clone()),
            expected_turn_id: Some(format!("turn-{}", attempt.id)),
        },
        operation: WorkflowControlOperation::Skip {
            replacement: WorkflowResult::Value {
                value: json!("MUST_NOT_APPLY_TO_THE_FENCED_PLAN"),
            },
        },
    };
    let skipping = {
        let engine = engine.clone();
        let id = run.id.clone();
        tokio::spawn(async move { engine.control(&id, command).await })
    };
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 10),
        host.abort_entered.notified(),
    )
    .await
    .map_err(|error| error.to_string())?;
    assert!(host.native_stopped.load(Ordering::SeqCst));
    engine
        .apply_revision(
            &run.id,
            RevisionApplyRequest {
                command_id: "race-apply".into(),
                expected_control_revision: engine.control_state(&run.id)?.control_revision,
                revision_id: staged.revision.id.clone(),
                mode: RevisionApplyMode::DrainAccepted,
            },
        )
        .await?;
    let mut events = engine.subscribe();
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        loop {
            if engine.control_state(&run.id)?.status == WorkflowRevisionStatus::Draining {
                return Ok::<_, String>(());
            }
            match events.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    })
    .await
    .map_err(|error| error.to_string())??;
    assert_eq!(
        engine.get(&run.id)?.steps[&step.id].attempts[0].status,
        WorkflowStatus::Running,
        "the engine has not yet received the held canonical abort receipt"
    );
    host.release_receipt.notify_one();
    let rejected = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), skipping)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())??;
    assert_eq!(rejected.command.status, WorkflowControlStatus::Rejected);
    assert!(
        rejected
            .command
            .error
            .as_deref()
            .is_some_and(|error| error.contains("plan changed"))
    );
    let ready = tokio::time::timeout(
        Duration::from_secs(/*secs*/ 10),
        engine.wait_revision_settlement(&run.id, &staged.revision.id),
    )
    .await
    .map_err(|error| error.to_string())??;
    assert_eq!(
        ready.revision.status,
        WorkflowRevisionStatus::Ready,
        "a settled native abort cannot strand the old execution's drain"
    );
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), running)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())??;
    let final_run = engine.get(&run.id)?;
    let final_step = &final_run.steps[&step.id];
    assert_eq!(
        (
            final_step.disposition,
            &final_step.result,
            &final_step.control_outcome
        ),
        (WorkflowStepDisposition::Native, &None, &None)
    );
    assert_eq!(
        (
            &final_step.attempts[0].id,
            &final_step.attempts[0].child_thread_id,
            final_step.attempts[0].status
        ),
        (
            &attempt.id,
            &attempt.child_thread_id,
            WorkflowStatus::Failed
        )
    );
    assert_eq!(
        (
            final_run.accepted_starts,
            final_run
                .metrics
                .usage
                .as_ref()
                .map(|usage| usage.total_tokens),
            final_run.result
        ),
        (1, Some(3), None)
    );
    assert!(!final_run.metrics.usage_complete);
    assert_eq!(
        host.native_calls.load(Ordering::SeqCst),
        1,
        "neither the superseded skip nor Apply may start a replacement worker"
    );
    Ok(())
}
