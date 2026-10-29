//! Plan edits are immutable proposals; applying one never grants fresh native admission.

use super::WorkflowMutationRecord;
use super::WorkflowSteeringState;
use super::validate_command_id;
use crate::RevisionApplyRequest;
use crate::RevisionResumeRequest;
use crate::RevisionStageRequest;
use crate::WorkflowResult;
use crate::WorkflowRevision;
use crate::WorkflowRevisionResponse;
use crate::WorkflowRevisionStatus;
use crate::WorkflowRun;
use crate::revision::MAX_WORKFLOW_REVISIONS;

impl WorkflowSteeringState {
    pub(crate) fn initialize(run: &WorkflowRun) -> Result<Self, String> {
        let compiled = crate::compile_script(&run.source)?;
        let program = crate::instrument_revision_script(&compiled, 0)?;
        let id = uuid::Uuid::new_v4().to_string();
        let revision = WorkflowRevision {
            id: id.clone(),
            parent_id: None,
            number: 0,
            created_at: run.created_at,
            source: run.source.clone(),
            input: run.input.clone(),
            source_hash: run.source_hash.clone(),
            metadata: compiled.metadata,
            reusable_statements: 0,
            invalidation_reason: "Initial workflow plan".into(),
            statement_count: program
                .statement_count
                .try_into()
                .map_err(|_| "workflow statement limit exceeded")?,
            status: WorkflowRevisionStatus::Active,
            mode: None,
            error: None,
            execution_ids: vec![run.execution_id.clone()],
        };
        let mut state = Self {
            version: 1,
            ..Default::default()
        };
        state.state.active_revision_id = id.clone();
        state.revisions.insert(id, revision);
        Ok(state)
    }
    pub(crate) fn stage(
        &mut self,
        run: &WorkflowRun,
        request: &RevisionStageRequest,
    ) -> Result<WorkflowRevisionResponse, String> {
        validate_command_id(&request.command_id)?;
        let payload = serde_json::to_value(request).map_err(|error| error.to_string())?;
        if let Some(record) = self.repeated_mutation(&request.command_id, &payload)? {
            return self.revision_response(
                record
                    .revision_id
                    .as_deref()
                    .ok_or("staged revision identity missing")?,
            );
        }
        self.check_revision(request.expected_control_revision)?;
        crate::orchestration::require_executable(run)?;
        if self.revisions.len() >= MAX_WORKFLOW_REVISIONS {
            return Err("workflow plan revision limit exhausted (128)".into());
        }
        let compiled = crate::compile_script(&request.source)?;
        let input = match &request.input {
            WorkflowResult::Absent => run.input.clone(),
            WorkflowResult::Value { value } => value.clone(),
        };
        crate::types::bounded_value(&input)?;
        let analysis = crate::analyze_revision(&run.source, &request.source, run.input != input)?;
        if self.state.active_revision_id.is_empty() {
            let original = crate::compile_script(&run.source)?;
            let program = crate::instrument_revision_script(&original, 0)?;
            let id = uuid::Uuid::new_v4().to_string();
            self.revisions.insert(
                id.clone(),
                WorkflowRevision {
                    id: id.clone(),
                    parent_id: None,
                    number: 0,
                    created_at: run.created_at,
                    source: run.source.clone(),
                    input: run.input.clone(),
                    source_hash: run.source_hash.clone(),
                    metadata: original.metadata,
                    reusable_statements: 0,
                    invalidation_reason: "Initial workflow plan".into(),
                    statement_count: program
                        .statement_count
                        .try_into()
                        .map_err(|_| "workflow statement limit exceeded")?,
                    status: WorkflowRevisionStatus::Active,
                    mode: None,
                    error: None,
                    execution_ids: vec![run.execution_id.clone()],
                },
            );
            self.state.active_revision_id = id;
        }
        let id = uuid::Uuid::new_v4().to_string();
        let mut frontier = if self.version == 1 {
            analysis.reusable_statements as u32
        } else {
            0
        };
        let mut reason = if self.version == 1 {
            analysis.invalidation_reason
        } else {
            "Current execution has no compiler statement receipts; no prior calls can be reused safely.".into()
        };
        for effect in self.effects.values() {
            if let super::WorkflowEffectKind::Agent { step_id } = &effect.kind
                && let Some(step) = run.steps.get(step_id)
                && effect.statement_index < frontier
                && ((step.result.is_none()
                    && step
                        .attempts
                        .last()
                        .is_some_and(|attempt| attempt.status == crate::WorkflowStatus::Failed))
                    || step.effective_options.is_some()
                    || step.control_outcome.is_some())
            {
                frontier = effect.statement_index;
                reason = "An earlier task is unfinished, failed, or has a human override; that statement must run again.".into();
            }
        }
        let revision = WorkflowRevision {
            id: id.clone(),
            parent_id: Some(self.state.active_revision_id.clone()),
            number: self
                .revisions
                .len()
                .try_into()
                .map_err(|_| "workflow revision limit exceeded")?,
            created_at: crate::types::timestamp(),
            source: request.source.clone(),
            input,
            source_hash: crate::orchestration::source_hash(&request.source),
            metadata: compiled.metadata,
            // Pre-instrumentation runs have no statement receipts: reuse is deliberately empty.
            reusable_statements: frontier,
            invalidation_reason: reason,
            statement_count: analysis
                .revised_statements
                .try_into()
                .map_err(|_| "workflow statement limit exceeded")?,
            status: WorkflowRevisionStatus::Draft,
            mode: None,
            error: None,
            execution_ids: Vec::new(),
        };
        self.revisions.insert(id.clone(), revision);
        self.remember_mutation(&request.command_id, payload, Some(id.clone()))?;
        self.revision_response(&id)
    }

    pub(crate) fn begin_apply(
        &mut self,
        request: &RevisionApplyRequest,
    ) -> Result<WorkflowRevisionResponse, String> {
        validate_command_id(&request.command_id)?;
        let payload = serde_json::to_value(request).map_err(|error| error.to_string())?;
        if self
            .repeated_mutation(&request.command_id, &payload)?
            .is_some()
        {
            return self.revision_response(&request.revision_id);
        }
        self.check_revision(request.expected_control_revision)?;
        if self.state.pending_revision_id.is_some() {
            return Err("another workflow plan revision is already being applied".into());
        }
        let revision = self
            .revisions
            .get_mut(&request.revision_id)
            .ok_or("workflow revision not found")?;
        if revision.status != WorkflowRevisionStatus::Draft
            || revision.parent_id.as_ref() != Some(&self.state.active_revision_id)
        {
            return Err("workflow revision does not revise the active plan".into());
        }
        revision.status = WorkflowRevisionStatus::Fencing;
        revision.mode = Some(request.mode);
        self.state.status = WorkflowRevisionStatus::Fencing;
        self.state.pending_revision_id = Some(request.revision_id.clone());
        self.remember_mutation(
            &request.command_id,
            payload,
            Some(request.revision_id.clone()),
        )?;
        self.revision_response(&request.revision_id)
    }

    pub(crate) fn begin_resume(
        &mut self,
        request: &RevisionResumeRequest,
    ) -> Result<WorkflowRevisionResponse, String> {
        validate_command_id(&request.command_id)?;
        let payload = serde_json::to_value(request).map_err(|error| error.to_string())?;
        if self
            .repeated_mutation(&request.command_id, &payload)?
            .is_some()
        {
            return self.revision_response(&request.revision_id);
        }
        self.check_revision(request.expected_control_revision)?;
        if self.state.status != WorkflowRevisionStatus::Ready
            || self.state.pending_revision_id.as_ref() != Some(&request.revision_id)
        {
            return Err("workflow revision is not ready to resume".into());
        }
        self.remember_mutation(
            &request.command_id,
            payload,
            Some(request.revision_id.clone()),
        )?;
        self.revision_response(&request.revision_id)
    }

    pub(crate) fn check_revision(&self, expected: u64) -> Result<(), String> {
        if self.state.control_revision != expected {
            return Err(format!(
                "workflow control revision changed: expected {expected}, found {}",
                self.state.control_revision
            ));
        }
        Ok(())
    }

    pub(crate) fn repeated_mutation(
        &self,
        id: &str,
        payload: &serde_json::Value,
    ) -> Result<Option<&WorkflowMutationRecord>, String> {
        match self.mutations.get(id) {
            Some(record) if &record.request != payload => {
                Err("workflow command ID was already used with different arguments".into())
            }
            existing => Ok(existing),
        }
    }

    pub(crate) fn remember_mutation(
        &mut self,
        id: &str,
        payload: serde_json::Value,
        revision_id: Option<String>,
    ) -> Result<(), String> {
        if self.mutations.len() >= super::MAX_WORKFLOW_CONTROLS {
            return Err("workflow control command limit exhausted (1024)".into());
        }
        self.state.control_revision = self
            .state
            .control_revision
            .checked_add(1)
            .ok_or("workflow control revision exhausted")?;
        self.mutations.insert(
            id.to_string(),
            WorkflowMutationRecord {
                request: payload,
                control_revision: self.state.control_revision,
                revision_id,
            },
        );
        Ok(())
    }

    pub(crate) fn revision_response(&self, id: &str) -> Result<WorkflowRevisionResponse, String> {
        Ok(WorkflowRevisionResponse {
            revision: self
                .revisions
                .get(id)
                .cloned()
                .ok_or("workflow revision not found")?,
            state: self.state.clone(),
        })
    }
}
