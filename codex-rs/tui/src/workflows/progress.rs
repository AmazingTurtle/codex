//! Full and lightweight snapshots share monotonic accounting within one execution.
use super::*;
use codex_app_server_protocol::WorkflowMetrics;
use codex_app_server_protocol::WorkflowSummary;

fn monotonic_metrics(
    old: &WorkflowMetrics,
    incoming: &WorkflowMetrics,
    previous_status: &str,
) -> WorkflowMetrics {
    let regresses_clock = matches!((old.active_duration_ms, incoming.active_duration_ms), (Some(old), Some(new)) if new < old)
        || matches!((old.wall_duration_ms, incoming.wall_duration_ms), (Some(old), Some(new)) if new < old);
    let regresses_usage = matches!((&old.usage, &incoming.usage), (Some(old), Some(new))
        if new.input_tokens < old.input_tokens || new.output_tokens < old.output_tokens
            || new.cached_input_tokens < old.cached_input_tokens || new.total_tokens < old.total_tokens);
    let regresses_final_coverage = !matches!(previous_status, "Pending" | "Running")
        && old.usage_complete
        && (!incoming.usage_complete || (old.usage.is_some() && incoming.usage.is_none()));
    let regresses_counts = matches!((old.model_turns,incoming.model_turns),(Some(old),Some(new)) if new<old)
        || matches!((old.agents_used,incoming.agents_used),(Some(old),Some(new)) if new<old);
    if regresses_clock || regresses_usage || regresses_final_coverage || regresses_counts {
        return old.clone();
    }
    let mut metrics = incoming.clone();
    if metrics.usage.is_none() && old.usage.is_some() {
        metrics.usage.clone_from(&old.usage);
        metrics.usage_complete = false;
    }
    metrics.active_duration_ms = metrics.active_duration_ms.or(old.active_duration_ms);
    metrics.wall_duration_ms = metrics.wall_duration_ms.or(old.wall_duration_ms);
    metrics.model_turns = metrics.model_turns.or(old.model_turns);
    metrics.agents_used = metrics.agents_used.or(old.agents_used);
    if !metrics.usage_complete && metrics.usage_incomplete_reason.is_none() {
        metrics
            .usage_incomplete_reason
            .clone_from(&old.usage_incomplete_reason);
    }
    metrics
}

impl WorkflowRow {
    pub(super) fn preserve_accounting_from(&mut self, previous: &Self) {
        if self.execution_id.is_empty()
            || self.execution_id != previous.execution_id
            || self.id != previous.id
            || self.owner != previous.owner
            || self.created_at != previous.created_at
        {
            return;
        }
        if let Some(old) = &previous.metrics {
            self.metrics = Some(self.metrics.as_ref().map_or_else(
                || old.clone(),
                |metrics| monotonic_metrics(old, metrics, &previous.status),
            ));
        }
        self.agents_used = self
            .metrics
            .as_ref()
            .and_then(|metrics| metrics.agents_used)
            .map(|count| count as usize);
    }
}

impl WorkflowState {
    pub(crate) fn apply_progress(&mut self, summary: WorkflowSummary) {
        if summary.execution_id.is_empty() {
            return;
        }
        let Some(row) = self.rows.iter_mut().find(|row| row.id == summary.id) else {
            return;
        };
        let Ok(owner) = ThreadId::from_string(&summary.owner_thread_id) else {
            return;
        };
        if row.owner != Some(owner)
            || row.execution_id != summary.execution_id
            || row.created_at != summary.created_at
            || summary.updated_at < row.updated_at
            || row.status != super::projection::status(summary.status)
            || row.read_only != summary.read_only
        {
            return;
        }
        // Separate queues can deliver old accounting after a newer lifecycle event.
        // Only accounting changes here; phase, step, log and selection state stay intact.
        row.metrics = Some(row.metrics.as_ref().map_or_else(
            || summary.metrics.clone(),
            |old| monotonic_metrics(old, &summary.metrics, &row.status),
        ));
        row.agents_used = row
            .metrics
            .as_ref()
            .and_then(|metrics| metrics.agents_used)
            .map(|count| count as usize);
        for incoming in summary.phase_progress {
            let Some(phase) = row
                .phase_progress
                .iter_mut()
                .find(|phase| phase.title == incoming.title && phase.status == incoming.status)
            else {
                continue;
            };
            phase.metrics = monotonic_metrics(
                &phase.metrics,
                &incoming.metrics,
                super::projection::phase_status(phase.status),
            );
            phase.completed_at = phase.completed_at.or(incoming.completed_at);
            phase.updated_at = phase
                .completed_at
                .or(phase.updated_at.max(incoming.updated_at));
            if let Some(child)=row.children.iter_mut().find(|child|matches!(&child.transcript_target,Some(codex_app_server_protocol::WorkflowTranscriptTarget::Phase {phase:title}) if title==&phase.title)) {
                child.updated_at=phase.completed_at.or(phase.updated_at);
                if let Some(overview)=child.overview.as_mut() {overview.metrics=Some(phase.metrics.clone());}
            }
        }
    }
}
