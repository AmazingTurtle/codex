//! One bounded terminal notice per execution, handed to the existing durable mail route.

use super::host::AgentHost;
use super::notification_id::terminal_notice_ids;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::context::ContextualUserFragment;
use crate::context::WorkflowCompletion;
use crate::session::session::Session;
use codex_dynamic_workflows::WorkflowEngine;
use codex_dynamic_workflows::WorkflowRun;
use codex_dynamic_workflows::WorkflowStatus;
use codex_protocol::AgentPath;
use codex_protocol::ResponseItemId;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::turn_input::TurnStartOptions;
use codex_thread_store::FindPersistedInputIdsParams;
use codex_thread_store::PersistedInputId;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;

pub(crate) enum NotificationSource {
    User,
    Model,
}

#[derive(Deserialize, Serialize)]
struct Origin {
    execution_id: String,
    wake_generation: Option<u64>,
    #[serde(default)]
    continuation: Option<super::continuation::ContinuationOrigin>,
}

pub(super) struct Notifications {
    root: AbsolutePathBuf,
    delivery: Mutex<()>,
}

impl Notifications {
    pub(super) fn start(
        self: &Arc<Self>,
        owner: &Arc<Session>,
        engine: WorkflowEngine<AgentHost>,
    ) -> Result<(), String> {
        let mut events = engine.subscribe_lifecycle();
        let mut state = owner
            .services
            .local_agent_runtime
            .execution_control
            .subscribe_state();
        let shutdown = owner.services.local_agent_runtime.shutdown.clone();
        let membership = owner
            .services
            .local_agent_runtime
            .track_tree_member()
            .map_err(|error| error.to_string())?;
        let owner = Arc::downgrade(owner);
        let notifications = Arc::clone(self);
        tokio::spawn(async move {
            let _membership = membership;
            let mut retry = None;
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    event = events.recv() => if matches!(event, Err(tokio::sync::broadcast::error::RecvError::Closed)) { return; },
                    changed = state.changed() => if changed.is_err() { return; },
                    _ = async { match retry { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => {},
                }
                let Some(owner) = owner.upgrade() else {
                    return;
                };
                match notifications.replay(&owner, &engine).await {
                    Ok(pending) => {
                        retry = pending.then(|| {
                            tokio::time::Instant::now() + std::time::Duration::from_secs(1)
                        })
                    }
                    Err(error) => {
                        tracing::warn!(%error, "workflow continuation delivery remains pending");
                        retry =
                            Some(tokio::time::Instant::now() + std::time::Duration::from_secs(1));
                    }
                }
            }
        });
        Ok(())
    }

    pub(super) fn new(root: AbsolutePathBuf) -> Self {
        Self {
            root,
            delivery: Mutex::new(()),
        }
    }

    pub(super) async fn record(
        &self,
        owner: &Session,
        caller: &Session,
        run: &WorkflowRun,
        source: NotificationSource,
    ) -> Result<(), String> {
        let origin = Origin {
            execution_id: run.execution_id.clone(),
            wake_generation: match source {
                NotificationSource::User => None,
                NotificationSource::Model => Some(owner.wake_generation.load(Ordering::Acquire)),
            },
            continuation: super::continuation::ContinuationOrigin::capture(
                caller,
                run.completion_mode,
            )
            .await,
        };
        let temporary = self.root.join(format!("{}.notification.tmp", run.id));
        let mut file = std::fs::File::create(&temporary).map_err(|err| err.to_string())?;
        let bytes = serde_json::to_vec(&origin).map_err(|err| err.to_string())?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|err| err.to_string())?;
        std::fs::rename(
            temporary,
            self.root.join(format!("{}.notification", run.id)),
        )
        .map_err(|err| err.to_string())
    }

    pub(super) async fn replay(
        &self,
        owner: &Arc<Session>,
        engine: &WorkflowEngine<AgentHost>,
    ) -> Result<bool, String> {
        let mut offset = 0;
        let mut pending = false;
        loop {
            let runs = engine.list(
                &owner.thread_id.to_string(),
                offset,
                codex_dynamic_workflows::MAX_PAGE_SIZE,
            )?;
            for run in &runs {
                self.deliver(owner, engine, run).await?;
                if !matches!(
                    run.status,
                    WorkflowStatus::Pending | WorkflowStatus::Running
                ) && !engine.get(&run.id)?.notification_delivered
                {
                    pending = true;
                }
            }
            if runs.len() < codex_dynamic_workflows::MAX_PAGE_SIZE {
                return Ok(pending);
            }
            offset = offset.saturating_add(runs.len());
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "pending receipt lookup, durable history lookup, enqueue, and acknowledgement must stay serialized to prevent duplicate terminal notices"
    )]
    pub(super) async fn deliver(
        &self,
        owner: &Arc<Session>,
        engine: &WorkflowEngine<AgentHost>,
        run: &WorkflowRun,
    ) -> Result<(), String> {
        if run.format_version != codex_dynamic_workflows::WORKFLOW_FORMAT_VERSION
            || run.notification_delivered
            || matches!(
                run.status,
                WorkflowStatus::Pending | WorkflowStatus::Running
            )
            || owner.services.local_agent_runtime.shutdown.is_cancelled()
        {
            return Ok(());
        }
        if run.lifecycle.enabled {
            // New executions publish immutable UI facts through the outbox.
            // Historical pending notices keep their original mail replay path.
            if run.completion_mode == codex_dynamic_workflows::WorkflowCompletionMode::Continue {
                let _delivery = self.delivery.lock().await;
                let path = self.root.join(format!("{}.notification", run.id));
                let origin: Origin = serde_json::from_slice(
                    &std::fs::read(path).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                if origin.execution_id == run.execution_id
                    && let Some(continuation) = origin.continuation
                    && !super::continuation::deliver(owner, run, continuation).await?
                {
                    return Ok(());
                }
            }
            return engine.acknowledge_notification(&run.id, &run.execution_id);
        }
        let _delivery = self.delivery.lock().await;
        let legacy_id =
            ResponseItemId::with_suffix("workflow", format!("{}-{}", run.id, run.execution_id));
        let receipt_ids = terminal_notice_ids(&legacy_id);
        let id = receipt_ids[0].clone();
        let mut pending = false;
        for receipt_id in &receipt_ids {
            if owner
                .services
                .local_agent_runtime
                .execution_control
                .has_delivery(owner.thread_id, receipt_id)
                .await
            {
                pending = true;
                break;
            }
        }
        if !pending && !recorded(owner, &receipt_ids).await? {
            let path = self.root.join(format!("{}.notification", run.id));
            let origin = match std::fs::metadata(&path) {
                Ok(metadata) if metadata.len() <= 512 => Some(
                    serde_json::from_str::<Origin>(
                        &std::fs::read_to_string(path).map_err(|err| err.to_string())?,
                    )
                    .map_err(|err| err.to_string())?,
                ),
                Ok(_) => return Err("workflow notification origin exceeds its size limit".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.to_string()),
            };
            let trigger_turn = matches!(
                run.status,
                WorkflowStatus::Succeeded | WorkflowStatus::Failed
            ) && origin.is_some_and(|origin| {
                origin.execution_id == run.execution_id
                    && origin.wake_generation == Some(owner.wake_generation.load(Ordering::Acquire))
            });
            let status = match run.status {
                WorkflowStatus::Succeeded => "succeeded",
                WorkflowStatus::Failed => "failed",
                WorkflowStatus::Stopped => "stopped",
                WorkflowStatus::Interrupted => "interrupted",
                WorkflowStatus::Pending | WorkflowStatus::Running => return Ok(()),
            };
            let completion = WorkflowCompletion::new(
                run.id.clone(),
                status.to_string(),
                run.steps
                    .values()
                    .filter(|step| step.result.is_some())
                    .count(),
                run.steps.len(),
                run.error.clone(),
            );
            let content = completion.render().into_text().unwrap_or_default();
            let path = AgentPath::root();
            let mut communication =
                InterAgentCommunication::new(path.clone(), path, Vec::new(), content, trigger_turn);
            communication.id = Some(id);
            let control = owner
                .services
                .local_agent_runtime
                .control(owner.session_id());
            control
                .send_inter_agent_communication(
                    owner.thread_id,
                    communication,
                    AgentCommunicationContext::new(AgentCommunicationKind::Result, owner.thread_id),
                    TurnStartOptions {
                        turn_trigger: Some("workflow".into()),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|err| err.to_string())?;
        }
        engine.acknowledge_notification(&run.id, &run.execution_id)
    }
}

async fn recorded(owner: &Session, ids: &[ResponseItemId]) -> Result<bool, String> {
    owner
        .flush_rollout()
        .await
        .map_err(|error| error.to_string())?;
    let found = owner
        .services
        .thread_store
        .find_persisted_input_ids(FindPersistedInputIdsParams {
            thread_id: owner.thread_id,
            ids: ids
                .iter()
                .cloned()
                .map(PersistedInputId::ResponseItem)
                .collect(),
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(!found.is_empty())
}
