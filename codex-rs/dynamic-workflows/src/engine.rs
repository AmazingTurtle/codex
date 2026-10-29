use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;

use codex_code_mode_protocol::CodeModeSession;
use codex_code_mode_protocol::CodeModeSessionCellExecutionLimits;
use codex_code_mode_protocol::CodeModeSessionProvider;
use codex_code_mode_protocol::CodeModeToolKind;
use codex_code_mode_protocol::ExecuteRequest;
use codex_code_mode_protocol::RuntimeResponse;
use codex_code_mode_protocol::ToolDefinition;
use codex_code_mode_protocol::WaitRequest;
use codex_protocol::ToolName;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::WorkflowHost;
use crate::WorkflowRun;
use crate::WorkflowStatus;
use crate::delegate::Delegate;
use crate::execution::ActiveRun;
use crate::execution::TeardownMode;
use crate::store::Store;
use crate::types::bounded_value;
use crate::types::timestamp;
use crate::types::validate_source;
mod controls;
mod lifecycle;
#[cfg(test)]
#[path = "engine/retirement_tests.rs"]
mod retirement_tests;
mod revisions;

/// Durable workflow snapshots with one isolated JavaScript session per execution.
/// Construct once per state directory; cloned handles share active run controls.
pub struct WorkflowEngine<H> {
    store: Arc<Store>,
    provider: Arc<dyn CodeModeSessionProvider>,
    host: Arc<H>,
    active: Arc<Mutex<HashMap<String, Arc<ActiveRun>>>>,
    finished: Arc<tokio::sync::Notify>,
    project_root: Option<AbsolutePathBuf>,
}

struct RunGuard<H: WorkflowHost> {
    store: Arc<Store>,
    active: Arc<Mutex<HashMap<String, Arc<ActiveRun>>>>,
    run_id: String,
    finished: Arc<tokio::sync::Notify>,
    host: Arc<H>,
    completed: bool,
    control: Arc<ActiveRun>,
}

impl<H: WorkflowHost> Drop for RunGuard<H> {
    fn drop(&mut self) {
        self.control
            .runner_done
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if self.completed {
            // Successful retirement happened atomically with the terminal commit.
            // This old stack frame must never touch a newly prepared execution.
            self.finished.notify_waiters();
            return;
        }
        if self
            .control
            .revision_draining
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            self.finished.notify_waiters();
            return;
        }
        let runtime = tokio::runtime::Handle::try_current().ok();
        let Ok(active) = self.active.lock() else {
            self.finished.notify_waiters();
            return;
        };
        if active
            .get(&self.run_id)
            .is_none_or(|current| !Arc::ptr_eq(current, &self.control))
            || self
                .control
                .failed
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            self.finished.notify_waiters();
            return;
        }
        if runtime.is_none() {
            self.control
                .failed
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        // Preparation uses the same active -> store lock order. Keep the identity
        // check and interruption atomic so a replacement cannot enter between them.
        let _ = self.store.interrupt(&self.run_id);
        drop(active);
        if let Some(runtime) = runtime {
            let host = self.host.clone();
            let run_id = self.run_id.clone();
            let active = self.active.clone();
            let finished = self.finished.clone();
            let control = self.control.clone();
            let store = self.store.clone();
            runtime.spawn(async move {
                if active.lock().is_ok_and(|active| {
                    active
                        .get(&run_id)
                        .is_none_or(|current| !Arc::ptr_eq(current, &control))
                }) {
                    finished.notify_waiters();
                    return;
                }
                if control
                    .stop(&*host, &store, &run_id, TeardownMode::Join)
                    .await
                    .is_ok()
                    && let Ok(mut active) = active.lock()
                    && active
                        .get(&run_id)
                        .is_some_and(|current| Arc::ptr_eq(current, &control))
                {
                    active.remove(&run_id);
                }
                finished.notify_waiters();
            });
            return;
        }
        self.finished.notify_waiters();
    }
}

struct SessionGuard(Option<Arc<dyn CodeModeSession>>);

#[derive(Clone, Copy)]
enum RunMode {
    Initial,
    Retry,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(session) = self.0.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(async move {
                let _ = session.shutdown().await;
            });
        }
    }
}

impl<H> Clone for WorkflowEngine<H> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            provider: self.provider.clone(),
            host: self.host.clone(),
            active: self.active.clone(),
            finished: self.finished.clone(),
            project_root: self.project_root.clone(),
        }
    }
}

impl<H: WorkflowHost> WorkflowEngine<H> {
    pub fn new(
        root: AbsolutePathBuf,
        provider: Arc<dyn CodeModeSessionProvider>,
        host: Arc<H>,
    ) -> Result<Self, String> {
        Ok(Self {
            store: Arc::new(Store::new(root)?),
            provider,
            host,
            active: Arc::new(Mutex::new(HashMap::new())),
            finished: Arc::new(tokio::sync::Notify::new()),
            project_root: None,
        })
    }

    pub fn with_project_root(mut self, project_root: AbsolutePathBuf) -> Self {
        self.project_root = Some(project_root);
        self
    }

    pub fn availability(&self) -> Result<(), String> {
        self.provider.availability()
    }

    /// Bounded lifecycle stream. Lagged consumers must reread authoritative state.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<WorkflowRun> {
        self.store.events.subscribe()
    }

    pub fn subscribe_progress(&self) -> tokio::sync::broadcast::Receiver<crate::WorkflowSummary> {
        self.store.progress.subscribe()
    }

    pub fn create_run(
        &self,
        owner_thread_id: String,
        source: String,
        input: Value,
    ) -> Result<WorkflowRun, String> {
        self.create_run_with_limits(owner_thread_id, source, input, crate::RunLimits::default())
    }

    pub fn create_run_with_limits(
        &self,
        owner_thread_id: String,
        source: String,
        input: Value,
        limits: crate::RunLimits,
    ) -> Result<WorkflowRun, String> {
        self.create_run_with_options(
            owner_thread_id,
            source,
            input,
            limits,
            crate::WorkflowCompletionMode::Display,
        )
    }

    pub fn create_run_with_options(
        &self,
        owner_thread_id: String,
        source: String,
        input: Value,
        limits: crate::RunLimits,
        completion_mode: crate::WorkflowCompletionMode,
    ) -> Result<WorkflowRun, String> {
        self.availability()?;
        validate_source(&source)?;
        bounded_value(&input)?;
        limits.validate()?;
        let compiled = crate::compile_script(&source)?;
        let phases: Vec<_> = compiled
            .metadata
            .phases
            .iter()
            .map(|phase| phase.title.clone())
            .collect();
        if owner_thread_id.is_empty() || owner_thread_id.len() > 128 {
            return Err("invalid workflow owner".into());
        }
        let now = timestamp();
        let mut run = WorkflowRun {
            id: uuid::Uuid::new_v4().to_string(),
            lifecycle_version: 1,
            completion_mode,
            lifecycle: crate::WorkflowLifecycleState {
                enabled: true,
                ..Default::default()
            },
            steering: Default::default(),
            format_version: crate::WORKFLOW_FORMAT_VERSION,
            metadata: Some(compiled.metadata),
            limits,
            accepted_starts: 0,
            phase_progress: phases
                .iter()
                .map(|title| crate::PhaseProgress {
                    title: title.clone(),
                    status: crate::PhaseStatus::Pending,
                    steps_discovered: 0,
                    steps_completed: 0,
                    updated_at: None,
                    completed_at: None,
                    metrics: Default::default(),
                })
                .collect(),
            metrics: crate::WorkflowMetrics {
                model_turns: Some(0),
                agents_used: Some(0),
                ..Default::default()
            },
            activity: crate::WorkflowActivity::default(),
            logs: Vec::new(),
            nested_workflows: Default::default(),
            budget_observations: Default::default(),
            worker_sequence: 0,
            execution_id: uuid::Uuid::new_v4().to_string(),
            notification_delivered: false,
            owner_thread_id,
            project_root: self.project_root.clone(),
            source_hash: crate::orchestration::source_hash(&source),
            source,
            input,
            status: WorkflowStatus::Pending,
            phase: None,
            phases,
            steps: Default::default(),
            result: None,
            error: None,
            created_at: now,
            updated_at: now,
        };
        run.steering = crate::steering::WorkflowSteeringState::initialize(&run)?;
        self.store.insert(&mut run)?;
        Ok(run)
    }

    pub fn get(&self, run_id: &str) -> Result<WorkflowRun, String> {
        self.store.get(run_id)
    }

    /// Acknowledges delivery for the exact execution revision, ignoring stale receipts.
    pub fn acknowledge_notification(&self, run_id: &str, execution_id: &str) -> Result<(), String> {
        self.store.acknowledge_notification(run_id, execution_id)
    }

    pub fn list(
        &self,
        owner_thread_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkflowRun>, String> {
        self.store.list(owner_thread_id, offset, limit)
    }

    pub fn list_summaries(
        &self,
        owner_thread_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<crate::WorkflowSummary>, String> {
        self.store.list_summaries(owner_thread_id, offset, limit)
    }

    pub async fn recover(&self, run_id: &str) -> Result<WorkflowRun, String> {
        self.prepare_recovery(run_id)?;
        self.run(run_id).await
    }

    pub async fn retry(&self, run_id: &str) -> Result<WorkflowRun, String> {
        self.prepare_retry(run_id)?;
        self.run_prepared_retry(run_id).await
    }

    pub fn prepare_retry(&self, run_id: &str) -> Result<WorkflowRun, String> {
        self.prepare(
            run_id,
            &[
                WorkflowStatus::Failed,
                WorkflowStatus::Stopped,
                WorkflowStatus::Interrupted,
            ],
            RunMode::Retry,
        )?;
        self.get(run_id)
    }

    pub fn prepare_recovery(&self, run_id: &str) -> Result<WorkflowRun, String> {
        self.prepare(
            run_id,
            &[
                WorkflowStatus::Failed,
                WorkflowStatus::Stopped,
                WorkflowStatus::Interrupted,
            ],
            RunMode::Initial,
        )?;
        self.get(run_id)
    }

    pub async fn run_prepared_retry(&self, run_id: &str) -> Result<WorkflowRun, String> {
        self.run_mode(run_id, RunMode::Retry).await
    }

    fn prepare(
        &self,
        run_id: &str,
        allowed: &[WorkflowStatus],
        mode: RunMode,
    ) -> Result<(), String> {
        let active = self.active.lock().map_err(|err| err.to_string())?;
        if active.contains_key(run_id) {
            return Err("workflow is already running".into());
        }
        self.store.update(run_id, |run| {
            crate::orchestration::require_executable(run)?;
            if !allowed.contains(&run.status) {
                return Err("workflow cannot be recovered in its current state".into());
            }
            if matches!(run.steering.state.status,crate::WorkflowRevisionStatus::Fencing|crate::WorkflowRevisionStatus::Draining|crate::WorkflowRevisionStatus::Ready) {return Err("workflow has a pending plan revision; inspect it and explicitly resume when ready".into());}
            if run.steering.state.status==crate::WorkflowRevisionStatus::Blocked {run.steering.state.status=crate::WorkflowRevisionStatus::Active;run.steering.state.pending_revision_id=None;}
            run.status = WorkflowStatus::Pending;
            run.error = None;
            run.execution_id = uuid::Uuid::new_v4().to_string();
            run.result = None;
            run.notification_delivered = false;
            run.lifecycle.terminal_ready = false;
            run.lifecycle.interrupted_unsettled = false;
            run.activity.execution_duration_ms = 0;
            if let Some(revision)=run.steering.revisions.get_mut(&run.steering.state.active_revision_id) {revision.execution_ids.push(run.execution_id.clone());}
            if matches!(mode, RunMode::Retry) {
                for step in run.steps.values_mut().filter(|step| step.result.is_none()) {
                    step.schema_attempts = 0;
                }
            }
            Ok(())
        })
    }

    pub async fn stop(&self, run_id: &str) -> Result<WorkflowRun, String> {
        self.end_run(run_id, WorkflowStatus::Stopped).await
    }

    /// Interrupt an owned runner during host shutdown, preserving explicit recovery.
    pub async fn interrupt(&self, run_id: &str) -> Result<WorkflowRun, String> {
        self.end_run(run_id, WorkflowStatus::Interrupted).await
    }

    async fn end_run(&self, run_id: &str, status: WorkflowStatus) -> Result<WorkflowRun, String> {
        let execution_id = self.get(run_id)?.execution_id;
        let control = self
            .active
            .lock()
            .map_err(|error| error.to_string())?
            .get(run_id)
            .cloned();
        let retry = control
            .as_ref()
            .is_some_and(|control| control.failed.load(std::sync::atomic::Ordering::SeqCst));
        self.store.update(run_id, |run| {
            crate::orchestration::require_executable(run)?;
            if run.execution_id != execution_id {
                return Err("workflow execution changed before its stop was applied".into());
            }
            if run.status == WorkflowStatus::Succeeded {
                return Err("completed workflow cannot be stopped".into());
            }
            run.status = status;
            Ok(())
        })?;
        if let Some(control) = &control {
            control.cancellation.cancel();
            control
                .stop(
                    &*self.host,
                    &self.store,
                    run_id,
                    if retry {
                        TeardownMode::RetryFailed
                    } else {
                        TeardownMode::Join
                    },
                )
                .await?;
            if (retry
                || control
                    .revision_draining
                    .load(std::sync::atomic::Ordering::SeqCst))
                && control
                    .runner_done
                    .load(std::sync::atomic::Ordering::SeqCst)
            {
                let mut active = self.active.lock().map_err(|error| error.to_string())?;
                if active
                    .get(run_id)
                    .is_some_and(|current| Arc::ptr_eq(current, control))
                {
                    active.remove(run_id);
                }
                self.finished.notify_waiters();
            }
        } else {
            self.host.stop_run(run_id.to_string()).await?;
            self.host.finish_run(run_id.to_string()).await?;
        }
        loop {
            let notified = self.finished.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self
                .active
                .lock()
                .map_err(|error| error.to_string())?
                .get(run_id)
                .is_some_and(|current| {
                    control
                        .as_ref()
                        .is_none_or(|expected| Arc::ptr_eq(current, expected))
                })
            {
                break;
            }
            if let Some(control) = &control
                && control.failed.load(std::sync::atomic::Ordering::SeqCst)
            {
                return control
                    .stop(&*self.host, &self.store, run_id, TeardownMode::Join)
                    .await
                    .and_then(|()| {
                        let run = self.get(run_id)?;
                        if run.execution_id != execution_id {
                            return Err(
                                "workflow execution changed while stopping its prior execution"
                                    .into(),
                            );
                        }
                        Ok(run)
                    });
            }
            notified.await;
        }
        let _active = self.active.lock().map_err(|error| error.to_string())?;
        let current = self.get(run_id)?;
        if current.execution_id != execution_id {
            return Err("workflow execution changed while stopping its prior execution".into());
        }
        if !current.lifecycle.terminal_ready {
            self.store.update(run_id, |run| {
                if run.execution_id != execution_id {
                    return Err(
                        "workflow execution changed while stopping its prior execution".into(),
                    );
                }
                run.lifecycle.terminal_ready = true;
                Ok(())
            })?;
        }
        self.get(run_id)
    }

    pub async fn run(&self, run_id: &str) -> Result<WorkflowRun, String> {
        self.run_mode(run_id, RunMode::Initial).await
    }

    async fn run_mode(&self, run_id: &str, mode: RunMode) -> Result<WorkflowRun, String> {
        let control = Arc::new(ActiveRun::default());
        let cancellation = control.cancellation.clone();
        {
            let mut active = self.active.lock().map_err(|err| err.to_string())?;
            if active.contains_key(run_id) {
                return Err("workflow is already running".into());
            }
            self.store.update(run_id, |run| {
                crate::orchestration::require_executable(run)?;
                if run.status != WorkflowStatus::Pending {
                    return Err("workflow requires explicit retry or recovery".into());
                }
                run.status = WorkflowStatus::Running;
                run.error = None;
                run.phase = None;
                if run.steering.version == 1 {
                    run.steering.suffix_open = false;
                }
                Ok(())
            })?;
            active.insert(run_id.to_string(), control.clone());
        }
        let mut guard = RunGuard {
            store: self.store.clone(),
            active: self.active.clone(),
            run_id: run_id.to_string(),
            finished: self.finished.clone(),
            host: self.host.clone(),
            completed: false,
            control: control.clone(),
        };
        let mut result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err("workflow stopped".into()),
            _ = control.revision_cancel.cancelled() => Err("workflow VM superseded by a staged revision".into()),
            result = async {
                let store = self.store.clone();
                let observed_run = run_id.to_string();
                let observed_execution = self.get(run_id)?.execution_id;
                let admission_store = self.store.clone();
                let admission_run = run_id.to_string();
                self.host.register_admission_guard(run_id.to_string(), Arc::new(move |attempt_id| {
                    crate::orchestration::validate_native_admission(&admission_store.get(&admission_run)?, attempt_id)
                })).await?;
                self.host.register_observer(run_id.to_string(), Arc::new(move |event| match event {
                    crate::WorkflowHostEvent::ModelTurnsIncomplete { attempt_id, thread_id } => store.mark_model_turns_incomplete(&observed_run, &attempt_id, &thread_id),
                    crate::WorkflowHostEvent::ModelTurns { attempt_id, thread_id, model_turns } => store.record_model_turns(&observed_run, &attempt_id, &thread_id, model_turns),
                    crate::WorkflowHostEvent::AttemptClock { attempt_id, active_duration_ms, at_ms } => store.record_attempt_clock(&observed_run, &observed_execution, &attempt_id, active_duration_ms, at_ms),
                    crate::WorkflowHostEvent::AttemptContext { attempt_id, owned_turn_ids, final_turn_id, resolved_model, reasoning_effort } => store.update(&observed_run, |run| {
                        if run.steering.known_attempt_ids.contains(&attempt_id) && !run.steps.values().flat_map(|step|&step.attempts).any(|attempt|attempt.id==attempt_id) {return Ok(());}
                        if owned_turn_ids.len() > 128 || owned_turn_ids.iter().any(|id| id.is_empty() || id.len() > 128)
                            || resolved_model.as_ref().is_some_and(|value| value.len() > 256)
                            || reasoning_effort.as_ref().is_some_and(|value| value.len() > 256)
                            || final_turn_id.as_ref().is_some_and(|value| value.is_empty() || value.len() > 128)
                        { return Err("workflow attempt context exceeds its bounded limit".into()); }
                        let attempt = run.steps.values_mut().flat_map(|step| &mut step.attempts).find(|attempt| attempt.id == attempt_id).ok_or("workflow attempt identity missing")?;
                        for id in owned_turn_ids { if !attempt.owned_turn_ids.contains(&id) { attempt.owned_turn_ids.push(id); } }
                        if attempt.owned_turn_ids.len() > 128 { return Err("workflow attempt turn limit exhausted (128)".into()); }
                        if let Some(turn) = final_turn_id {
                            if !attempt.owned_turn_ids.contains(&turn) { return Err("workflow final turn is not owned by the attempt".into()); }
                            if attempt.final_turn_id.as_ref().is_some_and(|old| old != &turn) { return Err("workflow final turn identity changed".into()); }
                            attempt.final_turn_id = Some(turn);
                        }
                        if let Some(model) = resolved_model { attempt.resolved_model = Some(model); }
                        if let Some(effort) = reasoning_effort { attempt.reasoning_effort = Some(effort); }
                        Ok(())
                    }),
                    crate::WorkflowHostEvent::StartUncertain { attempt_id, revision_id } => store.update(&observed_run, |run|{
                        crate::orchestration::validate_effect_execution(run,&observed_execution,&revision_id)?;
                        crate::orchestration::validate_native_admission(run,&attempt_id)?;
                        let attempt=run.steps.values_mut().flat_map(|step|&mut step.attempts).find(|attempt|attempt.id==attempt_id).ok_or("workflow admission identity missing")?;
                        if attempt.admission==crate::AdmissionStatus::Reserved {attempt.admission=crate::AdmissionStatus::Uncertain;}
                        Ok(())
                    }),
                    crate::WorkflowHostEvent::StartRetryable { attempt_id } => store.update(&observed_run, |run| {
                        if run.status != WorkflowStatus::Running { return Err("workflow is stopped".into()); }
                        let attempt = run.steps.values_mut().flat_map(|step| &mut step.attempts).find(|attempt| attempt.id == attempt_id).ok_or("workflow admission identity missing")?;
                        if attempt.child_thread_id.is_some() || attempt.status != WorkflowStatus::Running
                            || !matches!(attempt.admission, crate::AdmissionStatus::Reserved | crate::AdmissionStatus::Uncertain)
                        { return Err("workflow attempt cannot restore a native reservation".into()); }
                        attempt.admission = crate::AdmissionStatus::Reserved;
                        Ok(())
                    }),
                    crate::WorkflowHostEvent::StartAccepted { attempt_id, child_thread_id } => store.update(&observed_run, |run|crate::orchestration::record_acceptance(run, &attempt_id, child_thread_id)),
                    crate::WorkflowHostEvent::StartRejected { attempt_id, reason } => store.update(&observed_run, |run|crate::orchestration::record_rejection(run, &attempt_id, reason)),
                    crate::WorkflowHostEvent::UsageIncomplete { reason } => store.mark_usage_incomplete(&observed_run, reason),
                    crate::WorkflowHostEvent::ExecutionClock { active_duration_ms, at_ms } => store.record_clock(&observed_run, &observed_execution, active_duration_ms, at_ms),
                    crate::WorkflowHostEvent::Usage { attempt_id, thread_id, usage } => store.record_usage(&observed_run, &attempt_id, &thread_id, usage),
                    crate::WorkflowHostEvent::UsageComplete { attempt_id, thread_id } => store.complete_usage(&observed_run, &attempt_id, &thread_id),
                    crate::WorkflowHostEvent::ExecutionActive { active, at_ms } => store.set_execution_active(&observed_run, active, at_ms),
                })).await?;
                let revision_id=self.get(run_id)?.steering.state.active_revision_id;
                if !revision_id.is_empty() {self.host.resume_revision(run_id.to_string(),revision_id).await?;}
                self.host.start_run(run_id.to_string()).await?;
                if cancellation.is_cancelled() { return Err("workflow stopped".into()); }
                self.execute(run_id, cancellation.clone(), control.clone(), mode).await
            } => result,
        };
        if control
            .revision_draining
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            guard.completed = true;
            return self.get(run_id);
        }
        if result.is_err() {
            control
                .stop(&*self.host, &self.store, run_id, TeardownMode::Join)
                .await?;
        } else {
            control.supervision.fence()?;
            control.supervision.drain().await;
            let finish = tokio::select! {
                biased;
                _=cancellation.cancelled()=>control.stop(&*self.host,&self.store,run_id,TeardownMode::Join).await,
                result=self.host.finish_run(run_id.to_string())=>result,
            };
            if let Err(error) = finish {
                control.failed_finish(&self.store, run_id, error).await?;
                result = Err("workflow final observation failed".into());
            }
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;
        self.store.set_execution_active(run_id, false, now_ms)?;
        self.store.checkpoint(run_id)?;
        // Cleanup above has joined the VM, native observers and accepted supervision.
        // Recovery takes this same lock, so publishing a terminal execution and retiring
        // its exact admission fence are one operation from the caller's perspective.
        let mut active = self.active.lock().map_err(|error| error.to_string())?;
        if active
            .get(run_id)
            .is_none_or(|current| !Arc::ptr_eq(current, &control))
        {
            return Err("workflow execution ownership changed before retirement".into());
        }
        if control
            .revision_draining
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            guard.completed = true;
            return self.get(run_id);
        }
        self.store.update(run_id, |run| {
            run.lifecycle.terminal_ready = true;
            if result.is_err()
                && run.steering.version == 1
                && !run.steering.suffix_open
                && run.steering.replay_frontier > 0
            {
                run.steering.state.status = crate::WorkflowRevisionStatus::Blocked;
                if let Some(revision) = run
                    .steering
                    .revisions
                    .get_mut(&run.steering.state.active_revision_id)
                {
                    revision.status = crate::WorkflowRevisionStatus::Blocked;
                    revision.error = result.as_ref().err().cloned();
                }
            }
            if run.status == WorkflowStatus::Running {
                match &result {
                    Ok(()) => run.status = WorkflowStatus::Succeeded,
                    Err(error) => {
                        run.status = WorkflowStatus::Failed;
                        run.error = Some(error.chars().take(/*n*/ 2048).collect());
                    }
                }
            }
            crate::orchestration::refresh_phases(run);
            Ok(())
        })?;
        let terminal = self.get(run_id)?;
        guard.completed = true;
        active.remove(run_id);
        drop(active);
        self.finished.notify_waiters();
        Ok(terminal)
    }

    async fn execute(
        &self,
        run_id: &str,
        cancellation: CancellationToken,
        control: Arc<ActiveRun>,
        mode: RunMode,
    ) -> Result<(), String> {
        let run = self.get(run_id)?;
        crate::orchestration::require_executable(&run)?;
        let compiled = crate::compile_script(&run.source)?;
        let session = self
            .provider
            .create_session_with_limits(CodeModeSessionCellExecutionLimits {
                profile: codex_code_mode_protocol::CodeModeCellExecutionProfile::Workflow,
                max_yield_time_ms: Some(/*value*/ 1_000),
                max_heap_size_bytes: Some(64 * 1024 * 1024),
            })
            .await?;
        let mut session_guard = SessionGuard(Some(session.clone()));
        *control.session.lock().map_err(|error| error.to_string())? = Some(session.clone());
        let mut budget = crate::orchestration::budget(&run);
        budget["observations"] =
            serde_json::to_value(&run.budget_observations).map_err(|error| error.to_string())?;
        let source = if run.steering.version == 1 {
            crate::compiler::render_revision_script(
                &compiled,
                &run.input,
                &budget,
                run.steering.replay_frontier as usize,
            )?
        } else {
            crate::compiler::render_script(&compiled, &run.input, &budget)?
        };
        let delegate = Arc::new(Delegate {
            store: self.store.clone(),
            host: self.host.clone(),
            run_id: run_id.to_string(),
            seen: Arc::new(Mutex::new(HashSet::new())),
            seen_nested: Arc::new(Mutex::new(HashSet::new())),
            pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            retry_recovered_failure: matches!(mode, RunMode::Retry),
            seen_effects: Arc::new(Mutex::new(HashSet::new())),
            checkpoint_index: Arc::new(Mutex::new(0)),
            execution_id: run.execution_id.clone(),
            revision_id: run.steering.state.active_revision_id.clone(),
            supervision: control.supervision.clone(),
            cancellation: control.cancellation.clone(),
        });
        let result = async {
            let started = session.execute(ExecuteRequest {
                tool_call_id: format!("workflow-{run_id}"),
                enabled_tools: vec![ToolDefinition {
                    name: "workflow_host".into(), tool_name: ToolName::plain("workflow_host"), description: String::new(),
                    kind: CodeModeToolKind::Function, input_schema: None, input_schema_max_bytes: None, output_schema: None,
                }],
                source, yield_time_ms: Some(/*value*/ 1_000), max_output_tokens: Some(/*value*/ 128),
            }, delegate.clone(), /*preempt*/ None).await?;
            let cell_id = started.cell_id.clone();
            let observe = async {
                let mut response = started.initial_response().await?;
                loop {
                    match response {
                        RuntimeResponse::Yielded { .. } => {
                            self.store.checkpoint(run_id)?;
                            response = session.wait(WaitRequest { cell_id: cell_id.clone(), yield_time_ms: 1_000 }, /*preempt*/ None).await?.into();
                        }
                        RuntimeResponse::Result { error_text, .. } => {
                            if let Some(error) = error_text { return Err(error); }
                            if delegate.pending.load(std::sync::atomic::Ordering::SeqCst) != 0 { return Err("workflow finished with unfinished agent steps or nested operations; explicit recovery required".into()); }
                            // A changed branch that omits a previous step cannot
                            // silently turn partial work into a successful replay.
                            let seen = delegate.seen.lock().map_err(|err| err.to_string())?;
                            let run = self.get(run_id)?;
                            if let Some(error) = run.error { return Err(error); }
                            if run.steps.keys().any(|id| !seen.contains(id)) { return Err("inconsistent replay omitted a recorded step".into()); }
                            let seen_nested = delegate.seen_nested.lock().map_err(|error|error.to_string())?;
                            if run.nested_workflows.keys().any(|id|!seen_nested.contains(id)) { return Err("inconsistent replay omitted a nested workflow".into()); }
                            if run.steps.values().filter(|step|step.disposition!=crate::WorkflowStepDisposition::Skipped).any(|step| step.attempts.last().is_some_and(|attempt| matches!(attempt.status, WorkflowStatus::Running | WorkflowStatus::Interrupted))) {
                                return Err("workflow finished with unfinished agent steps; explicit recovery required".into());
                            }
                            return Ok(());
                        }
                        RuntimeResponse::Terminated { .. } => return Err("workflow runtime terminated".into()),
                    }
                }
            };
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    session.terminate(cell_id).await?;
                    Err("workflow stopped".into())
                }
                result = observe => result,
            }
        }.await;
        let shutdown = session.shutdown().await;
        if shutdown.is_ok() {
            control
                .session
                .lock()
                .map_err(|error| error.to_string())?
                .take();
        }
        session_guard.0 = None;
        result.and(shutdown)
    }
}
