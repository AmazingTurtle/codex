//! Schedules one durable attempt through the native child preparation and acceptance path.

use super::host::ChildOwner;
use super::host::Children;
use super::host::RunControl;
use crate::agent::types::AgentMessage;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::thread_spawn::ThreadSpawnError;
use crate::thread_spawn::ThreadSpawnRequest;
use codex_dynamic_workflows::AgentRequest;
use codex_dynamic_workflows::AgentStartError;
use codex_dynamic_workflows::AgentStartMode;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::MultiAgentVersion;
use codex_thread_store::ReadThreadParams;
use serde::Deserialize;
use std::sync::Arc;
use std::sync::PoisonError;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AgentOptions {
    message: String,
    #[serde(rename = "label")]
    _label: Option<String>,
    #[serde(rename = "phase")]
    _phase: Option<String>,
    #[serde(rename = "key")]
    _key: Option<String>,
    agent_type: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
    schema: Option<serde_json::Value>,
    isolation: Option<Isolation>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Isolation {
    Worktree,
}

pub(super) fn validate_origin(step: &StepContext) -> Result<(), String> {
    if step.turn.config.ephemeral
        || step.turn.config.multi_agent_v2.disable_native_tools
        || step.turn.multi_agent_version != MultiAgentVersion::V2
        || step.settings.model_info.multi_agent_version == Some(MultiAgentVersion::Disabled)
        || (step.turn.session_source.get_agent_path().is_some()
            && step.settings.model_info.multi_agent_version != Some(MultiAgentVersion::V2))
        || crate::agent::profile::is_leaf_worker(&step.turn.config, &step.turn.session_source)
    {
        return Err("dynamic workflows require an enabled native agent capability in a persistent conversation".into());
    }
    Ok(())
}

pub(super) async fn schedule(
    owner: Arc<Session>,
    control: Arc<RunControl>,
    admission: Arc<tokio::sync::Mutex<()>>,
    children: Children,
    request: AgentRequest,
    mode: AgentStartMode,
    receipt: Option<super::receipt::ReceiptLease>,
) -> Result<String, AgentStartError> {
    let result = schedule_inner(
        owner,
        control,
        admission,
        children,
        request,
        mode,
        receipt.as_ref().map(|receipt| Arc::clone(&receipt.0)),
    )
    .await;
    if let (Some(receipt), Err(AgentStartError::NotStarted(reason))) = (&receipt, &result) {
        receipt
            .0
            .reject(reason)
            .map_err(AgentStartError::Uncertain)?;
    }
    result
}

async fn schedule_inner(
    owner: Arc<Session>,
    control: Arc<RunControl>,
    admission: Arc<tokio::sync::Mutex<()>>,
    children: Children,
    request: AgentRequest,
    mode: AgentStartMode,
    receipt: Option<Arc<super::receipt::StartReceipt>>,
) -> Result<String, AgentStartError> {
    let pending_cancellation = receipt
        .as_ref()
        .map(|receipt| receipt.admission_cancellation())
        .unwrap_or_else(|| control.cancellation.clone());
    let options: AgentOptions = serde_json::from_value(request.options.clone())
        .map_err(|err| AgentStartError::NotStarted(err.to_string()))?;
    loop {
        tokio::select! {
            result = owner.wait_until_running(&owner.services.local_agent_runtime.shutdown) => result.map_err(|err| err.to_string())?,
            _ = pending_cancellation.cancelled() => return Err(AgentStartError::NotStarted("workflow native admission is cancelled".into())),
        }
        if matches!(mode, AgentStartMode::Start) {
            tokio::select! {
                result = owner.services.local_agent_runtime.wait_for_workflow_capacity() => result.map_err(|err| err.to_string())?,
                _ = pending_cancellation.cancelled() => return Err(AgentStartError::NotStarted("workflow native admission is cancelled".into())),
            }
        }
        let scheduling = tokio::select! {
            scheduling = Arc::clone(&control.scheduling).lock_owned() => scheduling,
            _ = pending_cancellation.cancelled() => return Err(AgentStartError::NotStarted("workflow native admission is cancelled".into())),
        };
        if pending_cancellation.is_cancelled() {
            return Err(AgentStartError::NotStarted(
                "workflow native admission is cancelled".into(),
            ));
        }
        // Only acceptance is serialized across runs. Stop and completion routing use
        // their per-run barrier and can cancel a pending acquisition independently.
        let admission = tokio::select! {
            admission = Arc::clone(&admission).lock_owned() => admission,
            _ = pending_cancellation.cancelled() => return Err(AgentStartError::NotStarted("workflow native admission is cancelled".into())),
        };
        if owner
            .services
            .local_agent_runtime
            .execution_control
            .is_paused()
            || (matches!(mode, AgentStartMode::Start)
                && !owner.services.local_agent_runtime.workflow_has_capacity())
        {
            drop(admission);
            drop(scheduling);
            continue;
        }
        let turn = owner.new_default_turn().await;
        let task_name = super::worker_name::task_name(&request)?;
        let parent_path = turn
            .session_source
            .get_agent_path()
            .unwrap_or_else(AgentPath::root);
        let expected_path = parent_path.join(&task_name)?;
        let legacy_path = parent_path.join(&super::worker_name::legacy_task_name(&request))?;
        let mut message = options.message.clone();
        if let Some(feedback) = &request.validation_feedback {
            let prefix = "\n\nThe previous attempt did not satisfy the output schema. Correct these validation errors:\n";
            let mut end = feedback.len().min(2048);
            while !feedback.is_char_boundary(end) {
                end -= 1;
            }
            message.push_str(prefix);
            message.push_str(&feedback[..end]);
        }
        if let AgentStartMode::Recover { child_thread_id } = &mode {
            let child = match child_thread_id {
                Some(id) => ThreadId::from_string(id).map_err(|err| err.to_string())?,
                None => match owner
                    .services
                    .agent_control
                    .resolve(
                        owner.thread_id,
                        turn.session_source.parent_thread_id(),
                        &turn.session_source,
                        expected_path.as_str(),
                    )
                    .await
                {
                    Ok(child) => child,
                    Err(_) if legacy_path != expected_path => owner
                        .services
                        .agent_control
                        .resolve(
                            owner.thread_id,
                            turn.session_source.parent_thread_id(),
                            &turn.session_source,
                            legacy_path.as_str(),
                        )
                        .await
                        .map_err(|err| format!("cannot reconcile workflow attempt: {err}"))?,
                    Err(error) => {
                        return Err(format!("cannot reconcile workflow attempt: {error}").into());
                    }
                },
            };
            let stored = owner
                .services
                .thread_store
                .read_thread(ReadThreadParams {
                    thread_id: child,
                    include_archived: true,
                    include_history: false,
                })
                .await
                .map_err(|err| err.to_string())?;
            if stored.parent_thread_id != Some(owner.thread_id)
                || ![expected_path.as_str(), legacy_path.as_str()]
                    .contains(&stored.agent_path.as_deref().unwrap_or_default())
            {
                return Err("workflow child identity does not match its durable attempt".into());
            }
            children
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(
                    child,
                    ChildOwner {
                        run_id: request.run_id,
                        attempt_id: request.attempt_id,
                    },
                );
            control.children_changed.notify_one();
            return Ok(child.to_string());
        }
        let step = owner
            .capture_speculative_step_context(turn, &owner.services.local_agent_runtime.shutdown)
            .await
            .map_err(|err| AgentStartError::NotStarted(err.to_string()))?;
        validate_origin(&step).map_err(AgentStartError::NotStarted)?;
        let workspace = match options.isolation {
            Some(Isolation::Worktree) => Some(
                super::worktree::prepare(&step, &request, &mode)
                    .await
                    .map_err(AgentStartError::NotStarted)?,
            ),
            None => None,
        };
        let admission_control = receipt.as_ref().map(|receipt| {
            let receipt_for_admit = Arc::clone(receipt);
            let admitted_owner = Arc::clone(&owner);
            let admitted_control = Arc::clone(&control);
            let admitted_attempt = request.attempt_id.clone();
            let owner = Arc::clone(&owner);
            let children = Arc::clone(&children);
            let control = Arc::clone(&control);
            let run = request.run_id.clone();
            let attempt = request.attempt_id.clone();
            let pending_cancellation = pending_cancellation.clone();
            crate::agent::types::AgentAdmissionControl {
                admitted: Arc::new(move || {
                    receipt_for_admit
                        .admitted()
                        .map_err(codex_protocol::error::CodexErr::InvalidRequest)?;
                    admitted_control.attempt_clocks.admitted(
                        &admitted_attempt,
                        admitted_owner
                            .services
                            .local_agent_runtime
                            .execution_control
                            .running_time_snapshot()
                            .active_duration_ms,
                    );
                    Ok(())
                }),
                check: Arc::new(move || {
                    let owner = Arc::clone(&owner);
                    let children = Arc::clone(&children);
                    let control = Arc::clone(&control);
                    let run = run.clone();
                    let attempt = attempt.clone();
                    let pending_cancellation = pending_cancellation.clone();
                    Box::pin(async move {
                        if control.cancellation.is_cancelled()
                            || pending_cancellation.is_cancelled()
                        {
                            return Err(codex_protocol::error::CodexErr::TurnAborted);
                        }
                        let observer = control
                            .observer
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .clone()
                            .ok_or_else(|| {
                                codex_protocol::error::CodexErr::InvalidRequest(
                                    "workflow observation is unavailable".into(),
                                )
                            })?;
                        super::usage_recovery::reconcile(
                            &owner,
                            &children,
                            &run,
                            &observer,
                            &control.usage_scopes,
                        )
                        .await
                        .map_err(codex_protocol::error::CodexErr::InvalidRequest)?;
                        super::usage::reconcile_live(
                            &owner,
                            &children,
                            &run,
                            &observer,
                            &control.usage_scopes,
                        )
                        .await
                        .map_err(codex_protocol::error::CodexErr::InvalidRequest)?;
                        let guard = control
                            .admission_guard
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .clone()
                            .ok_or_else(|| {
                                codex_protocol::error::CodexErr::InvalidRequest(
                                    "workflow admission guard is unavailable".into(),
                                )
                            })?;
                        guard(&attempt).map_err(codex_protocol::error::CodexErr::InvalidRequest)
                    })
                }),
            }
        });
        let spawned = match crate::thread_spawn::spawn(
            &owner,
            &step,
            &request.attempt_id,
            ThreadSpawnRequest {
                admission_cancellation: Some(pending_cancellation.clone()),
                admission_control,
                task_name,
                message: AgentMessage::WorkflowTask(message),
                fork_mode: None,
                agent_type: options.agent_type.clone(),
                model: options.model.clone(),
                reasoning_effort: options.reasoning_effort.clone(),
                final_output_json_schema: options.schema.clone(),
                workspace,
            },
        )
        .await
        {
            Ok(spawned) => spawned,
            Err(ThreadSpawnError::Backend(error))
                if matches!(error.details(), CodexErrorDetails::InvalidRequest(message) if message == "conversation is paused")
                    || matches!(error.details(), CodexErrorDetails::AgentLimitReached { .. }) =>
            {
                if let Some(receipt) = &receipt {
                    receipt
                        .retry_known_rejection()
                        .map_err(AgentStartError::Uncertain)?;
                }
                // These are known preacceptance admission refusals. Re-capture current
                // authority after the gate opens, retaining the same durable attempt.
                drop(scheduling);
                drop(admission);
                continue;
            }
            Err(ThreadSpawnError::Preparation(error)) => {
                return Err(AgentStartError::NotStarted(error));
            }
            Err(ThreadSpawnError::Backend(error)) => {
                return Err(
                    if receipt
                        .as_ref()
                        .is_some_and(|receipt| receipt.is_prepared())
                    {
                        AgentStartError::NotStarted(error.to_string())
                    } else {
                        AgentStartError::Uncertain(error.to_string())
                    },
                );
            }
        };
        children
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                spawned.agent.thread_id,
                ChildOwner {
                    run_id: request.run_id,
                    attempt_id: request.attempt_id,
                },
            );
        control.children_changed.notify_one();
        if let Some(receipt) = &receipt {
            receipt
                .accept(spawned.agent.thread_id.to_string())
                .map_err(AgentStartError::Uncertain)?;
        }
        let child = owner
            .services
            .local_agent_runtime
            .workflow_thread(spawned.agent.thread_id)
            .await
            .map_err(|err| err.to_string())?;
        child.flush_rollout().await.map_err(|err| err.to_string())?;
        return Ok(spawned.agent.thread_id.to_string());
    }
}
