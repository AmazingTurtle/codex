//! Keep the workflow engine independent of the app-server's wire schema.

use codex_app_server_protocol as api;
use codex_core::workflows as engine;
use codex_utils_path_uri::LegacyAppPathString;

pub(in crate::request_processors) fn workflow(run: engine::WorkflowRun) -> api::WorkflowRun {
    api::WorkflowRun {
        id: run.id,
        execution_id: run.execution_id,
        format_version: run.format_version,
        read_only: run.format_version != 2,
        metadata: run.metadata.map(metadata),
        limits: limits(run.limits),
        accepted_starts: run.accepted_starts,
        phase_progress: run.phase_progress.into_iter().map(phase_progress).collect(),
        metrics: metrics(run.metrics),
        logs: run
            .logs
            .into_iter()
            .map(|log| api::WorkflowLog {
                sequence: log.sequence,
                message: log.message,
                phase: log.phase,
                created_at: log.created_at,
            })
            .collect(),
        owner_thread_id: run.owner_thread_id,
        project_root: run
            .project_root
            .as_ref()
            .map(LegacyAppPathString::from_abs_path),
        source: run.source,
        input: run.input,
        status: status(run.status),
        phase: run.phase,
        phases: run.phases,
        steps: run
            .steps
            .into_iter()
            .map(|(id, step)| {
                (
                    id,
                    api::WorkflowStep {
                        id: step.id,
                        logical_id: step.logical_id,
                        label: step.label,
                        scope: step.scope,
                        key: step.key,
                        disposition: match step.disposition {
                            engine::WorkflowStepDisposition::Native => {
                                api::WorkflowStepDisposition::Native
                            }
                            engine::WorkflowStepDisposition::Skipped => {
                                api::WorkflowStepDisposition::Skipped
                            }
                        },
                        control_outcome: step.control_outcome.map(|outcome| {
                            api::WorkflowStepControlOutcome {
                                command_id: outcome.command_id,
                                revision_id: outcome.revision_id,
                                replacement: super::steering_conversion::result(
                                    outcome.replacement,
                                ),
                            }
                        }),
                        effective_options: step.effective_options,
                        phase: step.phase,
                        options: step.options,
                        attempts: step
                            .attempts
                            .into_iter()
                            .map(|attempt| api::WorkflowAttempt {
                                id: attempt.id,
                                execution_id: attempt.execution_id,
                                admission: match attempt.admission {
                                    engine::AdmissionStatus::NotRequested => {
                                        api::WorkflowAdmissionStatus::NotRequested
                                    }
                                    engine::AdmissionStatus::Reserved => {
                                        api::WorkflowAdmissionStatus::Reserved
                                    }
                                    engine::AdmissionStatus::Accepted => {
                                        api::WorkflowAdmissionStatus::Accepted
                                    }
                                    engine::AdmissionStatus::Rejected => {
                                        api::WorkflowAdmissionStatus::Rejected
                                    }
                                    engine::AdmissionStatus::Uncertain => {
                                        api::WorkflowAdmissionStatus::Uncertain
                                    }
                                },
                                child_thread_id: attempt.child_thread_id,
                                resolved_model: attempt.resolved_model,
                                reasoning_effort: attempt.reasoning_effort,
                                owned_turn_ids: attempt.owned_turn_ids,
                                final_turn_id: attempt.final_turn_id,
                                status: status(attempt.status),
                                usage: attempt.usage,
                                error: attempt.error,
                                metrics: metrics(attempt.metrics),
                                updated_at: attempt.updated_at,
                                completed_at: attempt.completed_at,
                            })
                            .collect(),
                        result: step.result,
                        metrics: metrics(step.metrics),
                        updated_at: step.updated_at,
                        completed_at: step.completed_at,
                    },
                )
            })
            .collect(),
        result: run.result,
        error: run.error,
        created_at: run.created_at,
        updated_at: run.updated_at,
    }
}

pub(in crate::request_processors) fn summary(run: engine::WorkflowSummary) -> api::WorkflowSummary {
    api::WorkflowSummary {
        id: run.id,
        execution_id: run.execution_id,
        format_version: run.format_version,
        read_only: run.format_version != 2,
        metadata: run.metadata.map(metadata),
        limits: limits(run.limits),
        accepted_starts: run.accepted_starts,
        phase_progress: run.phase_progress.into_iter().map(phase_progress).collect(),
        metrics: metrics(run.metrics),
        owner_thread_id: run.owner_thread_id,
        project_root: run
            .project_root
            .as_ref()
            .map(LegacyAppPathString::from_abs_path),
        status: status(run.status),
        phase: run.phase,
        phase_count: run.phase_count,
        step_count: run.step_count,
        created_at: run.created_at,
        updated_at: run.updated_at,
    }
}

pub(super) fn status(status: engine::WorkflowStatus) -> api::WorkflowStatus {
    match status {
        engine::WorkflowStatus::Pending => api::WorkflowStatus::Pending,
        engine::WorkflowStatus::Running => api::WorkflowStatus::Running,
        engine::WorkflowStatus::Succeeded => api::WorkflowStatus::Succeeded,
        engine::WorkflowStatus::Failed => api::WorkflowStatus::Failed,
        engine::WorkflowStatus::Stopped => api::WorkflowStatus::Stopped,
        engine::WorkflowStatus::Interrupted => api::WorkflowStatus::Interrupted,
    }
}

pub(super) fn saved(saved: engine::SavedWorkflow) -> api::SavedWorkflow {
    api::SavedWorkflow {
        id: saved.id,
        name: saved.name,
        scope: scope(saved.scope),
        source: saved.source,
        metadata: saved.metadata.map(metadata),
        read_only: saved.read_only,
        read_only_reason: saved.read_only_reason,
        format_version: saved.format_version,
    }
}

pub(super) fn saved_summary(saved: engine::SavedWorkflow) -> api::SavedWorkflowSummary {
    api::SavedWorkflowSummary {
        id: saved.id,
        name: saved.name,
        scope: scope(saved.scope),
        metadata: saved.metadata.map(metadata),
        read_only: saved.read_only,
        read_only_reason: saved.read_only_reason,
        format_version: saved.format_version,
    }
}

fn scope(scope: engine::WorkflowScope) -> api::WorkflowScope {
    match scope {
        engine::WorkflowScope::Project => api::WorkflowScope::Project,
        engine::WorkflowScope::User => api::WorkflowScope::User,
    }
}

pub(super) fn engine_scope(scope: api::WorkflowScope) -> engine::WorkflowScope {
    match scope {
        api::WorkflowScope::Project => engine::WorkflowScope::Project,
        api::WorkflowScope::User => engine::WorkflowScope::User,
    }
}

pub(super) fn metadata(value: engine::WorkflowMetadata) -> api::WorkflowMetadata {
    api::WorkflowMetadata {
        name: value.name,
        description: value.description,
        phases: value
            .phases
            .into_iter()
            .map(|phase| api::WorkflowPhaseDefinition { title: phase.title })
            .collect(),
    }
}
fn limits(value: engine::RunLimits) -> api::WorkflowRunLimits {
    api::WorkflowRunLimits {
        max_agents: value.max_agents as u32,
        max_tokens: value.max_tokens,
    }
}
fn phase_progress(value: engine::PhaseProgress) -> api::WorkflowPhaseProgress {
    api::WorkflowPhaseProgress {
        title: value.title,
        status: match value.status {
            engine::PhaseStatus::Pending => api::WorkflowPhaseStatus::Pending,
            engine::PhaseStatus::Active => api::WorkflowPhaseStatus::Active,
            engine::PhaseStatus::Completed => api::WorkflowPhaseStatus::Completed,
            engine::PhaseStatus::Skipped => api::WorkflowPhaseStatus::Skipped,
            engine::PhaseStatus::Failed => api::WorkflowPhaseStatus::Failed,
        },
        steps_discovered: value.steps_discovered,
        steps_completed: value.steps_completed,
        metrics: metrics(value.metrics),
        updated_at: value.updated_at,
        completed_at: value.completed_at,
    }
}
pub(super) fn metrics(value: engine::WorkflowMetrics) -> api::WorkflowMetrics {
    api::WorkflowMetrics {
        usage: value.usage.map(|usage| api::WorkflowTokenUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            total_tokens: usage.total_tokens,
        }),
        usage_complete: value.usage_complete,
        usage_incomplete_reason: value.usage_incomplete_reason,
        active_duration_ms: value.active_duration_ms,
        wall_duration_ms: value.wall_duration_ms,
        model_turns: value.model_turns,
        agents_used: value.agents_used,
    }
}
