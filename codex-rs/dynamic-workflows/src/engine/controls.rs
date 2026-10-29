//! Durable human commands are journaled before any exact native operation.
use super::WorkflowEngine;
use crate::AdmissionStatus;
use crate::AgentRequest;
use crate::WorkflowControlCommand;
use crate::WorkflowControlOperation;
use crate::WorkflowControlRequest;
use crate::WorkflowControlResponse;
use crate::WorkflowControlState;
use crate::WorkflowControlStatus;
use crate::WorkflowControlTarget;
use crate::WorkflowHost;
use crate::WorkflowPreparedDisposition;
use crate::WorkflowResult;
use crate::WorkflowRevisionStatus;
use crate::WorkflowRun;
use crate::WorkflowStepControlOutcome;
use crate::WorkflowStepDisposition;

struct Target {
    request: AgentRequest,
    child: Option<String>,
    turn: Option<String>,
    admission: AdmissionStatus,
    settled_failure: bool,
}

enum TargetError {
    Rejected(String),
    Blocked(String),
}
impl From<&str> for TargetError {
    fn from(value: &str) -> Self {
        Self::Rejected(value.to_owned())
    }
}
impl From<String> for TargetError {
    fn from(value: String) -> Self {
        Self::Rejected(value)
    }
}

impl<H: WorkflowHost> WorkflowEngine<H> {
    pub fn control_state(&self, id: &str) -> Result<WorkflowControlState, String> {
        Ok(self.get(id)?.steering.state)
    }

    pub fn list_controls(
        &self,
        id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkflowControlCommand>, String> {
        if !(1..=crate::MAX_PAGE_SIZE).contains(&limit) {
            return Err("workflow control limit must be between 1 and 50".into());
        }
        let mut commands = self.get(id)?.steering.commands;
        commands.sort_by(|a, b| (a.control_revision, &a.id).cmp(&(b.control_revision, &b.id)));
        Ok(commands.into_iter().skip(offset).take(limit).collect())
    }

    pub async fn control(
        &self,
        id: &str,
        request: WorkflowControlRequest,
    ) -> Result<WorkflowControlResponse, String> {
        request.validate()?;
        let (response, target) = self.store.update(id, |run| {
            crate::orchestration::require_executable(run)?;
            if run.steering.state.active_revision_id.is_empty() {
                let version = run.steering.version;
                let mut steering = crate::steering::WorkflowSteeringState::initialize(run)?;
                // An old suspended/running VM has no compiler receipts. Creating its
                // control identity must not silently enable instrumented replay.
                steering.version = version;
                run.steering = steering;
            }
            let repeated = run.steering.mutations.contains_key(&request.command_id);
            let response = run.steering.enqueue_control(&request)?;
            if repeated {
                return Ok((response, None));
            }
            if let WorkflowControlTarget::FutureStep { scope, key } = &request.target {
                if !matches!(run.status, crate::WorkflowStatus::Pending | crate::WorkflowStatus::Running) {
                    return Ok((run.steering.finish_control(&request.command_id, WorkflowControlStatus::Rejected, Some("this execution cannot discover future tasks; stage and resume a new revision".into()))?, None));
                }
                if run.steps.values().any(|step| &step.scope == scope && step.key.as_ref() == Some(key)) {
                    return Ok((run.steering.finish_control(&request.command_id, WorkflowControlStatus::Rejected, Some("the scoped task is already discovered; refresh its exact step and attempt".into()))?, None));
                }
                return Ok((response, None));
            }
            match resolve_target(run, &request) {
                Ok(target) => Ok((response, Some(target))),
                Err(error) => {
                    let (status, error) = match error { TargetError::Rejected(error) => (WorkflowControlStatus::Rejected, error), TargetError::Blocked(error) => (WorkflowControlStatus::Blocked, error) };
                    Ok((run.steering.finish_control(&request.command_id, status, Some(error))?, None))
                }
            }
        })?;
        let Some(target) = target else {
            return Ok(response);
        };
        let disposition = match &request.operation {
            WorkflowControlOperation::Guide { message } => {
                let outcome = self
                    .host
                    .guide_attempt(
                        target.request.clone(),
                        target
                            .child
                            .clone()
                            .ok_or("accepted workflow child missing")?,
                        target.turn.clone().ok_or("owned workflow turn missing")?,
                        message.clone(),
                    )
                    .await;
                return self.store.update(id, |run| match outcome {
                    Ok(()) => run.steering.finish_control(
                        &request.command_id,
                        WorkflowControlStatus::Applied,
                        None,
                    ),
                    Err(error) => run.steering.finish_control(
                        &request.command_id,
                        WorkflowControlStatus::Rejected,
                        Some(error),
                    ),
                });
            }
            WorkflowControlOperation::Interrupt => {
                let outcome = self
                    .host
                    .interrupt_attempt(
                        target.request.clone(),
                        target
                            .child
                            .clone()
                            .ok_or("accepted workflow child missing")?,
                        target.turn.clone().ok_or("owned workflow turn missing")?,
                    )
                    .await;
                return self.store.update(id, |run| match outcome {
                    Ok(receipt)
                        if receipt.settled
                            && Some(&receipt.child_thread_id) == target.child.as_ref()
                            && Some(&receipt.turn_id) == target.turn.as_ref() =>
                    {
                        record_settlement(
                            run,
                            &target.request,
                            &receipt.child_thread_id,
                            "workflow worker interrupted by control command",
                        )?;
                        run.steering.finish_control(
                            &request.command_id,
                            WorkflowControlStatus::Applied,
                            None,
                        )
                    }
                    Ok(_) => run.steering.finish_control(
                        &request.command_id,
                        WorkflowControlStatus::Rejected,
                        Some("the exact owned native turn did not settle".into()),
                    ),
                    Err(error) => run.steering.finish_control(
                        &request.command_id,
                        WorkflowControlStatus::Rejected,
                        Some(error),
                    ),
                });
            }
            WorkflowControlOperation::EditQueued { .. } | WorkflowControlOperation::Skip { .. } => {
                if target.admission == AdmissionStatus::Accepted {
                    if target.settled_failure {
                        WorkflowPreparedDisposition::Accepted {
                            child_thread_id: target
                                .child
                                .clone()
                                .ok_or("accepted workflow child missing")?,
                        }
                    } else {
                        let outcome = self
                            .host
                            .interrupt_attempt(
                                target.request.clone(),
                                target
                                    .child
                                    .clone()
                                    .ok_or("accepted workflow child missing")?,
                                target.turn.clone().ok_or("owned workflow turn missing")?,
                            )
                            .await;
                        match outcome {
                            Ok(receipt)
                                if receipt.settled
                                    && Some(&receipt.child_thread_id) == target.child.as_ref()
                                    && Some(&receipt.turn_id) == target.turn.as_ref() =>
                            {
                                WorkflowPreparedDisposition::Accepted {
                                    child_thread_id: receipt.child_thread_id,
                                }
                            }
                            Ok(_) => {
                                return self.reject_control(
                                    id,
                                    &request.command_id,
                                    WorkflowControlStatus::Rejected,
                                    "the exact owned native turn did not settle".into(),
                                );
                            }
                            Err(error) => {
                                return self.reject_control(
                                    id,
                                    &request.command_id,
                                    WorkflowControlStatus::Rejected,
                                    error,
                                );
                            }
                        }
                    }
                } else if target.admission == AdmissionStatus::Rejected
                    || target.admission == AdmissionStatus::NotRequested
                {
                    WorkflowPreparedDisposition::KnownRejected
                } else {
                    match self
                        .host
                        .cancel_prepared_attempt(target.request.clone())
                        .await
                    {
                        Ok(disposition) => disposition,
                        Err(error) => {
                            return self.reject_control(
                                id,
                                &request.command_id,
                                WorkflowControlStatus::Blocked,
                                error,
                            );
                        }
                    }
                }
            }
        };
        let disposition = match disposition {
            WorkflowPreparedDisposition::Accepted { child_thread_id }
                if target.admission != AdmissionStatus::Accepted =>
            {
                self.store.update(id, |run| {
                    crate::orchestration::record_acceptance(
                        run,
                        &target.request.attempt_id,
                        child_thread_id.clone(),
                    )
                })?;
                if !matches!(request.operation, WorkflowControlOperation::Skip { .. }) {
                    return self.reject_control(id, &request.command_id, WorkflowControlStatus::Rejected, "the queued attempt was already accepted; guide its exact owned turn instead".into());
                }
                let Some(turn) = target.turn.clone() else {
                    return self.reject_control(id, &request.command_id, WorkflowControlStatus::Rejected, "the queued attempt was already accepted; refresh its exact owned turn before interrupting it".into());
                };
                match self
                    .host
                    .interrupt_attempt(
                        target.request.clone(),
                        child_thread_id.clone(),
                        turn.clone(),
                    )
                    .await
                {
                    Ok(receipt)
                        if receipt.settled
                            && receipt.child_thread_id == child_thread_id
                            && receipt.turn_id == turn =>
                    {
                        WorkflowPreparedDisposition::Accepted { child_thread_id }
                    }
                    Ok(_) => {
                        return self.reject_control(
                            id,
                            &request.command_id,
                            WorkflowControlStatus::Rejected,
                            "the exact owned native turn did not settle".into(),
                        );
                    }
                    Err(error) => {
                        return self.reject_control(
                            id,
                            &request.command_id,
                            WorkflowControlStatus::Rejected,
                            error,
                        );
                    }
                }
            }
            disposition => disposition,
        };
        match disposition {
            WorkflowPreparedDisposition::Uncertain => self.reject_control(
                id,
                &request.command_id,
                WorkflowControlStatus::Blocked,
                "native acceptance is uncertain; reconcile the exact attempt before changing it"
                    .into(),
            ),
            WorkflowPreparedDisposition::KnownRejected
            | WorkflowPreparedDisposition::Accepted { .. } => self.store.update(id, |run| {
                if let WorkflowPreparedDisposition::Accepted { child_thread_id } = &disposition
                    && !target.settled_failure
                {
                    // Native settlement survives a superseded intent. Keep it in
                    // the same commit as an allowed replacement so observers see
                    // either Skipped or the real abort, never an intermediate failure.
                    record_settlement(
                        run,
                        &target.request,
                        child_thread_id,
                        "workflow worker stopped for skipped task",
                    )?;
                }
                if run.steering.state.active_revision_id != response.command.revision_id
                    || run.steering.state.status != WorkflowRevisionStatus::Active
                {
                    return run.steering.finish_control(
                        &request.command_id,
                        WorkflowControlStatus::Rejected,
                        Some("the workflow plan changed before the queued control settled".into()),
                    );
                }
                let Some(step) = run.steps.get_mut(&target.request.step_id) else {
                    return Err("controlled workflow step missing".into());
                };
                if step
                    .attempts
                    .last()
                    .is_none_or(|attempt| attempt.id != target.request.attempt_id)
                {
                    return run.steering.finish_control(
                        &request.command_id,
                        WorkflowControlStatus::Rejected,
                        Some("the workflow attempt changed before the control settled".into()),
                    );
                }
                if matches!(disposition, WorkflowPreparedDisposition::KnownRejected) {
                    if step
                        .attempts
                        .last()
                        .is_some_and(|attempt| attempt.admission == AdmissionStatus::Accepted)
                    {
                        return run.steering.finish_control(
                            &request.command_id,
                            WorkflowControlStatus::Blocked,
                            Some(
                                "native acceptance changed while rejecting the prepared attempt"
                                    .into(),
                            ),
                        );
                    }
                    if step
                        .attempts
                        .last()
                        .is_some_and(|attempt| attempt.admission != AdmissionStatus::Rejected)
                    {
                        crate::orchestration::record_rejection(
                            run,
                            &target.request.attempt_id,
                            "prepared attempt canceled by human control".into(),
                        )?;
                    }
                }
                if let Err(error) = apply_queued(run, &target.request.step_id, &response.command) {
                    return run.steering.finish_control(
                        &request.command_id,
                        WorkflowControlStatus::Rejected,
                        Some(error),
                    );
                }
                run.steering.finish_control(
                    &request.command_id,
                    WorkflowControlStatus::Applied,
                    None,
                )
            }),
        }
    }

    fn reject_control(
        &self,
        run_id: &str,
        command_id: &str,
        status: WorkflowControlStatus,
        error: String,
    ) -> Result<WorkflowControlResponse, String> {
        self.store.update(run_id, |run| {
            run.steering.finish_control(command_id, status, Some(error))
        })
    }
}

fn record_settlement(
    run: &mut WorkflowRun,
    request: &AgentRequest,
    child: &str,
    reason: &str,
) -> Result<(), String> {
    let attempt = run
        .steps
        .get_mut(&request.step_id)
        .and_then(|step| {
            step.attempts
                .iter_mut()
                .find(|attempt| attempt.id == request.attempt_id)
        })
        .ok_or("the settled workflow attempt is no longer available")?;
    if attempt.admission != AdmissionStatus::Accepted
        || attempt.child_thread_id.as_deref() != Some(child)
    {
        return Err("the settled native child does not match the accepted workflow attempt".into());
    }
    if !matches!(
        attempt.status,
        crate::WorkflowStatus::Failed | crate::WorkflowStatus::Succeeded
    ) {
        attempt.status = crate::WorkflowStatus::Failed;
        attempt.error = Some(reason.to_owned());
    }
    Ok(())
}

fn resolve_target(
    run: &WorkflowRun,
    command: &WorkflowControlRequest,
) -> Result<Target, TargetError> {
    let WorkflowControlTarget::Step {
        logical_id,
        expected_attempt_id,
        expected_turn_id,
    } = &command.target
    else {
        return Err("native controls require one exact discovered step".into());
    };
    let step = run
        .steps
        .values()
        .find(|step| &step.logical_id == logical_id)
        .ok_or("workflow step not found")?;
    if step.disposition == WorkflowStepDisposition::Skipped {
        return Err("workflow step was already skipped".into());
    }
    let attempt = step
        .attempts
        .last()
        .ok_or("workflow step has no prepared attempt; use its future scoped key")?;
    if expected_attempt_id.as_ref() != Some(&attempt.id) {
        return Err("workflow attempt changed; refresh the exact target".into());
    }
    if matches!(
        attempt.admission,
        AdmissionStatus::NotRequested | AdmissionStatus::Rejected
    ) && attempt.child_thread_id.is_some()
    {
        return Err(TargetError::Blocked("the attempt has a recorded child without a settled admission receipt; reconcile it before control".into()));
    }
    if run.status == crate::WorkflowStatus::Succeeded || step.result.is_some() {
        return Err("a completed workflow result cannot be changed; stage a new revision".into());
    }
    let terminal_failure = matches!(
        run.status,
        crate::WorkflowStatus::Failed
            | crate::WorkflowStatus::Stopped
            | crate::WorkflowStatus::Interrupted
    );
    let settled_failure = terminal_failure
        && matches!(command.operation, WorkflowControlOperation::Skip { .. })
        && attempt.status != crate::WorkflowStatus::Succeeded
        && attempt.admission == AdmissionStatus::Accepted
        && attempt.final_turn_id.is_some();
    if attempt.final_turn_id.is_some() && !settled_failure {
        return Err("workflow step already sealed its final owned turn".into());
    }
    if terminal_failure
        && matches!(command.operation, WorkflowControlOperation::Skip { .. })
        && !settled_failure
        && attempt.admission != AdmissionStatus::Rejected
    {
        return Err(TargetError::Blocked(
            "the failed attempt has no canonical settlement receipt; reconcile it before skipping"
                .into(),
        ));
    }
    if let WorkflowControlOperation::Skip {
        replacement: WorkflowResult::Value { value },
    } = &command.operation
        && let Some(schema) = step.options.get("schema")
    {
        crate::schema::validate_output(schema, value)?;
    }
    if let WorkflowControlOperation::EditQueued { prompt } = &command.operation {
        let mut options = step
            .effective_options
            .as_ref()
            .unwrap_or(&step.options)
            .clone();
        options["message"] = serde_json::Value::String(prompt.clone());
        crate::types::bounded_value(&options)?;
    }
    match &command.operation {
        WorkflowControlOperation::Guide { .. } | WorkflowControlOperation::Interrupt => {
            if attempt.admission != AdmissionStatus::Accepted
                || attempt.child_thread_id.is_none()
                || expected_turn_id.is_none()
            {
                return Err(
                    "guidance and interruption require an accepted child and exact owned turn"
                        .into(),
                );
            }
        }
        WorkflowControlOperation::EditQueued { .. }
            if attempt.admission == AdmissionStatus::Accepted =>
        {
            return Err(
                "an accepted agent's prompt cannot be replaced; guide its exact owned turn instead"
                    .into(),
            );
        }
        WorkflowControlOperation::Skip { .. }
            if attempt.admission == AdmissionStatus::Accepted
                && (attempt.child_thread_id.is_none()
                    || (!settled_failure && expected_turn_id.is_none())) =>
        {
            return Err("skipping an accepted agent requires its exact owned turn".into());
        }
        WorkflowControlOperation::EditQueued { .. } | WorkflowControlOperation::Skip { .. } => {}
    }
    Ok(Target {
        request: request(run, step, attempt)?,
        child: attempt.child_thread_id.clone(),
        turn: expected_turn_id.clone(),
        admission: attempt.admission,
        settled_failure,
    })
}

pub(super) fn request(
    run: &WorkflowRun,
    step: &crate::WorkflowStep,
    attempt: &crate::WorkflowAttempt,
) -> Result<AgentRequest, String> {
    Ok(AgentRequest {
        run_id: run.id.clone(),
        revision_id: run.steering.state.active_revision_id.clone(),
        run_sequence: run.worker_sequence,
        owner_thread_id: run.owner_thread_id.clone(),
        step_id: step.id.clone(),
        label: step.label.clone(),
        validation_feedback: attempt.validation_feedback.clone(),
        step_sequence: step.worker_sequence,
        attempt_id: attempt.id.clone(),
        attempt_number: step
            .attempts
            .len()
            .try_into()
            .map_err(|_| "workflow attempt count exceeds limit")?,
        options: step
            .effective_options
            .as_ref()
            .unwrap_or(&step.options)
            .clone(),
    })
}

fn apply_queued(
    run: &mut WorkflowRun,
    step_id: &str,
    command: &WorkflowControlCommand,
) -> Result<(), String> {
    let step = run
        .steps
        .get_mut(step_id)
        .ok_or("controlled workflow step missing")?;
    if step.result.is_some() || step.disposition == WorkflowStepDisposition::Skipped {
        return Err("the logical task already has a final result".into());
    }
    match &command.operation {
        WorkflowControlOperation::EditQueued { prompt } => {
            let mut options = step
                .effective_options
                .as_ref()
                .unwrap_or(&step.options)
                .clone();
            options["message"] = serde_json::Value::String(prompt.clone());
            crate::types::bounded_value(&options)?;
            step.effective_options = Some(options);
        }
        WorkflowControlOperation::Skip { replacement } => {
            let value = match replacement {
                WorkflowResult::Absent => serde_json::Value::Null,
                WorkflowResult::Value { value } => {
                    if let Some(schema) = step.options.get("schema") {
                        crate::schema::validate_output(schema, value)?;
                    }
                    value.clone()
                }
            };
            step.disposition = WorkflowStepDisposition::Skipped;
            step.control_outcome = Some(WorkflowStepControlOutcome {
                command_id: command.id.clone(),
                revision_id: command.revision_id.clone(),
                replacement: replacement.clone(),
            });
            step.result = Some(value);
        }
        WorkflowControlOperation::Guide { .. } | WorkflowControlOperation::Interrupt => {
            return Err("this control is not a queued operation".into());
        }
    }
    Ok(())
}
