//! Admission planning applies controls before reserving a real native attempt.

use crate::AdmissionStatus;
use crate::AgentRequest;
use crate::AgentStartMode;
use crate::WorkflowAttempt;
use crate::WorkflowControlOperation;
use crate::WorkflowControlStatus;
use crate::WorkflowControlTarget;
use crate::WorkflowResult;
use crate::WorkflowRun;
use crate::WorkflowStatus;
use crate::WorkflowStep;
use crate::WorkflowStepControlOutcome;
use crate::WorkflowStepDisposition;
use serde_json::Value;

pub(super) enum AgentPlan {
    Cached(Value),
    Dispatch(Box<AgentRequest>, AgentStartMode),
}

pub(super) struct AgentCall<'a> {
    pub step_id: &'a str,
    pub logical_id: &'a str,
    pub scope: &'a str,
    pub key: &'a Option<String>,
    pub label: &'a str,
    pub phase: &'a Option<String>,
    pub options: &'a Value,
    pub schema: Option<&'a Value>,
    pub cache_only: bool,
    pub execution_id: &'a str,
    pub revision_id: &'a str,
}

pub(super) fn plan(run: &mut WorkflowRun, call: AgentCall<'_>) -> Result<AgentPlan, String> {
    if run.status != WorkflowStatus::Running {
        return Err("workflow is stopped".into());
    }
    crate::orchestration::validate_effect_execution(run, call.execution_id, call.revision_id)?;
    if call
        .phase
        .as_ref()
        .is_some_and(|phase| !run.phases.contains(phase))
    {
        return Err("workflow phase was not declared in metadata".into());
    }
    if !run.steps.contains_key(call.step_id) {
        if call.cache_only {
            return Err("revision prefix has no completed recorded task".into());
        }
        let latest = run
            .steps
            .values()
            .map(|step| step.worker_sequence)
            .max()
            .unwrap_or_default()
            .max(run.steering.next_step_sequence);
        if usize::from(latest) >= crate::MAX_STEPS {
            return Err("workflow exceeds 1024 logical tasks across its revisions".into());
        }
        run.steering.next_step_sequence = latest + 1;
        run.steps.insert(
            call.step_id.to_string(),
            WorkflowStep {
                id: call.step_id.to_string(),
                updated_at: None,
                completed_at: None,
                metrics: Default::default(),
                scope: call.scope.to_string(),
                key: call.key.clone(),
                disposition: WorkflowStepDisposition::Native,
                control_outcome: None,
                effective_options: None,
                schema_attempts: 0,
                logical_id: call.logical_id.to_string(),
                label: call.label.to_string(),
                worker_sequence: latest + 1,
                phase: call.phase.clone(),
                options: call.options.clone(),
                attempts: Vec::new(),
                result: None,
            },
        );
    }
    {
        let step = run
            .steps
            .get_mut(call.step_id)
            .ok_or("workflow logical task missing")?;
        if step.options != *call.options
            || step.phase != *call.phase
            || step.logical_id != call.logical_id
            || step.scope != call.scope
            || step.key != *call.key
        {
            return Err("inconsistent replay arguments for workflow agent".into());
        }
        if !call.cache_only {
            for command in run.steering.commands.iter_mut().filter(|command| {
                command.revision_id == call.revision_id
                    && command.status == WorkflowControlStatus::Pending
            }) {
                let matches = match &command.target {
                    WorkflowControlTarget::Step { logical_id, .. } => logical_id == call.logical_id,
                    WorkflowControlTarget::FutureStep { scope, key } => {
                        scope == call.scope && call.key.as_ref() == Some(key)
                    }
                    WorkflowControlTarget::Run | WorkflowControlTarget::Phase { .. } => false,
                };
                if !matches {
                    continue;
                }
                if step
                    .attempts
                    .last()
                    .is_some_and(|attempt| attempt.admission != AdmissionStatus::Rejected)
                {
                    continue;
                }
                match &command.operation {
                    WorkflowControlOperation::EditQueued { prompt }
                        if step.disposition != WorkflowStepDisposition::Skipped =>
                    {
                        let mut effective = call.options.clone();
                        effective["message"] = Value::String(prompt.clone());
                        crate::types::bounded_value(&effective)?;
                        step.effective_options = Some(effective);
                        command.status = WorkflowControlStatus::Applied;
                    }
                    WorkflowControlOperation::Skip { replacement } => {
                        let result = match replacement {
                            WorkflowResult::Absent => Value::Null,
                            WorkflowResult::Value { value } => {
                                if let Some(schema) = call.schema {
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
                        step.result = Some(result);
                        command.status = WorkflowControlStatus::Applied;
                    }
                    WorkflowControlOperation::EditQueued { .. } => {
                        command.status = WorkflowControlStatus::Rejected;
                        command.error = Some("the logical task was already skipped".into());
                    }
                    WorkflowControlOperation::Guide { .. }
                    | WorkflowControlOperation::Interrupt => {}
                }
            }
        }
        if let Some(result) = &step.result {
            return Ok(AgentPlan::Cached(result.clone()));
        }
    }
    if call.cache_only {
        return Err("revision prefix task has no proven successful result".into());
    }
    let needs_start = run.steps.get(call.step_id).is_some_and(|step| {
        step.attempts.last().is_none_or(|attempt| {
            !matches!(
                attempt.status,
                WorkflowStatus::Running | WorkflowStatus::Interrupted | WorkflowStatus::Succeeded
            )
        })
    });
    if needs_start {
        crate::orchestration::reserve_start(run)?;
    }
    let step = run
        .steps
        .get_mut(call.step_id)
        .ok_or("workflow logical task missing")?;
    let mode = if let Some(attempt) = step.attempts.last().filter(|attempt| {
        matches!(
            attempt.status,
            WorkflowStatus::Running | WorkflowStatus::Interrupted | WorkflowStatus::Succeeded
        )
    }) {
        AgentStartMode::Recover {
            child_thread_id: attempt.child_thread_id.clone(),
        }
    } else {
        if call.schema.is_some() && step.schema_attempts >= 5 {
            return Err(
                "structured output failed after five attempts; explicit retry is required".into(),
            );
        }
        if step.attempts.len() >= 16 {
            return Err("workflow exceeds 16 attempts per logical step".into());
        }
        let feedback = step
            .attempts
            .last()
            .and_then(|attempt| {
                attempt
                    .schema_error
                    .as_ref()
                    .or(attempt.validation_feedback.as_ref())
            })
            .map(|feedback| {
                let mut feedback = feedback.clone();
                if feedback.len() > 2048 {
                    let mut end = 2048;
                    while !feedback.is_char_boundary(end) {
                        end -= 1;
                    }
                    feedback.truncate(end);
                }
                feedback
            });
        step.attempts.push(WorkflowAttempt {
            id: uuid::Uuid::new_v4().to_string(),
            updated_at: None,
            completed_at: None,
            metrics: Default::default(),
            activity: Default::default(),
            clock_execution_id: None,
            clock_incomplete: false,
            execution_id: run.execution_id.clone(),
            owned_turn_ids: Vec::new(),
            final_turn_id: None,
            resolved_model: None,
            reasoning_effort: None,
            validation_feedback: feedback,
            admission: AdmissionStatus::Reserved,
            schema_error: None,
            child_thread_id: None,
            status: WorkflowStatus::Running,
            usage: None,
            error: None,
        });
        AgentStartMode::Start
    };
    let attempt = step.attempts.last().ok_or("workflow attempt missing")?;
    Ok(AgentPlan::Dispatch(
        Box::new(AgentRequest {
            run_id: run.id.clone(),
            revision_id: call.revision_id.to_string(),
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
        }),
        mode,
    ))
}
