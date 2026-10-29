//! Explicit resume swaps the immutable plan only after its original execution has settled.

use super::WorkflowEngine;
use crate::RevisionResumeRequest;
use crate::WorkflowControlStatus;
use crate::WorkflowHost;
use crate::WorkflowRevisionResponse;
use crate::WorkflowRevisionStatus;
use crate::WorkflowRun;
use crate::WorkflowStatus;
use crate::steering::WorkflowEffectKind;

impl<H: WorkflowHost> WorkflowEngine<H> {
    pub fn prepare_revision_resume(
        &self,
        id: &str,
        request: RevisionResumeRequest,
    ) -> Result<WorkflowRevisionResponse, String> {
        if let Some(response) = self.repeated_revision(
            id,
            &request.command_id,
            &serde_json::to_value(&request).map_err(|error| error.to_string())?,
        )? {
            return Ok(response);
        }
        let active = self.active.lock().map_err(|error| error.to_string())?;
        if active.contains_key(id) {
            return Err("workflow revision is still draining its prior execution".into());
        }
        let original = self
            .store
            .read_execution_snapshot(id, &self.get(id)?.execution_id)?;
        self.store.update(id, |run| {
            crate::orchestration::require_executable(run)?;
            let repeated = run.steering.mutations.contains_key(&request.command_id);
            let response = run.steering.begin_resume(&request)?;
            if repeated {
                return Ok(response);
            }
            run.lifecycle
                .snapshots
                .entry(run.execution_id.clone())
                .or_insert_with(|| original.clone());
            let old_revision = run.steering.state.active_revision_id.clone();
            let revision = run
                .steering
                .revisions
                .get(&request.revision_id)
                .cloned()
                .ok_or("workflow revision not found")?;
            let mut frontier = revision.reusable_statements;
            for effect in run.steering.effects.values() {
                if let WorkflowEffectKind::Agent { step_id } = &effect.kind
                    && effect.statement_index < frontier
                    && run
                        .steps
                        .get(step_id)
                        .is_none_or(|step| step.result.is_none())
                {
                    frontier = effect.statement_index;
                }
            }
            run.steering
                .effects_by_revision
                .insert(old_revision.clone(), run.steering.effects.clone());
            run.steering
                .effects
                .retain(|_, effect| effect.statement_index < frontier);
            let steps = run
                .steering
                .effects
                .values()
                .filter_map(|effect| match &effect.kind {
                    WorkflowEffectKind::Agent { step_id } => Some(step_id.clone()),
                    _ => None,
                })
                .collect::<std::collections::HashSet<_>>();
            let nested = run
                .steering
                .effects
                .values()
                .filter_map(|effect| match &effect.kind {
                    WorkflowEffectKind::Nested { logical_id } => Some(logical_id.clone()),
                    _ => None,
                })
                .collect::<std::collections::HashSet<_>>();
            let budget = run
                .steering
                .effects
                .values()
                .filter_map(|effect| match &effect.kind {
                    WorkflowEffectKind::Budget { logical_id } => Some(logical_id.clone()),
                    _ => None,
                })
                .collect::<std::collections::HashSet<_>>();
            run.steps.retain(|id, _| steps.contains(id));
            run.nested_workflows.retain(|id, _| nested.contains(id));
            run.budget_observations.retain(|id, _| budget.contains(id));
            for command in &mut run.steering.commands {
                if command.revision_id == old_revision
                    && command.status == WorkflowControlStatus::Pending
                {
                    command.status = WorkflowControlStatus::Rejected;
                    command.error =
                        Some("the plan was superseded before this future task was reached".into());
                }
            }
            if let Some(old) = run.steering.revisions.get_mut(&old_revision) {
                old.status = WorkflowRevisionStatus::Superseded;
            }
            run.source = revision.source;
            run.source_hash = revision.source_hash;
            run.input = revision.input;
            run.metadata = Some(revision.metadata.clone());
            run.phases = revision
                .metadata
                .phases
                .iter()
                .map(|phase| phase.title.clone())
                .collect();
            for snapshot in run.nested_workflows.values() {
                for phase in &snapshot.metadata.phases {
                    let title = format!("{} / {}", snapshot.metadata.name, phase.title);
                    if !run.phases.contains(&title) {
                        run.phases.push(title);
                    }
                }
            }
            run.phase_progress = run
                .phases
                .iter()
                .map(|title| crate::PhaseProgress {
                    title: title.clone(),
                    status: crate::PhaseStatus::Pending,
                    steps_discovered: 0,
                    steps_completed: 0,
                    updated_at: None,
                    completed_at: None,
                    metrics: Default::default(),
                })
                .collect();
            run.phase = None;
            run.result = None;
            run.error = None;
            run.status = WorkflowStatus::Pending;
            run.execution_id = uuid::Uuid::new_v4().to_string();
            run.notification_delivered = false;
            run.activity.execution_duration_ms = 0;
            run.lifecycle.terminal_ready = false;
            run.lifecycle.interrupted_unsettled = false;
            run.steering.version = 1;
            run.steering.state.active_revision_id = request.revision_id.clone();
            run.steering.state.pending_revision_id = None;
            run.steering.state.status = WorkflowRevisionStatus::Active;
            run.steering.replay_frontier = frontier;
            run.steering.suffix_open = false;
            let plan = run
                .steering
                .revisions
                .get_mut(&request.revision_id)
                .ok_or("workflow revision missing")?;
            plan.status = WorkflowRevisionStatus::Active;
            plan.reusable_statements = frontier;
            plan.execution_ids.push(run.execution_id.clone());
            crate::orchestration::refresh_phases(run);
            run.steering.revision_response(&request.revision_id)
        })
    }

    pub async fn run_prepared_revision(&self, id: &str) -> Result<WorkflowRun, String> {
        self.run(id).await
    }
}
