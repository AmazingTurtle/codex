//! Hydrated controls retain their caller and revision; presentation never invents target identity.
use super::model::*;
use codex_app_server_protocol::WorkflowAdmissionStatus;
use codex_app_server_protocol::WorkflowControlActiveTarget;
use codex_app_server_protocol::WorkflowControlState;
use codex_app_server_protocol::WorkflowRevisionStatus;
use codex_app_server_protocol::WorkflowRun;
use codex_app_server_protocol::WorkflowStatus;
use codex_app_server_protocol::WorkflowStepDisposition;

pub(crate) fn context(
    mut scope: ActionScope,
    run: WorkflowRun,
    state: WorkflowControlState,
    active_targets: &[WorkflowControlActiveTarget],
) -> Result<ActionSnapshot, String> {
    if run.id != scope.workflow_id
        || run.owner_thread_id != scope.owner.to_string()
        || run.execution_id != scope.execution_id
    {
        return Err(
            "The selected workflow changed. Reopen Actions to inspect its current execution."
                .into(),
        );
    }
    scope.expected_control_revision = state.control_revision;
    if let ActionTarget::Step {
        logical_id,
        expected_attempt_id: Some(attempt),
        expected_turn_id,
    } = &mut scope.target
    {
        *expected_turn_id = active_targets
            .iter()
            .find(|active| active.logical_id == *logical_id && active.attempt_id == *attempt)
            .map(|active| active.turn_id.clone());
    }
    let step = match &scope.target {
        ActionTarget::Step { logical_id, .. } => run
            .steps
            .values()
            .find(|step| step.logical_id == *logical_id),
        _ => None,
    };
    let latest = step.and_then(|step| step.attempts.last());
    let terminal = matches!(
        run.status,
        WorkflowStatus::Succeeded
            | WorkflowStatus::Failed
            | WorkflowStatus::Stopped
            | WorkflowStatus::Interrupted
    );
    let exact_active = match (&scope.target, latest) {
        (
            ActionTarget::Step {
                expected_attempt_id: Some(attempt),
                expected_turn_id: Some(turn),
                ..
            },
            Some(latest),
        ) => {
            latest.id == *attempt
                && latest.status == WorkflowStatus::Running
                && latest.owned_turn_ids.contains(turn)
                && latest.final_turn_id.as_ref() != Some(turn)
        }
        _ => false,
    };
    let queued = step.is_some_and(|step| step.attempts.is_empty())
        || latest.is_some_and(|attempt| {
            matches!(
                attempt.status,
                WorkflowStatus::Pending | WorkflowStatus::Running
            ) && matches!(
                attempt.admission,
                WorkflowAdmissionStatus::NotRequested | WorkflowAdmissionStatus::Reserved
            ) && attempt.child_thread_id.is_none()
        });
    let current_attempt = match (&scope.target, latest) {
        (
            ActionTarget::Step {
                expected_attempt_id,
                ..
            },
            latest,
        ) => expected_attempt_id.as_deref() == latest.map(|attempt| attempt.id.as_str()),
        _ => false,
    };
    let exact_queued = current_attempt
        && queued
        && step.is_none_or(|step| step.disposition != WorkflowStepDisposition::Skipped);
    let settled_skip = current_attempt
        && matches!(
            run.status,
            WorkflowStatus::Failed | WorkflowStatus::Stopped | WorkflowStatus::Interrupted
        )
        && step.is_some_and(|step| {
            step.disposition != WorkflowStepDisposition::Skipped && step.result.is_none()
        })
        && latest.is_some_and(|attempt| {
            attempt.status != WorkflowStatus::Succeeded
                && (attempt.admission == WorkflowAdmissionStatus::Rejected
                    || (attempt.admission == WorkflowAdmissionStatus::Accepted
                        && attempt.final_turn_id.is_some()))
        });
    let blocked = matches!(
        state.status,
        WorkflowRevisionStatus::Fencing
            | WorkflowRevisionStatus::Draining
            | WorkflowRevisionStatus::Superseded
    );
    let unavailable = |action| {
        if run.read_only && !matches!(action, Action::ShowResult | Action::Revisions) {
            return Some("Legacy workflows are read-only".into());
        }
        if blocked && !matches!(action, Action::ShowResult | Action::Revisions) {
            return Some("Wait for the current revision transition to finish".into());
        }
        match action {
            Action::Guide | Action::EditQueued | Action::Skip | Action::Interrupt
                if state.status != WorkflowRevisionStatus::Active =>
            {
                Some("Resume the ready revision before changing a step".into())
            }
            Action::Guide | Action::Interrupt if !exact_active => {
                Some("Select the current active step with a known owned turn".into())
            }
            Action::EditQueued if !exact_queued => {
                Some("Select a queued step that has not started".into())
            }
            Action::Skip if !exact_queued && !exact_active && !settled_skip => {
                Some("Select a queued step, an exact active turn, or a settled failed step".into())
            }
            Action::ApplyDrain | Action::ApplyInterrupt if state.pending_revision_id.is_some() => {
                Some("A revision is already pending; resume it when ready".into())
            }
            Action::ApplyDrain | Action::ApplyInterrupt => {
                Some("Stage a revision before applying it".into())
            }
            Action::Resume
                if state.status != WorkflowRevisionStatus::Ready
                    || state.pending_revision_id.is_none() =>
            {
                Some("A reconciled revision must be ready before resuming".into())
            }
            Action::ShowResult if !terminal => {
                Some("The workflow has not produced a terminal result".into())
            }
            _ => None,
        }
    };
    let availability = [
        Action::Guide,
        Action::EditQueued,
        Action::Skip,
        Action::Interrupt,
        Action::Revise,
        Action::ApplyDrain,
        Action::ApplyInterrupt,
        Action::Resume,
        Action::ShowResult,
        Action::Revisions,
    ]
    .into_iter()
    .map(|action| ActionAvailability {
        action,
        unavailable_reason: unavailable(action),
    })
    .collect();
    let queued_prompt = step
        .and_then(|step| {
            let options = step.effective_options.as_ref().unwrap_or(&step.options);
            options.get("message").or_else(|| options.get("prompt"))
        })
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let budget_summary = format!(
        "{} / {} agent starts{}",
        run.accepted_starts,
        run.limits.max_agents,
        run.limits
            .max_tokens
            .map_or_else(String::new, |limit| format!(
                " · {} token limit",
                crate::status::format_tokens_compact(i64::try_from(limit).unwrap_or(i64::MAX))
            ))
    );
    Ok(ActionSnapshot {
        footer_hidden: false,
        scope,
        title: run.metadata.as_ref().map_or_else(
            || "Legacy workflow".into(),
            |metadata| metadata.name.clone(),
        ),
        availability,
        queued_prompt,
        source: run.source,
        input: run.input,
        staged_revision_id: state.pending_revision_id,
        active_revision_id: Some(state.active_revision_id),
        budget_summary,
    })
}
