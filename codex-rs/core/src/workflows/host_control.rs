//! Native revision fences preserve accepted tasks and target only captured owned turns.
//!
use super::AgentHost;
use super::RunControl;
use crate::CodexThread;
use crate::session::session::Session;
use codex_dynamic_workflows::AgentRequest;
use codex_dynamic_workflows::WorkflowFenceReceipt;
use codex_dynamic_workflows::WorkflowInterruptReceipt;
use codex_dynamic_workflows::WorkflowPreparedDisposition;
use codex_protocol::ThreadId;
use codex_protocol::turn_input::SteerSubmission;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::user_input::UserInput;
use std::sync::Arc;
use std::sync::PoisonError;
use tokio_util::sync::CancellationToken;

pub(super) struct AdmissionRevision {
    pub(super) id: String,
    pub(super) cancellation: CancellationToken,
}

impl Default for AdmissionRevision {
    fn default() -> Self {
        Self {
            id: String::new(),
            cancellation: CancellationToken::new(),
        }
    }
}

impl RunControl {
    pub(super) fn fresh_admission(&self, revision_id: &str) -> Result<CancellationToken, String> {
        let mut admission = self
            .admission_revision
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if admission.id.is_empty() && !admission.cancellation.is_cancelled() {
            admission.id = revision_id.to_string();
            admission.cancellation = self.cancellation.child_token();
        }
        if admission.id != revision_id || admission.cancellation.is_cancelled() {
            return Err("workflow revision is fenced before native admission".into());
        }
        Ok(admission.cancellation.child_token())
    }
}

impl AgentHost {
    pub(super) async fn fence_native_admission(
        &self,
        run_id: &str,
        expected_revision_id: &str,
    ) -> Result<WorkflowFenceReceipt, String> {
        let control = self
            .run_controls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(run_id)
            .cloned();
        let Some(control) = control else {
            return Ok(WorkflowFenceReceipt::default());
        };
        {
            let mut admission = control
                .admission_revision
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if admission.id.is_empty() {
                admission.id = expected_revision_id.to_string();
            }
            if admission.id != expected_revision_id {
                return Err("workflow admission revision changed".into());
            }
            admission.cancellation.cancel();
        }
        // Accepted native creation is shielded under this barrier. Pending waits
        // observe admission cancellation without cancelling their accepted peers.
        let scheduling = control.scheduling.lock().await;
        let starts = control
            .starts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut receipt = WorkflowFenceReceipt::default();
        for start in starts {
            if start.revision_id() != expected_revision_id {
                continue;
            }
            match start.disposition() {
                super::super::receipt::StartDisposition::Prepared => {
                    start.reject("workflow revision fenced before native admission")?
                }
                super::super::receipt::StartDisposition::Accepted => receipt
                    .accepted_attempt_ids
                    .push(start.attempt_id().to_string()),
                super::super::receipt::StartDisposition::Uncertain => receipt
                    .uncertain_attempt_ids
                    .push(start.attempt_id().to_string()),
                super::super::receipt::StartDisposition::Rejected => {}
            }
        }
        drop(scheduling);
        receipt.accepted_attempt_ids.sort();
        receipt.uncertain_attempt_ids.sort();
        Ok(receipt)
    }

    pub(super) async fn activate_native_revision(
        &self,
        run_id: &str,
        revision_id: &str,
    ) -> Result<(), String> {
        let control = self
            .run_controls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(run_id.to_string())
            .or_insert_with(|| Arc::new(super::RunControl::default()))
            .clone();
        let scheduling = control.scheduling.lock().await;
        let mut admission = control
            .admission_revision
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if admission.id == revision_id && !admission.cancellation.is_cancelled() {
            return Ok(());
        }
        if !admission.id.is_empty() && !admission.cancellation.is_cancelled() {
            return Err("the prior workflow revision has not been fenced".into());
        }
        *admission = AdmissionRevision {
            id: revision_id.to_string(),
            cancellation: control.cancellation.child_token(),
        };
        drop(admission);
        drop(scheduling);
        Ok(())
    }

    pub(in crate::workflows) fn authorizing_thread_id(
        &self,
        run_id: &str,
    ) -> Result<ThreadId, String> {
        let path = self.origins_root.join(format!("{run_id}.origin"));
        if std::fs::metadata(&path)
            .map_err(|error| error.to_string())?
            .len()
            > 128
        {
            return Err("invalid workflow origin record".into());
        }
        ThreadId::from_string(&std::fs::read_to_string(path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())
    }

    pub(super) async fn cancel_native_prepared(
        &self,
        request: &AgentRequest,
    ) -> Result<WorkflowPreparedDisposition, String> {
        let control = self
            .run_controls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&request.run_id)
            .cloned()
            .ok_or("workflow native supervision is unavailable")?;
        let receipt = control
            .starts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&request.attempt_id)
            .cloned();
        if let Some(receipt) = &receipt {
            receipt.cancel_prepared();
        }
        let scheduling = control.scheduling.lock().await;
        let accepted = self
            .children
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(_, owner)| {
                owner.run_id == request.run_id && owner.attempt_id == request.attempt_id
            })
            .map(|(thread, _)| thread.to_string());
        if let Some(child_thread_id) = accepted {
            return Ok(WorkflowPreparedDisposition::Accepted { child_thread_id });
        }
        let disposition = match receipt {
            Some(receipt) => match receipt.disposition() {
                super::super::receipt::StartDisposition::Prepared => {
                    receipt.reject("workflow command cancelled a prepared native attempt")?;
                    WorkflowPreparedDisposition::KnownRejected
                }
                super::super::receipt::StartDisposition::Rejected => {
                    WorkflowPreparedDisposition::KnownRejected
                }
                super::super::receipt::StartDisposition::Uncertain
                | super::super::receipt::StartDisposition::Accepted => {
                    WorkflowPreparedDisposition::Uncertain
                }
            },
            // No local receipt is not proof of noncreation after a restart.
            None => WorkflowPreparedDisposition::Uncertain,
        };
        drop(scheduling);
        Ok(disposition)
    }

    async fn registered_control_identity(
        &self,
        request: &AgentRequest,
        expected_child: &str,
    ) -> Result<(Arc<Session>, ThreadId), String> {
        let owner = self.owner(request).await?;
        let child_id = ThreadId::from_string(expected_child).map_err(|error| error.to_string())?;
        let registered = self
            .children
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&child_id)
            .cloned()
            .ok_or("workflow attempt has no registered native child")?;
        if registered.run_id != request.run_id || registered.attempt_id != request.attempt_id {
            return Err("workflow attempt does not own the selected native child".into());
        }
        Ok((owner, child_id))
    }

    pub(super) async fn current_native_owned_turn(
        &self,
        request: &AgentRequest,
        expected_child: &str,
    ) -> Result<Option<String>, String> {
        let (owner, child_id) = self
            .registered_control_identity(request, expected_child)
            .await?;
        let child = match owner
            .services
            .local_agent_runtime
            .workflow_thread(child_id)
            .await
        {
            Ok(child) => child,
            Err(error)
                if matches!(
                    error.details(),
                    codex_protocol::error::CodexErrorDetails::ThreadNotFound(_)
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.to_string()),
        };
        let snapshot = child.session.workflow_usage_snapshot().await;
        if snapshot.restored || snapshot.ownership_retired || snapshot.terminal.is_some() {
            return Ok(None);
        }
        Ok(snapshot
            .active_turn
            .map(|turn| turn.turn_id)
            .filter(|turn| snapshot.owned_turns.contains(turn)))
    }

    async fn owned_control_turn(
        &self,
        request: &AgentRequest,
        expected_child: &str,
        expected_turn: &str,
    ) -> Result<(Arc<Session>, Arc<CodexThread>), String> {
        let (owner, child_id) = self
            .registered_control_identity(request, expected_child)
            .await?;
        let child = owner
            .services
            .local_agent_runtime
            .workflow_thread(child_id)
            .await
            .map_err(|error| error.to_string())?;
        let snapshot = child.session.workflow_usage_snapshot().await;
        if snapshot.restored
            || snapshot.ownership_retired
            || snapshot.terminal.is_some()
            || !snapshot
                .owned_turns
                .iter()
                .any(|turn| turn == expected_turn)
            || snapshot
                .active_turn
                .as_ref()
                .is_none_or(|turn| turn.turn_id != expected_turn)
        {
            return Err("the expected workflow-owned turn is no longer active".into());
        }
        Ok((owner, child))
    }

    pub(super) async fn guide_native_attempt(
        &self,
        request: &AgentRequest,
        expected_child: &str,
        expected_turn: &str,
        message: String,
    ) -> Result<(), String> {
        if message.trim().is_empty() || message.len() > 8192 {
            return Err("workflow guidance must contain 1..=8192 UTF-8 bytes".into());
        }
        let (_owner, child) = self
            .owned_control_turn(request, expected_child, expected_turn)
            .await?;
        match child
            .steer_turn(
                TurnInputRequest::user_input(vec![UserInput::Text {
                    text: message,
                    text_elements: Vec::new(),
                }])
                .from_agent(),
                expected_turn.to_string(),
            )
            .await
            .map_err(|error| error.to_string())?
        {
            SteerSubmission::Steered { .. } => Ok(()),
            SteerSubmission::NotSubmitted { reason } => {
                Err(format!("workflow guidance was not applied: {reason:?}"))
            }
        }
    }

    pub(super) async fn interrupt_native_attempt(
        &self,
        request: &AgentRequest,
        expected_child: &str,
        expected_turn: &str,
    ) -> Result<WorkflowInterruptReceipt, String> {
        let (_owner, child) = self
            .owned_control_turn(request, expected_child, expected_turn)
            .await?;
        let control = self
            .run_controls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&request.run_id)
            .cloned()
            .ok_or("workflow native supervision is unavailable")?;
        let settled = child
            .session
            .abort_workflow_turn_if_active(expected_turn)
            .await;
        if settled {
            let snapshot = child.session.workflow_usage_snapshot().await;
            let observer = control
                .observer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(observer) = observer {
                control.attempt_clocks.publish(
                    &observer,
                    &request.attempt_id,
                    snapshot.gate_active_duration_ms,
                    /*complete*/ true,
                )?;
            }
            super::super::usage_ownership::retire_selected(&control.usage_scopes, &snapshot);
            if let Some(observer) = control
                .observer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                && let Some(selected) = super::super::usage_ownership::cached(
                    &control.usage_scopes,
                    child.session.thread_id,
                )
            {
                super::super::usage::report_selection(
                    &observer,
                    &request.attempt_id,
                    child.session.thread_id,
                    selected,
                )?;
            }
            control.settlement_changed.notify_waiters();
        }
        Ok(WorkflowInterruptReceipt {
            child_thread_id: expected_child.to_string(),
            turn_id: expected_turn.to_string(),
            settled,
        })
    }
}
