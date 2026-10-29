use crate::session::session::Session;
use codex_dynamic_workflows::AgentOutcome;
use codex_dynamic_workflows::AgentRequest;
use codex_dynamic_workflows::AgentStartError;
use codex_dynamic_workflows::AgentStartMode;
use codex_dynamic_workflows::WorkflowHost;
use codex_dynamic_workflows::WorkflowLibrary;
use codex_dynamic_workflows::WorkflowRun;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::protocol::AgentStatus;
use codex_thread_store::ReadThreadParams;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::PoisonError;
use std::sync::Weak;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[path = "attempt_clock.rs"]
pub(super) mod clock;
#[path = "host_control.rs"]
mod control;

#[derive(Clone)]
pub(super) struct ChildOwner {
    pub run_id: String,
    pub attempt_id: String,
}
pub(super) type Children = Arc<StdMutex<HashMap<ThreadId, ChildOwner>>>;

#[derive(Default)]
pub(super) struct RunControl {
    pub cancellation: CancellationToken,
    pub scheduling: Arc<Mutex<()>>,
    pub children_changed: tokio::sync::Notify,
    pub settlement_changed: Arc<tokio::sync::Notify>,
    pub observation_error: StdMutex<Option<String>>,
    pub observer: StdMutex<Option<codex_dynamic_workflows::WorkflowObserver>>,
    pub observation_cancellation: CancellationToken,
    pub finish_requested: CancellationToken,
    pub observation_task: Mutex<Option<tokio::task::JoinHandle<Result<(), String>>>>,
    pub admission_guard: StdMutex<Option<codex_dynamic_workflows::WorkflowAdmissionGuard>>,
    pub starts: StdMutex<HashMap<String, Arc<super::receipt::StartReceipt>>>,
    pub usage_scopes: super::usage_ownership::Scopes,
    pub attempt_clocks: Arc<clock::AttemptClocks>,
    admission_revision: StdMutex<control::AdmissionRevision>,
}

pub(super) struct AgentHost {
    owner: Weak<Session>,
    children: Children,
    admission: Arc<Mutex<()>>,
    run_controls: StdMutex<HashMap<String, Arc<RunControl>>>,
    worker_names: StdMutex<HashMap<String, String>>,
    origins_root: AbsolutePathBuf,
}

impl AgentHost {
    pub(super) fn new(owner: Weak<Session>, origins_root: AbsolutePathBuf) -> Self {
        Self {
            owner,
            children: Arc::default(),
            admission: Arc::default(),
            run_controls: StdMutex::default(),
            worker_names: StdMutex::default(),
            origins_root,
        }
    }

    pub(super) fn record_origin(&self, run: &str, caller: ThreadId) -> Result<(), String> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.origins_root.join(format!("{run}.origin")))
            .map_err(|err| err.to_string())?;
        file.write_all(caller.to_string().as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|err| err.to_string())
    }

    pub(super) fn record_children(&self, run: &WorkflowRun) {
        let mut children = self.children.lock().unwrap_or_else(PoisonError::into_inner);
        for attempt in run.steps.values().flat_map(|step| &step.attempts) {
            if let Some(id) = &attempt.child_thread_id
                && let Ok(child) = ThreadId::from_string(id)
            {
                children.insert(
                    child,
                    ChildOwner {
                        run_id: run.id.clone(),
                        attempt_id: attempt.id.clone(),
                    },
                );
            }
        }
    }

    pub(super) async fn owns_child(&self, child: ThreadId, path: &AgentPath) -> bool {
        let run = self
            .worker_names
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path.name())
            .cloned()
            .or_else(|| {
                path.name()
                    .strip_prefix(super::CHILD_NAME_PREFIX)
                    .and_then(|name| name.split_once('_'))
                    .and_then(|(run, _)| uuid::Uuid::parse_str(run).ok())
                    .map(|run| run.to_string())
            });
        let control = run.and_then(|run| {
            self.run_controls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&run)
                .cloned()
        });
        // A fast child can finish before native spawn returns its acceptance receipt.
        // Waiting for that run's barrier lets registration win without blocking sibling stops.
        let _scheduling = match &control {
            Some(control) => Some(control.scheduling.lock().await),
            None => None,
        };
        self.children
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&child)
    }

    async fn owner(&self, request: &AgentRequest) -> Result<Arc<Session>, String> {
        self.owner_for_run(&request.owner_thread_id, &request.run_id)
            .await
    }

    async fn owner_for_run(
        &self,
        owner_thread_id: &str,
        run_id: &str,
    ) -> Result<Arc<Session>, String> {
        let root = self
            .owner
            .upgrade()
            .ok_or("workflow owner is unavailable")?;
        if owner_thread_id != root.thread_id.to_string() {
            return Err("workflow belongs to another thread".into());
        }
        let id = self.authorizing_thread_id(run_id)?;
        if id == root.thread_id {
            return Ok(root);
        }
        // Restore the recorded origin's normal ancestry. The root's privileges must never
        // replace the original caller's profile, permissions, model or environment bindings.
        let mut ancestry = Vec::new();
        let mut child = id;
        while child != root.thread_id {
            if ancestry.len() >= 64 {
                return Err("workflow origin ancestry exceeds the supported depth".into());
            }
            let stored = root
                .services
                .thread_store
                .read_thread(ReadThreadParams {
                    thread_id: child,
                    include_archived: true,
                    include_history: false,
                })
                .await
                .map_err(|err| err.to_string())?;
            let parent = stored
                .parent_thread_id
                .ok_or("workflow origin is outside its owning conversation")?;
            ancestry.push((parent, child));
            child = parent;
        }
        for (parent, child) in ancestry.into_iter().rev() {
            root.services
                .agent_control
                .ensure_child_loaded(parent, child)
                .await
                .map_err(|err| err.to_string())?;
        }
        let origin = root
            .services
            .local_agent_runtime
            .workflow_thread(id)
            .await
            .map_err(|err| err.to_string())?;
        Ok(Arc::clone(&origin.session))
    }
}

impl WorkflowHost for AgentHost {
    async fn fence_run(
        &self,
        run_id: String,
        expected_revision_id: String,
    ) -> Result<codex_dynamic_workflows::WorkflowFenceReceipt, String> {
        self.fence_native_admission(&run_id, &expected_revision_id)
            .await
    }

    async fn resume_revision(&self, run_id: String, revision_id: String) -> Result<(), String> {
        self.activate_native_revision(&run_id, &revision_id).await
    }

    async fn cancel_prepared_attempt(
        &self,
        request: AgentRequest,
    ) -> Result<codex_dynamic_workflows::WorkflowPreparedDisposition, String> {
        self.cancel_native_prepared(&request).await
    }

    async fn guide_attempt(
        &self,
        request: AgentRequest,
        expected_child: String,
        expected_turn: String,
        message: String,
    ) -> Result<(), String> {
        self.guide_native_attempt(&request, &expected_child, &expected_turn, message)
            .await
    }

    async fn interrupt_attempt(
        &self,
        request: AgentRequest,
        expected_child: String,
        expected_turn: String,
    ) -> Result<codex_dynamic_workflows::WorkflowInterruptReceipt, String> {
        self.interrupt_native_attempt(&request, &expected_child, &expected_turn)
            .await
    }

    async fn active_owned_turn(
        &self,
        request: AgentRequest,
        expected_child: String,
    ) -> Result<Option<String>, String> {
        self.current_native_owned_turn(&request, &expected_child)
            .await
    }

    async fn register_admission_guard(
        &self,
        run_id: String,
        guard: codex_dynamic_workflows::WorkflowAdmissionGuard,
    ) -> Result<(), String> {
        let control = self
            .run_controls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(run_id)
            .or_insert_with(|| Arc::new(RunControl::default()))
            .clone();
        *control
            .admission_guard
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(guard);
        Ok(())
    }

    async fn register_observer(
        &self,
        run_id: String,
        observer: codex_dynamic_workflows::WorkflowObserver,
    ) -> Result<(), String> {
        let control = self
            .run_controls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(run_id.clone())
            .or_insert_with(|| Arc::new(RunControl::default()))
            .clone();
        *control
            .observer
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&observer));
        control.attempt_clocks.begin_observation();
        let owner = self
            .owner
            .upgrade()
            .ok_or("workflow owner is unavailable")?;
        let clock_baseline = owner
            .services
            .local_agent_runtime
            .execution_control
            .running_time_snapshot()
            .active_duration_ms;
        let membership = owner
            .services
            .local_agent_runtime
            .track_tree_member()
            .map_err(|error| error.to_string())?;
        let children = Arc::clone(&self.children);
        let task_control = Arc::clone(&control);
        let task_run = run_id.clone();
        *control.observation_task.lock().await = Some(tokio::spawn(async move {
            let _membership = membership;
            let result = super::usage::observe(
                Arc::clone(&owner),
                task_run.clone(),
                Arc::clone(&children),
                Arc::clone(&task_control),
                observer,
                clock_baseline,
            )
            .await;
            if let Err(error) = &result {
                *task_control
                    .observation_error
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(error.clone());
                task_control.cancellation.cancel();
                let scheduling = Arc::clone(&task_control.scheduling).lock_owned().await;
                drop(scheduling);
                super::usage::stop_children(
                    &owner,
                    &children,
                    &task_run,
                    &task_control.usage_scopes,
                )
                .await?;
            }
            result
        }));
        Ok(())
    }

    async fn load_workflow(
        &self,
        run_id: String,
        name: String,
    ) -> Result<codex_dynamic_workflows::ResolvedWorkflow, String> {
        let root = self
            .owner
            .upgrade()
            .ok_or("workflow owner is unavailable")?;
        let caller = self
            .owner_for_run(&root.thread_id.to_string(), &run_id)
            .await?;
        // Recovery must resolve new nested calls in the run's original project,
        // even when the caller is in a worktree or the root has changed cwd.
        let project_root = root
            .services
            .workflows
            .service
            .get()
            .ok_or("workflow service is unavailable")?
            .get(&run_id)?
            .project_root
            .ok_or("workflow has no captured project root")?;
        let config = root.get_config().await;
        let library = WorkflowLibrary::new(project_root, config.codex_home.clone());
        super::source::load(&caller, &library, &name).await
    }

    async fn start_run(&self, run_id: String) -> Result<(), String> {
        let control = self
            .run_controls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&run_id)
            .cloned()
            .ok_or("workflow observation is not registered")?;
        let owner = self
            .owner
            .upgrade()
            .ok_or("workflow owner is unavailable")?;
        tokio::select! {
            result = owner.wait_until_running(&owner.services.local_agent_runtime.shutdown) => result.map_err(|err| err.to_string()),
            _ = control.cancellation.cancelled() => Err("workflow is stopped".into()),
        }
    }

    async fn finish_run(&self, run_id: String) -> Result<(), String> {
        let control = self
            .run_controls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&run_id)
            .cloned();
        if let Some(control) = control {
            control.finish_requested.cancel();
            let task = control.observation_task.lock().await.take();
            let result = match task {
                Some(task) => task
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(std::convert::identity),
                None => Ok(()),
            };
            self.run_controls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&run_id);
            result?;
            let observer = control
                .observer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(observer) = observer {
                let owner = self
                    .owner
                    .upgrade()
                    .ok_or("workflow owner is unavailable")?;
                super::usage_recovery::reconcile(
                    &owner,
                    &self.children,
                    &run_id,
                    &observer,
                    &control.usage_scopes,
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn schedule_agent(
        &self,
        request: AgentRequest,
        mode: AgentStartMode,
    ) -> Result<String, AgentStartError> {
        let control = self
            .run_controls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&request.run_id)
            .cloned()
            .ok_or("workflow has not started")?;
        let receipt = if matches!(mode, AgentStartMode::Start) {
            let observer = control
                .observer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .ok_or("workflow observation is not registered")?;
            let pending = control
                .fresh_admission(&request.revision_id)
                .map_err(AgentStartError::NotStarted)?;
            let receipt = super::receipt::StartReceipt::new(
                request.attempt_id.clone(),
                request.revision_id.clone(),
                pending,
                observer,
            );
            let receipt = control
                .starts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(request.attempt_id.clone())
                .or_insert(receipt)
                .clone();
            Some(super::receipt::ReceiptLease(receipt))
        } else {
            None
        };
        self.worker_names
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                super::worker_name::task_name(&request)?,
                request.run_id.clone(),
            );
        let owner = tokio::select! {
            result = self.owner(&request) => result.map_err(|error| match mode {
                AgentStartMode::Start => AgentStartError::NotStarted(error),
                AgentStartMode::Recover { .. } => AgentStartError::Uncertain(error),
            })?,
            _ = control.cancellation.cancelled() => return Err("workflow is stopped".into()),
        };
        let children = Arc::clone(&self.children);
        // Once admitted, native acceptance owns this task even when V8 cancels its callback.
        // Stop cancels waits and joins the per-run barrier before inspecting accepted children.
        tokio::spawn(super::step::schedule(
            owner,
            control,
            Arc::clone(&self.admission),
            children,
            request,
            mode,
            receipt,
        ))
        .await
        .map_err(|err| AgentStartError::Uncertain(err.to_string()))?
    }

    async fn wait_agent(
        &self,
        request: AgentRequest,
        child_thread_id: String,
    ) -> Result<AgentOutcome, String> {
        let owner = self.owner(&request).await?;
        let child_id = ThreadId::from_string(&child_thread_id).map_err(|err| err.to_string())?;
        let child = match owner
            .services
            .local_agent_runtime
            .workflow_thread(child_id)
            .await
        {
            Ok(child) => child,
            Err(error) if matches!(error.details(), CodexErrorDetails::ThreadNotFound(_)) => {
                // Unloaded recovery consumes authoritative terminal history before restoring
                // a runtime. A live Interrupted status, by contrast, permits human continuation.
                let outcome =
                    super::outcome::persisted(&owner, child_id, /*expected_final*/ None).await?;
                match owner
                    .services
                    .local_agent_runtime
                    .workflow_thread(child_id)
                    .await
                {
                    Ok(child) => child,
                    Err(error)
                        if matches!(error.details(), CodexErrorDetails::ThreadNotFound(_)) =>
                    {
                        if let Some(outcome) = outcome {
                            return Ok(outcome);
                        }
                        owner
                            .services
                            .agent_control
                            .ensure_child_loaded(owner.thread_id, child_id)
                            .await
                            .map_err(|err| err.to_string())?;
                        owner
                            .services
                            .local_agent_runtime
                            .workflow_thread(child_id)
                            .await
                            .map_err(|err| err.to_string())?
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
            Err(error) => return Err(error.to_string()),
        };
        let mut status = child.subscribe_status();
        loop {
            let current = status.borrow_and_update().clone();
            if current == AgentStatus::NotFound {
                return Err("workflow child is unavailable".into());
            }
            let usage_snapshot = child.session.workflow_usage_snapshot().await;
            let observer = self
                .run_controls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&request.run_id)
                .and_then(|control| {
                    control
                        .observer
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone()
                });
            if let Some(observer) = observer {
                if let Some(control) = self
                    .run_controls
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&request.run_id)
                    .cloned()
                {
                    control.attempt_clocks.publish(
                        &observer,
                        &request.attempt_id,
                        usage_snapshot.gate_active_duration_ms,
                        usage_snapshot.terminal.is_some() || usage_snapshot.ownership_retired,
                    )?;
                    let selected = super::usage_ownership::select(
                        &usage_snapshot,
                        /*direct*/ true,
                        &control.usage_scopes,
                    );
                    super::usage::report_selection(
                        &observer,
                        &request.attempt_id,
                        child_id,
                        selected,
                    )?;
                }
                super::attempt_context::publish(
                    &child.session,
                    &usage_snapshot,
                    &request.attempt_id,
                    &observer,
                )
                .await?;
            }
            let active_turn = usage_snapshot
                .active_turn
                .as_ref()
                .map(|turn| turn.turn_id.clone());
            let restored_idle = usage_snapshot.restored && active_turn.is_none();
            let terminal = if usage_snapshot.restored {
                None
            } else {
                usage_snapshot.terminal
            };
            let owned_final = terminal.is_some();
            if crate::agent::status::is_final(&current) || owned_final || restored_idle {
                // Capacity may evict a completed child while its workflow observes the
                // result. Pin only this final I/O window, never an active child wait.
                let _residency = match owner
                    .services
                    .local_agent_runtime
                    .pin_workflow_observation(&child)
                    .await
                {
                    Ok(pin) => {
                        child.flush_rollout().await.map_err(|err| err.to_string())?;
                        pin
                    }
                    // Eviction already flushed and closed the writer. Its persisted
                    // outcome remains authoritative without restoring another runtime.
                    Err(error)
                        if matches!(error.details(), CodexErrorDetails::ThreadNotFound(_)) =>
                    {
                        None
                    }
                    Err(error) => return Err(error.to_string()),
                };
                let outcome = super::outcome::persisted(
                    &owner,
                    child_id,
                    terminal.as_ref().map(|terminal| terminal.turn_id.as_str()),
                )
                .await?;
                // Persistence I/O can overlap a new human turn on the same child. Observe
                // that newer lifecycle before accepting an older turn's result or abort.
                if !owned_final {
                    let latest = child.session.workflow_usage_snapshot().await;
                    if status.has_changed().unwrap_or(false)
                        || *status.borrow() != current
                        || latest.active_turn.as_ref().map(|turn| &turn.turn_id)
                            != active_turn.as_ref()
                    {
                        continue;
                    }
                }
                return outcome.ok_or_else(|| "workflow child has no authoritative terminal outcome; explicit recovery is required".into());
            }
            tokio::select! {
                changed = status.changed() => changed.map_err(|_| "workflow child observation ended before a durable outcome")?,
                _ = async {
                    let control = self.run_controls.lock().unwrap_or_else(PoisonError::into_inner)
                        .get(&request.run_id).cloned();
                    match control { Some(control) => control.cancellation.cancelled().await, None => std::future::pending().await }
                } => {
                    return Err(self.run_controls.lock().unwrap_or_else(PoisonError::into_inner)
                        .get(&request.run_id).and_then(|control| control.observation_error.lock().unwrap_or_else(PoisonError::into_inner).clone())
                        .unwrap_or_else(|| "workflow is stopped".into()));
                },
                _ = owner.services.local_agent_runtime.shutdown.cancelled() => return Err("agent tree is shutting down".into()),
            }
        }
    }

    async fn stop_run(&self, run_id: String) -> Result<(), String> {
        let control = self
            .run_controls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&run_id)
            .cloned();
        if let Some(control) = &control {
            control.cancellation.cancel();
        }
        let _scheduling = match &control {
            Some(control) => Some(control.scheduling.lock().await),
            None => None,
        };
        drop(_scheduling);
        if let Some(control) = &control {
            let starts = control
                .starts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for start in starts {
                start.reject("workflow stopped before native admission")?;
            }
        }
        let owner = self
            .owner
            .upgrade()
            .ok_or("workflow owner is unavailable")?;
        let scopes = control
            .as_ref()
            .map(|control| Arc::clone(&control.usage_scopes))
            .unwrap_or_default();
        super::usage::stop_children(&owner, &self.children, &run_id, &scopes).await
    }
}
