//! Own-thread cumulative checkpoints are replaced, never summed as events.
use crate::WorkflowRun;
use crate::WorkflowStatus;
use crate::WorkflowTokenUsage;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Deserialize, Serialize, PartialEq)]
pub(super) struct UsageObservation {
    pub attempt_id: String,
    pub usage: Option<WorkflowTokenUsage>,
    pub complete: bool,
    #[serde(default)]
    pub model_turns: Option<u64>,
    #[serde(default)]
    pub model_turns_incomplete: bool,
}

pub(super) fn observe(
    run: &WorkflowRun,
    observations: &mut BTreeMap<String, UsageObservation>,
    attempt: &str,
    thread: &str,
    usage: Option<WorkflowTokenUsage>,
    complete: bool,
) -> Result<(), String> {
    if thread.is_empty() || thread.len() > 128 {
        return Err("invalid workflow usage thread".into());
    }
    if !run.steering.known_attempt_ids.contains(attempt)
        && !run
            .steps
            .values()
            .flat_map(|step| &step.attempts)
            .any(|candidate| candidate.id == attempt)
    {
        return Err("workflow usage attempt is unknown".into());
    }
    if !observations.contains_key(thread) && observations.len() >= crate::MAX_OBSERVED_THREADS {
        return Err("workflow usage contributor limit exceeded".into());
    }
    let observation = observations
        .entry(thread.to_string())
        .or_insert_with(|| UsageObservation {
            attempt_id: attempt.to_string(),
            usage: None,
            complete: false,
            model_turns: None,
            model_turns_incomplete: false,
        });
    if observation.attempt_id != attempt {
        return Err("workflow usage thread belongs to another attempt".into());
    }
    if let Some(usage) = usage {
        observation.usage = Some(match &observation.usage {
            None => usage,
            Some(prior) => WorkflowTokenUsage {
                input_tokens: prior.input_tokens.max(usage.input_tokens),
                output_tokens: prior.output_tokens.max(usage.output_tokens),
                cached_input_tokens: prior.cached_input_tokens.max(usage.cached_input_tokens),
                total_tokens: prior.total_tokens.max(usage.total_tokens),
            },
        });
    }
    observation.complete |= complete;
    Ok(())
}

pub(super) fn refresh(
    run: &mut WorkflowRun,
    observations: &BTreeMap<String, UsageObservation>,
    now: i64,
) {
    let reported = observations
        .values()
        .filter_map(|observation| observation.usage.as_ref())
        .collect::<Vec<_>>();
    let terminal = !matches!(
        run.status,
        WorkflowStatus::Pending | WorkflowStatus::Running
    );
    run.metrics.usage = if reported.is_empty()
        && !(terminal && run.accepted_starts == 0 && run.metrics.usage_incomplete_reason.is_none())
    {
        None
    } else {
        Some(
            reported
                .into_iter()
                .fold(WorkflowTokenUsage::default(), |mut total, usage| {
                    total.input_tokens = total.input_tokens.saturating_add(usage.input_tokens);
                    total.output_tokens = total.output_tokens.saturating_add(usage.output_tokens);
                    total.cached_input_tokens = total
                        .cached_input_tokens
                        .saturating_add(usage.cached_input_tokens);
                    total.total_tokens = total.total_tokens.saturating_add(usage.total_tokens);
                    total
                }),
        )
    };
    run.metrics.usage_complete = terminal
        && run.metrics.usage_incomplete_reason.is_none()
        && run.metrics.usage.is_some()
        && observations.len() >= run.accepted_starts as usize
        && observations
            .values()
            .all(|observation| observation.complete && observation.usage.is_some())
        && run
            .steps
            .values()
            .flat_map(|step| &step.attempts)
            .filter_map(|attempt| attempt.child_thread_id.as_ref())
            .all(|child| observations.contains_key(child));
    run.metrics.model_turns = if observations.is_empty() {
        (run.accepted_starts == 0 && run.metrics.model_turns == Some(0)).then_some(0)
    } else {
        model_turns(observations.values())
    };
    if run
        .steering
        .accepted_child_ids
        .iter()
        .any(|child| !observations.contains_key(child))
    {
        run.metrics.model_turns = None;
    }
    if run.metrics.agents_used.is_some() {
        run.metrics.agents_used = u32::try_from(run.steering.accepted_child_ids.len()).ok();
    }
    super::entity_metrics::refresh(run, observations);
    run.metrics.active_duration_ms = run.activity.checkpoint_at_ms.map(|_| {
        run.activity.active_duration_ms.saturating_add(
            run.activity
                .active_since_ms
                .map_or(0, |start| now.saturating_sub(start).max(0) as u64),
        )
    });
    if !terminal || run.metrics.wall_duration_ms.is_none() {
        run.metrics.wall_duration_ms = Some(
            now.saturating_sub(run.created_at.saturating_mul(1000))
                .max(0) as u64,
        );
    }
}

pub(super) fn model_turns<'a>(
    mut observations: impl Iterator<Item = &'a UsageObservation>,
) -> Option<u64> {
    observations.try_fold(0u64, |sum, observation| {
        if observation.model_turns_incomplete {
            return None;
        }
        observation
            .model_turns
            .and_then(|count| sum.checked_add(count))
    })
}

pub(super) fn set_active(run: &mut WorkflowRun, active: bool, now: i64) {
    if !active && run.activity.active_since_ms.is_none() {
        return;
    }
    if let Some(start) = run.activity.active_since_ms.take() {
        run.activity.active_duration_ms = run
            .activity
            .active_duration_ms
            .saturating_add(now.saturating_sub(start).max(0) as u64);
    }
    run.activity.active_since_ms = active.then_some(now);
    run.activity.checkpoint_at_ms = Some(now);
}

pub(super) fn checkpoint_clock(run: &mut WorkflowRun, now: i64) {
    let active = run.activity.active_since_ms.is_some();
    set_active(run, active, now);
}
pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
