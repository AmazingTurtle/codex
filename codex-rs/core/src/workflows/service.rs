use super::WorkflowLibrary;
use super::WorkflowRun;
use super::host::AgentHost;
use super::notification::NotificationSource;
use super::notification::Notifications;
use crate::session::session::Session;
use codex_dynamic_workflows::RunLimits;
use codex_dynamic_workflows::WorkflowEngine;
use codex_dynamic_workflows::WorkflowSummary;
use codex_features::DynamicWorkflowsMode;
use codex_features::Feature;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde_json::Value;
use std::sync::Arc;
use std::sync::Weak;

#[path = "service_control.rs"]
mod control;

/// One conversation's access to durable workflows and its project/user script library.
pub struct DynamicWorkflowService {
    engine: WorkflowEngine<AgentHost>,
    host: Arc<AgentHost>,
    owner: Weak<Session>,
    owner_id: String,
    library: WorkflowLibrary,
    runs_root: AbsolutePathBuf,
    project_root: AbsolutePathBuf,
    notifications: Arc<Notifications>,
    lifecycle_delivery: Arc<super::lifecycle_delivery::LifecycleDelivery>,
}

pub(crate) async fn service(session: &Arc<Session>) -> Result<Arc<DynamicWorkflowService>, String> {
    let root_id = session
        .services
        .local_agent_runtime
        .execution_control
        .snapshot()
        .await
        .map_err(|err| err.to_string())?
        .root_thread_id;
    let session = if root_id == session.thread_id {
        Arc::clone(session)
    } else {
        Arc::clone(
            &session
                .services
                .local_agent_runtime
                .workflow_thread(root_id)
                .await
                .map_err(|err| err.to_string())?
                .session,
        )
    };
    let service = session
        .services
        .workflows
        .service
        .get_or_try_init(|| async {
            let config = session.get_config().await;
            if config.ephemeral {
                return Err("dynamic workflows are unavailable in ephemeral conversations".into());
            }
            let runs_root = config.codex_home.join("workflow-runs");
            let state_root = runs_root.join(session.thread_id.to_string());
            let project_root = config.cwd.clone();
            let library = WorkflowLibrary::new(project_root.clone(), config.codex_home.clone());
            let host = Arc::new(AgentHost::new(Arc::downgrade(&session), state_root.clone()));
            let engine = WorkflowEngine::new(
                state_root.clone(),
                session.services.code_mode_service.session_provider(),
                Arc::clone(&host),
            )?
            .with_project_root(project_root.clone());
            let lifecycle_delivery =
                Arc::new(super::lifecycle_delivery::LifecycleDelivery::default());
            super::lifecycle_watcher::start(
                &session,
                engine.clone(),
                Arc::clone(&lifecycle_delivery),
            )?;
            let notifications = Arc::new(Notifications::new(state_root));
            notifications.start(&session, engine.clone())?;
            Ok::<Arc<DynamicWorkflowService>, String>(Arc::new(DynamicWorkflowService {
                engine,
                host,
                owner: Arc::downgrade(&session),
                owner_id: session.thread_id.to_string(),
                library,
                runs_root,
                project_root,
                notifications,
                lifecycle_delivery,
            }))
        })
        .await
        .cloned()?;
    super::lifecycle_watcher::replay(&session, &service.engine, &service.lifecycle_delivery)
        .await?;
    if let Err(error) = service
        .notifications
        .replay(&session, &service.engine)
        .await
    {
        tracing::warn!(%error, "workflow terminal notification recovery failed");
    }
    Ok(service)
}

impl DynamicWorkflowService {
    pub(crate) fn project_root(&self) -> &AbsolutePathBuf {
        &self.project_root
    }
    pub fn owner_thread_id(&self) -> &str {
        &self.owner_id
    }
    pub(crate) async fn owns_child(
        &self,
        child: codex_protocol::ThreadId,
        path: &codex_protocol::AgentPath,
    ) -> bool {
        if self.host.owns_child(child, path).await {
            return true;
        }
        // Names aid presentation and acceptance synchronization. The complete
        // persisted child identity is the authority for both naming generations.
        let child = child.to_string();
        let mut offset = 0;
        loop {
            let Ok(runs) = self.list(offset, codex_dynamic_workflows::MAX_PAGE_SIZE) else {
                return false;
            };
            if runs.iter().any(|run| {
                run.steps
                    .values()
                    .flat_map(|step| &step.attempts)
                    .any(|attempt| attempt.child_thread_id.as_deref() == Some(child.as_str()))
            }) {
                return true;
            }
            if runs.len() < codex_dynamic_workflows::MAX_PAGE_SIZE {
                return false;
            }
            offset = offset.saturating_add(runs.len());
        }
    }
    pub fn library(&self) -> &WorkflowLibrary {
        &self.library
    }

    pub(crate) async fn load_saved_from(
        &self,
        caller: &Arc<Session>,
        name: &str,
    ) -> Result<codex_dynamic_workflows::ResolvedWorkflow, String> {
        super::source::load(caller, &self.library, name).await
    }

    pub(crate) async fn read_saved_from(
        &self,
        caller: &Arc<Session>,
        id: &str,
    ) -> Result<codex_dynamic_workflows::SavedWorkflow, String> {
        super::source::read(caller, &self.library, id).await
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<WorkflowRun> {
        self.engine.subscribe()
    }

    pub fn subscribe_progress(&self) -> tokio::sync::broadcast::Receiver<WorkflowSummary> {
        self.engine.subscribe_progress()
    }

    pub fn get(&self, id: &str) -> Result<WorkflowRun, String> {
        let run = self.engine.get(id)?;
        if run.owner_thread_id != self.owner_id {
            return Err("workflow belongs to another thread".into());
        }
        Ok(run)
    }

    pub async fn display_result(
        &self,
        id: &str,
        execution_id: Option<&str>,
    ) -> Result<codex_protocol::workflow::WorkflowLifecycleEvent, String> {
        self.get(id)?;
        let owner = self
            .owner
            .upgrade()
            .ok_or("workflow owner is unavailable")?;
        self.lifecycle_delivery
            .display(&owner, &self.engine, id, execution_id)
            .await
    }

    pub fn list(&self, offset: usize, limit: usize) -> Result<Vec<WorkflowRun>, String> {
        self.engine.list(&self.owner_id, offset, limit)
    }

    pub fn list_summaries(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkflowSummary>, String> {
        self.engine.list_summaries(&self.owner_id, offset, limit)
    }

    pub fn list_project_summaries(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkflowSummary>, String> {
        codex_dynamic_workflows::list_persisted_summaries(
            &self.runs_root,
            Some(&self.project_root),
            offset,
            limit,
        )
    }

    pub fn list_all_summaries(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkflowSummary>, String> {
        codex_dynamic_workflows::list_persisted_summaries(&self.runs_root, None, offset, limit)
    }

    pub fn list_project(&self, offset: usize, limit: usize) -> Result<Vec<WorkflowRun>, String> {
        codex_dynamic_workflows::list_persisted(
            &self.runs_root,
            Some(&self.project_root),
            offset,
            limit,
        )
    }

    pub fn list_all(&self, offset: usize, limit: usize) -> Result<Vec<WorkflowRun>, String> {
        codex_dynamic_workflows::list_persisted(&self.runs_root, None, offset, limit)
    }

    /// Explicit host/user launch. Model launches additionally check their captured turn activation.
    pub async fn start(&self, source: String, input: Value) -> Result<WorkflowRun, String> {
        self.start_with_completion_mode(
            source,
            input,
            codex_dynamic_workflows::WorkflowCompletionMode::Display,
        )
        .await
    }

    pub async fn start_with_completion_mode(
        &self,
        source: String,
        input: Value,
        completion_mode: codex_dynamic_workflows::WorkflowCompletionMode,
    ) -> Result<WorkflowRun, String> {
        let owner = self
            .owner
            .upgrade()
            .ok_or("workflow owner is unavailable")?;
        self.start_from_with_completion_mode(
            owner,
            source,
            input,
            NotificationSource::User,
            completion_mode,
        )
        .await
    }

    pub(crate) async fn start_from(
        &self,
        caller: Arc<Session>,
        source: String,
        input: Value,
        notification: NotificationSource,
    ) -> Result<WorkflowRun, String> {
        self.start_from_with_completion_mode(
            caller,
            source,
            input,
            notification,
            codex_dynamic_workflows::WorkflowCompletionMode::Display,
        )
        .await
    }

    pub(crate) async fn start_from_with_completion_mode(
        &self,
        caller: Arc<Session>,
        source: String,
        input: Value,
        notification: NotificationSource,
        completion_mode: codex_dynamic_workflows::WorkflowCompletionMode,
    ) -> Result<WorkflowRun, String> {
        let owner = self
            .owner
            .upgrade()
            .ok_or("workflow owner is unavailable")?;
        if !Arc::ptr_eq(
            &caller.services.local_agent_runtime.execution_control,
            &owner.services.local_agent_runtime.execution_control,
        ) {
            return Err("workflow caller belongs to another conversation".into());
        }
        let turn = caller.new_default_turn().await;
        // Bound the nested MCP capture future's type before model-tool dispatch.
        let capture: futures::future::BoxFuture<'_, _> =
            Box::pin(caller.capture_speculative_step_context(
                turn,
                &caller.services.local_agent_runtime.shutdown,
            ));
        let step = capture.await.map_err(|error| error.to_string())?;
        super::step::validate_origin(&step)?;
        if !caller.services.code_mode_service.is_available() {
            return Err("dynamic workflow runtime is unavailable".into());
        }
        if !step.turn.config.features.enabled(Feature::DynamicWorkflows)
            || step.turn.config.dynamic_workflows_mode == DynamicWorkflowsMode::Off
        {
            return Err("dynamic workflows are disabled".into());
        }
        let config = owner.get_config().await;
        if !config.features.enabled(Feature::DynamicWorkflows)
            || config.dynamic_workflows_mode == DynamicWorkflowsMode::Off
        {
            return Err("dynamic workflows are disabled".into());
        }
        caller
            .services
            .workflows
            .persist_origin_runtime(&caller, &step.turn)
            .await?;
        let run = self.engine.create_run_with_options(
            self.owner_id.clone(),
            source,
            input,
            RunLimits {
                max_agents: config.dynamic_workflows.max_agents,
                max_tokens: config
                    .dynamic_workflows
                    .max_tokens
                    .map(std::num::NonZeroU64::get),
            },
            completion_mode,
        )?;
        // Persist the authorizing conversation before any worker can be started. Recovery
        // restores this original agent's profile and permissions through normal thread loading.
        self.host.record_origin(&run.id, caller.thread_id)?;
        self.notifications
            .record(&owner, &caller, &run, notification)
            .await?;
        self.launch(owner, Some(caller), run.id.clone(), Launch::Start)?;
        Ok(run)
    }

    pub async fn stop(&self, id: &str) -> Result<WorkflowRun, String> {
        let run = self.get(id)?;
        if run.format_version != codex_dynamic_workflows::WORKFLOW_FORMAT_VERSION {
            return Err("legacy workflow is read-only".into());
        }
        self.host.record_children(&run);
        self.engine.stop(id).await
    }

    pub async fn retry(&self, id: &str) -> Result<WorkflowRun, String> {
        self.resume(id, Launch::Retry, NotificationSource::User)
            .await
    }

    pub(crate) async fn retry_from_model(&self, id: &str) -> Result<WorkflowRun, String> {
        self.resume(id, Launch::Retry, NotificationSource::Model)
            .await
    }

    pub async fn recover(&self, id: &str) -> Result<WorkflowRun, String> {
        self.resume(id, Launch::Recover, NotificationSource::User)
            .await
    }

    async fn resume(
        &self,
        id: &str,
        launch: Launch,
        notification: NotificationSource,
    ) -> Result<WorkflowRun, String> {
        self.get(id)?;
        let owner = self
            .owner
            .upgrade()
            .ok_or("workflow owner is unavailable")?;
        let run = match launch {
            Launch::Retry => self.engine.prepare_retry(id)?,
            Launch::Recover => self.engine.prepare_recovery(id)?,
            Launch::Start | Launch::Revision => {
                return Err(
                    "new workflows and revisions require their explicit preparation".into(),
                );
            }
        };
        self.notifications
            .record(&owner, &owner, &run, notification)
            .await?;
        self.launch(owner, None, id.to_string(), launch)?;
        Ok(run)
    }

    fn launch(
        &self,
        owner: Arc<Session>,
        caller: Option<Arc<Session>>,
        id: String,
        launch: Launch,
    ) -> Result<(), String> {
        let engine = self.engine.clone();
        let notifications = Arc::clone(&self.notifications);
        let membership = owner
            .services
            .local_agent_runtime
            .track_tree_member()
            .map_err(|err| err.to_string())?;
        // The task owns the session while executing, independently of the chat turn and its
        // cancellation token. Host shutdown still fences normal agent admission.
        tokio::spawn(async move {
            let _membership = membership;
            let _caller = caller;
            let runner_engine = engine.clone();
            let runner_id = id.clone();
            let mut runner = tokio::spawn(async move {
                match launch {
                    Launch::Start => runner_engine.run(&runner_id).await,
                    Launch::Retry => runner_engine.run_prepared_retry(&runner_id).await,
                    Launch::Recover => runner_engine.run(&runner_id).await,
                    Launch::Revision => runner_engine.run_prepared_revision(&runner_id).await,
                }
            });
            let result = tokio::select! {
                result = &mut runner => result,
                _ = owner.services.local_agent_runtime.shutdown.cancelled() => {
                    if let Err(error) = engine.interrupt(&id).await { tracing::warn!(workflow_id = id, %error, "workflow shutdown interruption failed"); }
                    runner.await
                }
            };
            match result {
                Ok(Ok(run)) => {
                    if let Err(error) = notifications.deliver(&owner, &engine, &run).await {
                        tracing::warn!(workflow_id = id, %error, "workflow terminal notification failed");
                    }
                }
                Ok(Err(error)) => {
                    tracing::warn!(workflow_id = id, %error, "workflow execution failed")
                }
                Err(error) => tracing::warn!(workflow_id = id, %error, "workflow runner failed"),
            }
        });
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Launch {
    Start,
    Retry,
    Recover,
    Revision,
}
