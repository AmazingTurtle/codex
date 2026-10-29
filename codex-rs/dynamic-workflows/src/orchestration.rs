//! Shared durable admission, phase and immutable-source invariants.
use serde_json::Value;
use serde_json::json;

use crate::AdmissionStatus;
use crate::PhaseStatus;
use crate::WORKFLOW_FORMAT_VERSION;
use crate::WorkflowRun;
use crate::WorkflowStatus;

pub(crate) fn require_executable(run: &WorkflowRun) -> Result<(), String> {
    if run.format_version != WORKFLOW_FORMAT_VERSION {
        return Err("legacy workflows are read-only; create a new V2 workflow to execute".into());
    }
    if run.source_hash != source_hash(&run.source) {
        return Err(
            "workflow source changed; start a new run instead of reusing cached results".into(),
        );
    }
    Ok(())
}

pub(crate) fn source_hash(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

pub(crate) fn validate_effect_execution(
    run: &WorkflowRun,
    execution: &str,
    revision: &str,
) -> Result<(), String> {
    if run.status != WorkflowStatus::Running
        || run.execution_id != execution
        || run.steering.state.status != crate::WorkflowRevisionStatus::Active
        || ((!revision.is_empty() || run.steering.version == 1)
            && run.steering.state.active_revision_id != revision)
    {
        return Err("workflow revision is fenced against effects".into());
    }
    Ok(())
}

pub(crate) fn budget(run: &WorkflowRun) -> Value {
    json!({"total":run.limits.max_tokens,"spent":run.metrics.usage.as_ref().map_or(0, |usage|usage.total_tokens),
        "usageComplete":run.metrics.usage_complete,"agentLimit":run.limits.max_agents,"acceptedStarts":run.accepted_starts})
}

pub(crate) fn reserve_start(run: &WorkflowRun) -> Result<(), String> {
    if run.steering.state.status != crate::WorkflowRevisionStatus::Active
        || (run.steering.version == 1 && !run.steering.suffix_open)
    {
        return Err("workflow revision is fenced against fresh admission".into());
    }
    validate_token_admission(run)?;
    let reserved = run
        .steps
        .values()
        .flat_map(|step| &step.attempts)
        .filter(|attempt| {
            matches!(
                attempt.admission,
                AdmissionStatus::Reserved | AdmissionStatus::Uncertain
            )
        })
        .count();
    if run.accepted_starts as usize + reserved >= run.limits.max_agents {
        return Err(format!(
            "workflow agent-start limit exhausted ({})",
            run.limits.max_agents
        ));
    }
    Ok(())
}

fn validate_token_admission(run: &WorkflowRun) -> Result<(), String> {
    if run.limits.max_tokens.is_some()
        && let Some(reason) = &run.metrics.usage_incomplete_reason
    {
        return Err(format!(
            "workflow token accounting is incomplete; cannot start another agent: {reason}"
        ));
    }
    if let Some(limit) = run.limits.max_tokens
        && run
            .metrics
            .usage
            .as_ref()
            .is_some_and(|usage| usage.total_tokens >= limit)
    {
        return Err(format!("workflow token budget exhausted ({limit})"));
    }
    Ok(())
}

pub(crate) fn validate_native_admission(run: &WorkflowRun, attempt_id: &str) -> Result<(), String> {
    if run.status != WorkflowStatus::Running {
        return Err("workflow is stopped".into());
    }
    let attempt = run
        .steps
        .values()
        .flat_map(|step| &step.attempts)
        .find(|attempt| attempt.id == attempt_id)
        .ok_or("workflow admission identity missing")?;
    if attempt.admission != AdmissionStatus::Reserved {
        return Err("workflow native start has no fresh reservation".into());
    }
    if run.steering.state.status != crate::WorkflowRevisionStatus::Active
        || (run.steering.version == 1 && !run.steering.suffix_open)
    {
        return Err("workflow revision is fenced against fresh admission".into());
    }
    if run.steering.commands.iter().any(|command| command.status == crate::WorkflowControlStatus::Pending && matches!(&command.target, crate::WorkflowControlTarget::Step {logical_id,..} if run.steps.values().any(|step|step.logical_id==*logical_id && step.attempts.iter().any(|attempt|attempt.id==attempt_id))) && matches!(command.operation,crate::WorkflowControlOperation::Skip {..}|crate::WorkflowControlOperation::EditQueued {..})) {return Err("workflow native attempt is fenced by a queued control".into());}
    validate_token_admission(run)
}

pub(crate) fn record_acceptance(
    run: &mut WorkflowRun,
    attempt_id: &str,
    child: String,
) -> Result<(), String> {
    run.steering
        .known_attempt_ids
        .insert(attempt_id.to_string());
    let attempt = run
        .steps
        .values_mut()
        .flat_map(|step| &mut step.attempts)
        .find(|attempt| attempt.id == attempt_id)
        .ok_or("workflow acceptance identity missing")?;
    if attempt
        .child_thread_id
        .as_ref()
        .is_some_and(|existing| existing != &child)
    {
        return Err("inconsistent workflow accepted child identity".into());
    }
    if attempt.admission != AdmissionStatus::Accepted {
        run.accepted_starts += 1;
        attempt.admission = AdmissionStatus::Accepted;
    }
    run.steering.accepted_child_ids.insert(child.clone());
    attempt.child_thread_id = Some(child);
    Ok(())
}

pub(crate) fn record_rejection(
    run: &mut WorkflowRun,
    attempt_id: &str,
    reason: String,
) -> Result<(), String> {
    let attempt = run
        .steps
        .values_mut()
        .flat_map(|step| &mut step.attempts)
        .find(|attempt| attempt.id == attempt_id)
        .ok_or("workflow rejection identity missing")?;
    if attempt.admission != AdmissionStatus::Accepted && attempt.child_thread_id.is_none() {
        attempt.admission = AdmissionStatus::Rejected;
        attempt.status = WorkflowStatus::Failed;
        attempt.error = Some(reason);
    }
    Ok(())
}

pub(crate) fn refresh_phases(run: &mut WorkflowRun) {
    for progress in &mut run.phase_progress {
        let steps: Vec<_> = run
            .steps
            .values()
            .filter(|step| step.phase.as_deref() == Some(progress.title.as_str()))
            .collect();
        progress.steps_discovered = steps.len() as u32;
        progress.steps_completed = steps
            .iter()
            .filter(|step| {
                step.result.is_some()
                    || step
                        .attempts
                        .last()
                        .is_some_and(|attempt| attempt.status == WorkflowStatus::Succeeded)
            })
            .count() as u32;
        let unfinished = steps
            .iter()
            .filter(|step| step.disposition != crate::WorkflowStepDisposition::Skipped)
            .any(|step| {
                step.attempts.last().is_some_and(|attempt| {
                    matches!(
                        attempt.status,
                        WorkflowStatus::Running | WorkflowStatus::Interrupted
                    )
                })
            });
        if run.status == WorkflowStatus::Succeeded {
            progress.status = if steps.is_empty() && progress.status == PhaseStatus::Pending {
                PhaseStatus::Skipped
            } else {
                PhaseStatus::Completed
            };
        } else if run.status == WorkflowStatus::Failed && progress.status == PhaseStatus::Active {
            progress.status = PhaseStatus::Failed;
        } else if progress.status == PhaseStatus::Active
            && run.phase.as_deref() != Some(progress.title.as_str())
            && !unfinished
        {
            progress.status = PhaseStatus::Completed;
        }
    }
}
