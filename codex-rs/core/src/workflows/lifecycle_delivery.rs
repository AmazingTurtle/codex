//! The engine outbox becomes durable presentation history without a model turn.

use super::host::AgentHost;
use crate::session::session::Session;
use codex_dynamic_workflows::WorkflowEngine;
use codex_dynamic_workflows::WorkflowLifecycleEvent;
use codex_dynamic_workflows::WorkflowLifecycleKind;
use codex_dynamic_workflows::WorkflowResult;
use codex_dynamic_workflows::WorkflowStatus;
use codex_history::RolloutItem;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::workflow as wire;
use codex_thread_store::FindPersistedInputIdsParams;
use codex_thread_store::PersistContext;
use codex_thread_store::PersistedInputId;
use tokio::sync::Mutex;

#[derive(Default)]
pub(super) struct LifecycleDelivery(Mutex<()>);

impl LifecycleDelivery {
    pub(super) async fn display(
        &self,
        owner: &Session,
        engine: &WorkflowEngine<AgentHost>,
        run_id: &str,
        execution_id: Option<&str>,
    ) -> Result<wire::WorkflowLifecycleEvent, String> {
        let run = engine.get(run_id)?;
        if run.owner_thread_id != owner.thread_id.to_string() {
            return Err("workflow belongs to another thread".into());
        }
        let execution = execution_id.unwrap_or(&run.execution_id);
        let event = engine.show_result_once(run_id, execution)?;
        self.drain(owner, engine, run_id).await?;
        Ok(convert(&event))
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "serialize durable outbox receipt lookup, append, flush, and acknowledgement"
    )]
    pub(super) async fn drain(
        &self,
        owner: &Session,
        engine: &WorkflowEngine<AgentHost>,
        run_id: &str,
    ) -> Result<(), String> {
        let _delivery = self.0.lock().await;
        loop {
            if owner
                .services
                .local_agent_runtime
                .execution_control
                .is_paused()
                || owner.services.local_agent_runtime.shutdown.is_cancelled()
            {
                return Ok(());
            }
            let events = engine.pending_lifecycle_events(
                run_id,
                /*offset*/ 0,
                codex_dynamic_workflows::MAX_PAGE_SIZE,
            )?;
            if events.is_empty() {
                return Ok(());
            }
            owner
                .flush_rollout()
                .await
                .map_err(|error| error.to_string())?;
            let found = owner
                .services
                .thread_store
                .find_persisted_input_ids(FindPersistedInputIdsParams {
                    thread_id: owner.thread_id,
                    ids: events
                        .iter()
                        .map(|event| PersistedInputId::WorkflowLifecycle(event.id.clone()))
                        .collect(),
                })
                .await
                .map_err(|error| error.to_string())?;
            for event in events {
                if event.owner_thread_id != owner.thread_id.to_string() {
                    return Err(
                        "workflow lifecycle owner does not match its delivery thread".into(),
                    );
                }
                let presented = convert(&event);
                if !found.contains(&PersistedInputId::WorkflowLifecycle(event.id.clone())) {
                    if !owner
                        .persist_rollout_items(&[RolloutItem::EventMsg(
                            EventMsg::WorkflowLifecycle(presented.clone()),
                        )])
                        .await
                    {
                        return Err("failed to append workflow lifecycle event".into());
                    }
                    owner
                        .try_ensure_rollout_materialized(PersistContext::ThreadPreparation)
                        .await
                        .map_err(|error| error.to_string())?;
                    owner
                        .flush_rollout()
                        .await
                        .map_err(|error| error.to_string())?;
                }
                // Replaying the same stable ID also repairs an append/flush
                // failure that prevented its earlier live delivery.
                owner
                    .deliver_event_raw(Event {
                        id: event.id.clone(),
                        msg: EventMsg::WorkflowLifecycle(presented),
                    })
                    .await;
                engine.acknowledge_lifecycle_event(run_id, &event.id)?;
            }
        }
    }
}

fn convert(event: &WorkflowLifecycleEvent) -> wire::WorkflowLifecycleEvent {
    let kind = match &event.kind {
        WorkflowLifecycleKind::Started { name, description } => {
            wire::WorkflowLifecycleKind::Started {
                name: name.clone(),
                description: description.clone(),
            }
        }
        WorkflowLifecycleKind::StepSkipped {
            step_id,
            logical_id,
            label,
            phase,
            command_id,
            result: reply,
        } => wire::WorkflowLifecycleKind::StepSkipped {
            step_id: step_id.clone(),
            logical_id: logical_id.clone(),
            label: label.clone(),
            phase: phase.clone(),
            command_id: command_id.clone(),
            result: result(reply),
        },
        WorkflowLifecycleKind::StepCompleted {
            step_id,
            attempt_id,
            child_thread_id,
            owned_turn_ids,
            final_turn_id,
            label,
            phase,
            status: state,
            result: reply,
            error,
            resolved_model,
            reasoning_effort,
            active_duration_ms,
        } => wire::WorkflowLifecycleKind::StepCompleted {
            step_id: step_id.clone(),
            attempt_id: attempt_id.clone(),
            child_thread_id: child_thread_id.clone(),
            owned_turn_ids: owned_turn_ids.clone(),
            final_turn_id: final_turn_id.clone(),
            label: label.clone(),
            phase: phase.clone(),
            status: status(*state),
            result: result(reply),
            error: error.clone(),
            resolved_model: resolved_model.clone(),
            reasoning_effort: reasoning_effort.clone(),
            active_duration_ms: *active_duration_ms,
        },
        WorkflowLifecycleKind::Completed {
            name,
            status: state,
            result: reply,
            error,
            metrics,
        } => wire::WorkflowLifecycleKind::Completed {
            name: name.clone(),
            status: status(*state),
            result: result(reply),
            error: error.clone(),
            metrics: wire::WorkflowLifecycleMetrics {
                usage: metrics
                    .usage
                    .as_ref()
                    .map(|usage| wire::WorkflowLifecycleTokenUsage {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cached_input_tokens: usage.cached_input_tokens,
                        total_tokens: usage.total_tokens,
                    }),
                usage_complete: metrics.usage_complete,
                model_turns: metrics.model_turns,
                agents_used: metrics.agents_used,
                usage_incomplete_reason: metrics.usage_incomplete_reason.clone(),
                active_duration_ms: metrics.active_duration_ms,
                wall_duration_ms: metrics.wall_duration_ms,
            },
        },
        WorkflowLifecycleKind::Interrupted {
            name,
            reason,
            native_settled,
            metrics,
        } => wire::WorkflowLifecycleKind::Interrupted {
            name: name.clone(),
            reason: reason.clone(),
            native_settled: *native_settled,
            metrics: wire::WorkflowLifecycleMetrics {
                usage: metrics
                    .usage
                    .as_ref()
                    .map(|usage| wire::WorkflowLifecycleTokenUsage {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cached_input_tokens: usage.cached_input_tokens,
                        total_tokens: usage.total_tokens,
                    }),
                usage_complete: metrics.usage_complete,
                model_turns: metrics.model_turns,
                agents_used: metrics.agents_used,
                usage_incomplete_reason: metrics.usage_incomplete_reason.clone(),
                active_duration_ms: metrics.active_duration_ms,
                wall_duration_ms: metrics.wall_duration_ms,
            },
        },
        WorkflowLifecycleKind::ResultShown {
            name,
            status: state,
            result: reply,
            error,
        } => wire::WorkflowLifecycleKind::ResultShown {
            name: name.clone(),
            status: status(*state),
            result: result(reply),
            error: error.clone(),
        },
    };
    let completion_mode = match event.completion_mode {
        codex_dynamic_workflows::WorkflowCompletionMode::Display => {
            wire::WorkflowCompletionMode::Display
        }
        codex_dynamic_workflows::WorkflowCompletionMode::Continue => {
            wire::WorkflowCompletionMode::Continue
        }
    };
    wire::WorkflowLifecycleEvent {
        id: event.id.clone(),
        owner_thread_id: event.owner_thread_id.clone(),
        run_id: event.run_id.clone(),
        execution_id: event.execution_id.clone(),
        workflow_name: event.workflow_name.clone(),
        completion_mode,
        sequence: event.sequence,
        created_at: event.created_at,
        kind,
    }
}

fn result(value: &WorkflowResult) -> wire::WorkflowResult {
    match value {
        WorkflowResult::Absent => wire::WorkflowResult::Absent,
        WorkflowResult::Value { value } => wire::WorkflowResult::Value {
            value: value.clone(),
        },
    }
}

fn status(value: WorkflowStatus) -> wire::WorkflowLifecycleStatus {
    match value {
        WorkflowStatus::Pending => wire::WorkflowLifecycleStatus::Pending,
        WorkflowStatus::Running => wire::WorkflowLifecycleStatus::Running,
        WorkflowStatus::Succeeded => wire::WorkflowLifecycleStatus::Succeeded,
        WorkflowStatus::Failed => wire::WorkflowLifecycleStatus::Failed,
        WorkflowStatus::Stopped => wire::WorkflowLifecycleStatus::Stopped,
        WorkflowStatus::Interrupted => wire::WorkflowLifecycleStatus::Interrupted,
    }
}
