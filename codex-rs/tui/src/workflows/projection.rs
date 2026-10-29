//! Typed API snapshots become bounded terminal rows without inventing progress totals.
use super::WorkflowChild;
use super::WorkflowOverview;
use super::WorkflowRow;
use codex_app_server_protocol::WorkflowPhaseStatus;
use codex_app_server_protocol::WorkflowRun;
use codex_app_server_protocol::WorkflowStatus;
use codex_app_server_protocol::WorkflowStep;
use codex_app_server_protocol::WorkflowStepDisposition;
use codex_protocol::ThreadId;
#[path = "projection_details.rs"]
mod details;

pub(super) fn status(status: WorkflowStatus) -> &'static str {
    match status {
        WorkflowStatus::Pending => "Pending",
        WorkflowStatus::Running => "Running",
        WorkflowStatus::Succeeded => "Completed",
        WorkflowStatus::Failed => "Failed",
        WorkflowStatus::Stopped => "Stopped",
        WorkflowStatus::Interrupted => "Interrupted",
    }
}

pub(super) fn phase_status(status: WorkflowPhaseStatus) -> &'static str {
    match status {
        WorkflowPhaseStatus::Pending => "Planned",
        WorkflowPhaseStatus::Active => "Active",
        WorkflowPhaseStatus::Completed => "Completed",
        WorkflowPhaseStatus::Skipped => "Skipped",
        WorkflowPhaseStatus::Failed => "Failed",
    }
}

fn step_status(step: &WorkflowStep) -> &'static str {
    if step.disposition == WorkflowStepDisposition::Skipped {
        "Skipped"
    } else if step.result.is_some()
        || step
            .attempts
            .iter()
            .any(|attempt| attempt.status == WorkflowStatus::Succeeded)
    {
        "Completed"
    } else {
        step.attempts
            .last()
            .map_or("Discovered", |attempt| status(attempt.status))
    }
}

pub(crate) fn project(run: WorkflowRun) -> WorkflowRow {
    let title = run.metadata.as_ref().map_or_else(
        || "Legacy workflow".into(),
        |metadata| metadata.name.clone(),
    );
    let mut children = Vec::new();
    let agents_used = run.metrics.agents_used.map(|count| count as usize);
    let completed = run
        .steps
        .values()
        .filter(|step| {
            step.disposition == WorkflowStepDisposition::Skipped
                || step.result.is_some()
                || step
                    .attempts
                    .iter()
                    .any(|attempt| attempt.status == WorkflowStatus::Succeeded)
        })
        .count();
    let phases = run.metadata.as_ref().map_or_else(
        || run.phases.clone(),
        |metadata| {
            metadata
                .phases
                .iter()
                .map(|phase| phase.title.clone())
                .collect::<Vec<_>>()
        },
    );
    for phase in phases
        .iter()
        .map(|phase| Some(phase.as_str()))
        .chain(std::iter::once(None))
    {
        let steps: Vec<_> = run
            .steps
            .values()
            .filter(|step| {
                if phase.is_none() {
                    step.phase
                        .as_ref()
                        .is_none_or(|phase| !phases.contains(phase))
                } else {
                    step.phase.as_deref() == phase
                }
            })
            .collect();
        if phase.is_none() && steps.is_empty() {
            continue;
        }
        let show_phase = phase.is_some();
        if show_phase && let Some(label) = phase {
            let progress = run
                .phase_progress
                .iter()
                .find(|progress| progress.title == label);
            let phase_state = progress
                .map(|progress| phase_status(progress.status))
                .unwrap_or("Unknown");
            children.push(WorkflowChild {
                key: format!("phase:{phase:?}"),
                action_target: Some(super::actions::ActionTarget::Phase {
                    title: label.into(),
                }),
                transcript_target: Some(
                    codex_app_server_protocol::WorkflowTranscriptTarget::Phase {
                        phase: label.into(),
                    },
                ),
                label: format!("{label} · {phase_state}"),
                depth: 1,
                updated_at: progress
                    .and_then(|progress| progress.completed_at.or(progress.updated_at)),
                overview: Some(WorkflowOverview {
                    title: label.into(),
                    metrics: progress.map(|progress| progress.metrics.clone()),
                    metadata: vec![
                        ("Status".into(), phase_state.into()),
                        (
                            "Steps".into(),
                            progress.map_or_else(
                                || format!("{} discovered", steps.len()),
                                |progress| {
                                    format!(
                                        "{}/{} completed",
                                        progress.steps_completed, progress.steps_discovered
                                    )
                                },
                            ),
                        ),
                    ],
                    phase_steps: steps
                        .iter()
                        .map(|step| {
                            let latest = step.attempts.last();
                            let options = step.effective_options.as_ref().unwrap_or(&step.options);
                            super::WorkflowPhaseStep {
                                label: if step.label.is_empty() {
                                    step.logical_id.clone()
                                } else {
                                    step.label.clone()
                                },
                                model: latest.and_then(|attempt| attempt.resolved_model.clone()),
                                reasoning: latest
                                    .and_then(|attempt| attempt.reasoning_effort.clone()),
                                prompt: options
                                    .get("message")
                                    .or_else(|| options.get("prompt"))
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or_default()
                                    .into(),
                                status: step_status(step).into(),
                            }
                        })
                        .collect(),
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
        for step in steps {
            let label = if step.label.is_empty() {
                if step.logical_id.is_empty() {
                    &step.id
                } else {
                    &step.logical_id
                }
            } else {
                &step.label
            };
            let latest = step.attempts.last();
            let step_status = step_status(step);
            let overview = details::overview(step, label, step.attempts.len().checked_sub(1));
            let step_key = format!("step:{}", step.id);
            let depth = if show_phase { 2 } else { 1 };
            children.push(WorkflowChild {
                key: step_key.clone(),
                resolved_model: latest.and_then(|attempt| attempt.resolved_model.clone()),
                reasoning_effort: latest.and_then(|attempt| attempt.reasoning_effort.clone()),
                updated_at: step.completed_at.or(step.updated_at),
                action_target: Some(super::actions::ActionTarget::Step {
                    logical_id: step.logical_id.clone(),
                    expected_attempt_id: latest.map(|attempt| attempt.id.clone()),
                    expected_turn_id: None,
                }),
                transcript_target: Some(
                    codex_app_server_protocol::WorkflowTranscriptTarget::Step {
                        step_id: step.id.clone(),
                    },
                ),
                label: format!("{label} · {step_status}"),
                depth,
                thread_id: latest
                    .and_then(|attempt| attempt.child_thread_id.as_deref())
                    .and_then(|id| ThreadId::from_string(id).ok()),
                overview: Some(overview),
                ..Default::default()
            });
            for (index, attempt) in step.attempts.iter().enumerate().rev() {
                if index + 1 == step.attempts.len() {
                    continue;
                }
                let overview = details::overview(step, label, Some(index));
                children.push(WorkflowChild {
                    resolved_model: attempt.resolved_model.clone(),
                    reasoning_effort: attempt.reasoning_effort.clone(),
                    updated_at: attempt.completed_at.or(attempt.updated_at),
                    key: format!("attempt:{}", attempt.id),
                    action_target: Some(super::actions::ActionTarget::Step {
                        logical_id: step.logical_id.clone(),
                        expected_attempt_id: Some(attempt.id.clone()),
                        expected_turn_id: None,
                    }),
                    transcript_target: Some(
                        codex_app_server_protocol::WorkflowTranscriptTarget::Attempt {
                            step_id: step.id.clone(),
                            attempt_id: attempt.id.clone(),
                        },
                    ),
                    parent_step_key: Some(step_key.clone()),
                    label: format!(
                        "{} · Attempt {} · {}",
                        label,
                        index + 1,
                        status(attempt.status)
                    ),
                    depth,
                    thread_id: attempt
                        .child_thread_id
                        .as_deref()
                        .and_then(|id| ThreadId::from_string(id).ok()),
                    overview: Some(overview),
                    ..Default::default()
                });
            }
        }
    }
    WorkflowRow {
        detail_revision: 0,
        owner: ThreadId::from_string(&run.owner_thread_id).ok(),
        title: title.clone(),
        id: run.id,
        execution_id: run.execution_id,
        status: status(run.status).into(),
        read_only: run.read_only,
        read_only_reason: run.read_only.then(|| {
            "Legacy workflow is read-only. Its source and transcripts remain available.".into()
        }),
        metadata: run.metadata,
        phase: run.phase,
        phase_progress: run.phase_progress,
        metrics: Some(run.metrics),
        logs: run.logs,
        completed: Some(completed),
        discovered: run.steps.len(),
        agents_used,
        overview: Some(WorkflowOverview {
            title,
            metadata: {
                let mut metadata = vec![("Status".into(), status(run.status).into())];
                if run.format_version >= 2 {
                    metadata.push((
                        "Agent starts".into(),
                        format!("{}/{}", run.accepted_starts, run.limits.max_agents),
                    ));
                }
                if !run.steps.is_empty() {
                    metadata.push((
                        "Steps".into(),
                        format!("{completed}/{} completed", run.steps.len()),
                    ));
                }
                metadata
            },
            input: Some(run.input),
            result: run.result,
            error: run.error,
            ..Default::default()
        }),
        source: run.source,
        project_root: run.project_root.map(|path| path.to_string()),
        created_at: run.created_at,
        updated_at: run.updated_at,
        details: Vec::new(),
        children,
    }
}

pub(crate) fn summary(run: codex_app_server_protocol::WorkflowSummary) -> WorkflowRow {
    let agents_used = run.metrics.agents_used.map(|count| count as usize);
    let title = run.metadata.as_ref().map_or_else(
        || "Legacy workflow".into(),
        |metadata| metadata.name.clone(),
    );
    WorkflowRow {
        owner: ThreadId::from_string(&run.owner_thread_id).ok(),
        title: title.clone(),
        id: run.id,
        execution_id: run.execution_id,
        status: status(run.status).into(),
        read_only: run.read_only,
        read_only_reason: run.read_only.then(|| {
            "Legacy workflow is read-only. Its source and transcripts remain available.".into()
        }),
        metadata: run.metadata,
        phase: run.phase,
        phase_progress: run.phase_progress,
        metrics: Some(run.metrics),
        discovered: run.step_count as usize,
        agents_used,
        project_root: run.project_root.map(|path| path.to_string()),
        created_at: run.created_at,
        updated_at: run.updated_at,
        overview: Some(WorkflowOverview {
            title,
            metadata: {
                let mut metadata = vec![("Status".into(), status(run.status).into())];
                if run.format_version >= 2 {
                    metadata.push((
                        "Agent starts".into(),
                        format!("{}/{}", run.accepted_starts, run.limits.max_agents),
                    ));
                }
                if run.step_count > 0 {
                    metadata.push(("Agent steps".into(), run.step_count.to_string()));
                }
                metadata
            },
            ..Default::default()
        }),
        details: vec!["Press o to open the owning conversation and load workflow details.".into()],
        ..Default::default()
    }
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
