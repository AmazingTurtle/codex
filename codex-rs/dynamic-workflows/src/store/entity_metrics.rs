//! Entity projections reuse the contributor ledger; snapshots and retries never add observations.
use super::metrics::UsageObservation;
use crate::AdmissionStatus;
use crate::PhaseStatus;
use crate::WorkflowMetrics;
use crate::WorkflowRun;
use crate::WorkflowStatus;
use crate::WorkflowStep;
use crate::WorkflowTokenUsage;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

pub(super) enum ChangeSource {
    Transition,
    Observation,
}

fn terminal(status: WorkflowStatus) -> bool {
    matches!(
        status,
        WorkflowStatus::Succeeded | WorkflowStatus::Failed | WorkflowStatus::Stopped
    )
}

fn settled(run: &WorkflowRun) -> bool {
    run.lifecycle.terminal_ready
        && !run.lifecycle.interrupted_unsettled
        && matches!(
            run.status,
            WorkflowStatus::Succeeded | WorkflowStatus::Failed | WorkflowStatus::Stopped
        )
}

fn sum_usage<'a>(values: impl Iterator<Item = &'a WorkflowTokenUsage>) -> WorkflowTokenUsage {
    values.fold(WorkflowTokenUsage::default(), |mut sum, value| {
        sum.input_tokens = sum.input_tokens.saturating_add(value.input_tokens);
        sum.output_tokens = sum.output_tokens.saturating_add(value.output_tokens);
        sum.cached_input_tokens = sum
            .cached_input_tokens
            .saturating_add(value.cached_input_tokens);
        sum.total_tokens = sum.total_tokens.saturating_add(value.total_tokens);
        sum
    })
}

fn rollup<'a>(
    metrics: impl Iterator<Item = &'a WorkflowMetrics>,
    known_empty: bool,
) -> WorkflowMetrics {
    let metrics: Vec<_> = metrics.collect();
    let usage = metrics
        .iter()
        .filter_map(|metric| metric.usage.as_ref())
        .collect::<Vec<_>>();
    WorkflowMetrics {
        usage: (!usage.is_empty() || known_empty).then(|| sum_usage(usage.into_iter())),
        usage_complete: (known_empty || !metrics.is_empty())
            && metrics.iter().all(|metric| metric.usage_complete),
        usage_incomplete_reason: metrics
            .iter()
            .find_map(|metric| metric.usage_incomplete_reason.clone()),
        model_turns: if metrics.is_empty() && !known_empty {
            None
        } else {
            metrics
                .iter()
                .try_fold(0u64, |sum, metric| sum.checked_add(metric.model_turns?))
        },
        agents_used: if metrics.is_empty() && !known_empty {
            None
        } else {
            metrics
                .iter()
                .try_fold(0u32, |sum, metric| sum.checked_add(metric.agents_used?))
        },
        active_duration_ms: None,
        wall_duration_ms: None,
    }
}

pub(super) fn refresh(run: &mut WorkflowRun, observations: &BTreeMap<String, UsageObservation>) {
    let run_settled = settled(run);
    let mut by_attempt: BTreeMap<&str, Vec<&UsageObservation>> = BTreeMap::new();
    for observation in observations.values() {
        by_attempt
            .entry(observation.attempt_id.as_str())
            .or_default()
            .push(observation);
    }
    for step in run.steps.values_mut() {
        for attempt in &mut step.attempts {
            if attempt.admission != AdmissionStatus::Accepted {
                continue;
            }
            let entries = by_attempt
                .get(attempt.id.as_str())
                .map(Vec::as_slice)
                .unwrap_or_default();
            let direct = attempt
                .child_thread_id
                .as_ref()
                .is_some_and(|child| observations.contains_key(child));
            let values = entries
                .iter()
                .filter_map(|entry| entry.usage.as_ref())
                .collect::<Vec<_>>();
            attempt.metrics.usage = (!values.is_empty()).then(|| sum_usage(values.into_iter()));
            attempt.metrics.model_turns = direct
                .then(|| super::metrics::model_turns(entries.iter().copied()))
                .flatten();
            attempt.metrics.agents_used = attempt.child_thread_id.as_ref().map(|_| 1);
            attempt.metrics.usage_complete = (terminal(attempt.status) || run_settled)
                && direct
                && attempt.metrics.usage_incomplete_reason.is_none()
                && entries
                    .iter()
                    .all(|entry| entry.complete && entry.usage.is_some());
        }
        let accepted = step
            .attempts
            .iter()
            .filter(|attempt| attempt.admission == AdmissionStatus::Accepted)
            .collect::<Vec<_>>();
        let known_empty = accepted.is_empty()
            && step
                .attempts
                .iter()
                .all(|attempt| attempt.admission != AdmissionStatus::Uncertain)
            && (step.updated_at.is_some()
                || step.disposition == crate::WorkflowStepDisposition::Skipped);
        let mut metrics = rollup(accepted.iter().map(|attempt| &attempt.metrics), known_empty);
        let agents = accepted
            .iter()
            .filter_map(|attempt| attempt.child_thread_id.as_ref())
            .collect::<BTreeSet<_>>();
        metrics.agents_used = if accepted.is_empty() && !known_empty {
            None
        } else {
            u32::try_from(agents.len()).ok()
        };
        metrics.active_duration_ms = if accepted.is_empty() {
            None
        } else {
            accepted.iter().try_fold(0u64, |sum, attempt| {
                sum.checked_add(attempt.metrics.active_duration_ms?)
            })
        };
        step.metrics = metrics;
    }
    for phase in &mut run.phase_progress {
        let steps = run
            .steps
            .values()
            .filter(|step| step.phase.as_deref() == Some(phase.title.as_str()))
            .collect::<Vec<_>>();
        let known_empty = steps.is_empty() && phase.updated_at.is_some();
        phase.metrics = rollup(steps.iter().map(|step| &step.metrics), known_empty);
        let agents = steps
            .iter()
            .flat_map(|step| &step.attempts)
            .filter(|attempt| attempt.admission == AdmissionStatus::Accepted)
            .filter_map(|attempt| attempt.child_thread_id.as_ref())
            .collect::<BTreeSet<_>>();
        if !steps.is_empty() || known_empty {
            phase.metrics.agents_used = u32::try_from(agents.len()).ok();
        }
        // Phase work can overlap and be revisited. No invented sum of child clocks.
        phase.metrics.active_duration_ms = None;
    }
}

fn step_terminal(step: &WorkflowStep, run: &WorkflowRun) -> bool {
    step.result.is_some()
        || step.disposition == crate::WorkflowStepDisposition::Skipped
        || (settled(run) && !step.attempts.is_empty())
        || step.attempts.last().is_some_and(|attempt| {
            terminal(attempt.status)
                && (attempt.schema_error.is_none()
                    || step.schema_attempts >= 5
                    || run.lifecycle.terminal_ready)
        })
}

/// Only actual entity changes stamp activity. ACKs, reads and unrelated sibling activity do not.
pub(super) fn stamp(previous: &WorkflowRun, next: &mut WorkflowRun, at: i64, source: ChangeSource) {
    let observed = matches!(source, ChangeSource::Observation);
    let now_settled = settled(next);
    let before_settled = settled(previous);
    let terminal_steps = next
        .steps
        .iter()
        .filter(|(_, step)| step_terminal(step, next))
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    for (id, step) in &mut next.steps {
        let before = previous.steps.get(id);
        for attempt in &mut step.attempts {
            let old = before.and_then(|step| step.attempts.iter().find(|old| old.id == attempt.id));
            let changed = old.is_none_or(|old| {
                attempt_changed(old, attempt)
                    || ((observed || old.updated_at.is_some()) && old.metrics != attempt.metrics)
            });
            let attempt_settled = terminal(attempt.status)
                || (now_settled && attempt.admission == AdmissionStatus::Accepted);
            let was_settled = old.is_some_and(|old| {
                terminal(old.status)
                    || (before_settled && old.admission == AdmissionStatus::Accepted)
            });
            if attempt_settled {
                if !was_settled && (changed || now_settled != before_settled) {
                    attempt.completed_at = Some(at);
                    attempt.updated_at = Some(at);
                }
            } else {
                attempt.completed_at = None;
                if changed {
                    attempt.updated_at = Some(at);
                }
            }
        }
        if terminal_steps.contains(id) {
            let controlled_skip =
                before.is_some_and(|old| old.control_outcome != step.control_outcome);
            if (controlled_skip || before.is_none_or(|old| !step_terminal(old, previous)))
                && before != Some(step)
            {
                step.completed_at = Some(at);
                step.updated_at = Some(at);
            }
        } else {
            step.completed_at = None;
            if before.is_none_or(|old| {
                step_changed(old, step)
                    || ((observed || old.updated_at.is_some()) && old.metrics != step.metrics)
            }) {
                step.updated_at = Some(at);
            }
        }
    }
    for phase in &mut next.phase_progress {
        let old = previous
            .phase_progress
            .iter()
            .find(|old| old.title == phase.title);
        let changed = old.is_none_or(|old| {
            (old.status, old.steps_discovered, old.steps_completed)
                != (phase.status, phase.steps_discovered, phase.steps_completed)
                || ((observed || old.updated_at.is_some()) && old.metrics != phase.metrics)
        });
        let phase_terminal = matches!(
            phase.status,
            PhaseStatus::Completed | PhaseStatus::Skipped | PhaseStatus::Failed
        ) || (now_settled && phase.status == PhaseStatus::Active);
        let was_terminal = old.is_some_and(|old| {
            matches!(
                old.status,
                PhaseStatus::Completed | PhaseStatus::Skipped | PhaseStatus::Failed
            ) || (before_settled && old.status == PhaseStatus::Active)
        });
        if phase_terminal {
            if !was_terminal && (changed || now_settled != before_settled) {
                phase.completed_at = Some(at);
                phase.updated_at = Some(at);
            }
        } else {
            phase.completed_at = None;
            if changed {
                phase.updated_at = Some(at);
            }
        }
    }
}

fn attempt_changed(old: &crate::WorkflowAttempt, next: &crate::WorkflowAttempt) -> bool {
    (
        old.status,
        old.admission,
        &old.child_thread_id,
        &old.owned_turn_ids,
        &old.final_turn_id,
        &old.resolved_model,
        &old.reasoning_effort,
        &old.error,
        &old.schema_error,
        &old.validation_feedback,
        &old.usage,
    ) != (
        next.status,
        next.admission,
        &next.child_thread_id,
        &next.owned_turn_ids,
        &next.final_turn_id,
        &next.resolved_model,
        &next.reasoning_effort,
        &next.error,
        &next.schema_error,
        &next.validation_feedback,
        &next.usage,
    )
}

fn step_changed(old: &WorkflowStep, next: &WorkflowStep) -> bool {
    (
        &old.options,
        &old.effective_options,
        old.disposition,
        &old.control_outcome,
        &old.result,
        old.schema_attempts,
    ) != (
        &next.options,
        &next.effective_options,
        next.disposition,
        &next.control_outcome,
        &next.result,
        next.schema_attempts,
    ) || old.attempts.len() != next.attempts.len()
        || old
            .attempts
            .iter()
            .zip(&next.attempts)
            .any(|(old, next)| old.id != next.id || attempt_changed(old, next))
}
