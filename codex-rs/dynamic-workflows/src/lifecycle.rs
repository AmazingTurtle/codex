//! Immutable execution facts and an outbox committed with their state transition.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::WorkflowMetrics;
use crate::WorkflowRun;
use crate::WorkflowStatus;

pub const MAX_LIFECYCLE_EVENTS: usize = 4096;
pub const MAX_EXECUTION_SNAPSHOTS: usize = 128;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowCompletionMode {
    #[default]
    Display,
    Continue,
}

/// Unlike an optional JSON field, this distinguishes no reply from a JSON null reply.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkflowResult {
    Absent,
    Value { value: Value },
}

impl From<&Option<Value>> for WorkflowResult {
    fn from(value: &Option<Value>) -> Self {
        match value {
            Some(value) => Self::Value {
                value: value.clone(),
            },
            None => Self::Absent,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowLifecycleEvent {
    pub id: String,
    pub owner_thread_id: String,
    pub run_id: String,
    pub execution_id: String,
    pub workflow_name: String,
    pub sequence: u64,
    pub created_at: i64,
    pub completion_mode: WorkflowCompletionMode,
    pub kind: WorkflowLifecycleKind,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkflowLifecycleKind {
    StepSkipped {
        step_id: String,
        logical_id: String,
        label: String,
        phase: Option<String>,
        command_id: String,
        result: WorkflowResult,
    },
    Started {
        name: String,
        description: String,
    },
    StepCompleted {
        step_id: String,
        attempt_id: String,
        child_thread_id: String,
        owned_turn_ids: Vec<String>,
        final_turn_id: Option<String>,
        label: String,
        phase: Option<String>,
        status: WorkflowStatus,
        result: WorkflowResult,
        error: Option<String>,
        resolved_model: Option<String>,
        reasoning_effort: Option<String>,
        #[serde(default)]
        active_duration_ms: Option<u64>,
    },
    Completed {
        name: String,
        status: WorkflowStatus,
        result: WorkflowResult,
        error: Option<String>,
        metrics: WorkflowMetrics,
    },
    Interrupted {
        name: String,
        reason: String,
        native_settled: bool,
        metrics: WorkflowMetrics,
    },
    ResultShown {
        name: String,
        status: WorkflowStatus,
        result: WorkflowResult,
        error: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSnapshotAttempt {
    pub step_id: String,
    pub attempt_id: String,
    pub child_thread_id: Option<String>,
    pub status: WorkflowStatus,
}

/// One terminal snapshot per execution, without mutable outbox or duplicated transcripts.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowExecutionSnapshot {
    pub run_id: String,
    pub execution_id: String,
    pub source: String,
    #[serde(default)]
    pub revision_id: Option<String>,
    pub source_hash: String,
    pub input: Value,
    pub metadata: Option<crate::ScriptMetadata>,
    pub completion_mode: WorkflowCompletionMode,
    pub status: WorkflowStatus,
    /// The execution VM was lost before its accepted native work was reconciled.
    pub partial: bool,
    pub result: WorkflowResult,
    pub error: Option<String>,
    pub metrics: WorkflowMetrics,
    pub attempts: Vec<WorkflowSnapshotAttempt>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowLifecycleState {
    /// Old records default to disabled. Opening a store never synthesizes old events.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub terminal_ready: bool,
    #[serde(default)]
    pub interrupted_unsettled: bool,
    #[serde(default)]
    pub events: Vec<WorkflowLifecycleEvent>,
    #[serde(default)]
    pub delivered: BTreeSet<String>,
    #[serde(default)]
    pub snapshots: BTreeMap<String, WorkflowExecutionSnapshot>,
}

pub(crate) fn record_transitions(
    previous: &WorkflowRun,
    next: &mut WorkflowRun,
) -> Result<(), String> {
    if next.lifecycle_version != 1 || !next.lifecycle.enabled {
        return Ok(());
    }
    if next.execution_id != previous.execution_id
        && next.lifecycle.snapshots.len() >= MAX_EXECUTION_SNAPSHOTS
    {
        return Err("workflow execution snapshot limit exhausted (128)".into());
    }
    if next.status == WorkflowStatus::Running && previous.status != WorkflowStatus::Running {
        let metadata = next.metadata.as_ref().ok_or("workflow metadata missing")?;
        push(
            next,
            "started",
            WorkflowLifecycleKind::Started {
                name: metadata.name.clone(),
                description: metadata.description.clone(),
            },
        )?;
    }
    let mut completed = Vec::new();
    let finishing = next.lifecycle.terminal_ready && !previous.lifecycle.terminal_ready;
    for step in next.steps.values() {
        if step.disposition == crate::WorkflowStepDisposition::Skipped
            && previous
                .steps
                .get(&step.id)
                .is_none_or(|old| old.disposition != crate::WorkflowStepDisposition::Skipped)
        {
            let outcome = step
                .control_outcome
                .as_ref()
                .ok_or("skipped workflow task has no control receipt")?;
            completed.push((
                format!("skip:{}:{}", step.id, outcome.command_id),
                WorkflowLifecycleKind::StepSkipped {
                    step_id: step.id.clone(),
                    logical_id: step.logical_id.clone(),
                    label: step.label.clone(),
                    phase: step.phase.clone(),
                    command_id: outcome.command_id.clone(),
                    result: WorkflowResult::from(&step.result),
                },
            ));
        }
        if step.disposition == crate::WorkflowStepDisposition::Skipped {
            continue;
        }
        for attempt in &step.attempts {
            let final_pending = finishing
                && next.status != WorkflowStatus::Interrupted
                && step
                    .attempts
                    .iter()
                    .rev()
                    .find(|last| last.admission == crate::AdmissionStatus::Accepted)
                    .is_some_and(|last| last.id == attempt.id)
                && (attempt.schema_error.is_some()
                    || matches!(
                        attempt.status,
                        WorkflowStatus::Running | WorkflowStatus::Interrupted
                    ));
            if attempt.admission != crate::AdmissionStatus::Accepted
                || (!final_pending
                    && !matches!(
                        attempt.status,
                        WorkflowStatus::Succeeded | WorkflowStatus::Failed
                    ))
                || (attempt.schema_error.is_some() && step.schema_attempts < 5 && !final_pending)
            {
                continue;
            }
            let before = previous
                .steps
                .get(&step.id)
                .and_then(|step| step.attempts.iter().find(|before| before.id == attempt.id));
            if before.is_some_and(|before| before.status == attempt.status) && !final_pending {
                continue;
            }
            let status = if final_pending
                && matches!(
                    attempt.status,
                    WorkflowStatus::Running | WorkflowStatus::Interrupted
                ) {
                next.status
            } else {
                attempt.status
            };
            let child = attempt
                .child_thread_id
                .clone()
                .ok_or("accepted workflow attempt has no child identity")?;
            completed.push((
                format!("step:{}", attempt.id),
                WorkflowLifecycleKind::StepCompleted {
                    step_id: step.id.clone(),
                    attempt_id: attempt.id.clone(),
                    child_thread_id: child,
                    owned_turn_ids: attempt.owned_turn_ids.clone(),
                    final_turn_id: attempt.final_turn_id.clone(),
                    label: step.label.clone(),
                    phase: step.phase.clone(),
                    status,
                    result: if attempt.status == WorkflowStatus::Succeeded {
                        WorkflowResult::from(&step.result)
                    } else {
                        WorkflowResult::Absent
                    },
                    error: if final_pending {
                        next.error.clone().or_else(|| attempt.error.clone())
                    } else {
                        attempt.error.clone()
                    },
                    resolved_model: attempt.resolved_model.clone(),
                    reasoning_effort: attempt.reasoning_effort.clone(),
                    active_duration_ms: step.metrics.active_duration_ms,
                },
            ));
        }
    }
    for (identity, kind) in completed {
        push(next, &identity, kind)?;
    }
    if matches!(
        next.status,
        WorkflowStatus::Succeeded
            | WorkflowStatus::Failed
            | WorkflowStatus::Stopped
            | WorkflowStatus::Interrupted
    ) && next.lifecycle.terminal_ready
        && !previous.lifecycle.terminal_ready
    {
        let name = next
            .metadata
            .as_ref()
            .map(|metadata| metadata.name.clone())
            .unwrap_or_default();
        let mut execution_metrics = next.metrics.clone();
        if next.lifecycle.interrupted_unsettled {
            execution_metrics.usage_complete = false;
            execution_metrics
                .usage_incomplete_reason
                .get_or_insert_with(|| {
                    "workflow VM was lost before accepted native work was reconciled".into()
                });
        }
        let kind = if next.status == WorkflowStatus::Interrupted {
            WorkflowLifecycleKind::Interrupted {
                name,
                reason: next
                    .error
                    .clone()
                    .unwrap_or_else(|| "workflow interrupted; recovery required".into()),
                native_settled: !next.lifecycle.interrupted_unsettled,
                metrics: execution_metrics.clone(),
            }
        } else {
            WorkflowLifecycleKind::Completed {
                name,
                status: next.status,
                result: WorkflowResult::from(&next.result),
                error: next.error.clone(),
                metrics: next.metrics.clone(),
            }
        };
        push(next, "completed", kind)?;
        if !next.lifecycle.snapshots.contains_key(&next.execution_id) {
            if next.lifecycle.snapshots.len() >= MAX_EXECUTION_SNAPSHOTS {
                return Err("workflow execution snapshot limit exhausted (128)".into());
            }
            let snapshot = WorkflowExecutionSnapshot {
                run_id: next.id.clone(),
                execution_id: next.execution_id.clone(),
                revision_id: (!next.steering.state.active_revision_id.is_empty())
                    .then(|| next.steering.state.active_revision_id.clone()),
                source: next.source.clone(),
                source_hash: next.source_hash.clone(),
                input: next.input.clone(),
                metadata: next.metadata.clone(),
                completion_mode: next.completion_mode,
                status: next.status,
                partial: next.lifecycle.interrupted_unsettled,
                result: WorkflowResult::from(&next.result),
                error: next.error.clone(),
                metrics: execution_metrics,
                created_at: next.updated_at,
                attempts: next
                    .steps
                    .values()
                    .filter_map(|step| {
                        step.attempts.last().map(|attempt| WorkflowSnapshotAttempt {
                            step_id: step.id.clone(),
                            attempt_id: attempt.id.clone(),
                            child_thread_id: attempt.child_thread_id.clone(),
                            status: attempt.status,
                        })
                    })
                    .collect(),
            };
            next.lifecycle
                .snapshots
                .insert(next.execution_id.clone(), snapshot);
        }
    }
    Ok(())
}

fn push(run: &mut WorkflowRun, identity: &str, kind: WorkflowLifecycleKind) -> Result<(), String> {
    let key = format!("{}:{}:{identity}", run.id, run.execution_id);
    let id = format!("wfe_{}", &blake3::hash(key.as_bytes()).to_hex()[..48]);
    if run.lifecycle.events.iter().any(|event| event.id == id) {
        return Ok(());
    }
    if run.lifecycle.events.len() >= MAX_LIFECYCLE_EVENTS {
        return Err("workflow lifecycle event limit exhausted (4096)".into());
    }
    run.lifecycle.events.push(WorkflowLifecycleEvent {
        id,
        owner_thread_id: run.owner_thread_id.clone(),
        run_id: run.id.clone(),
        execution_id: run.execution_id.clone(),
        workflow_name: run
            .metadata
            .as_ref()
            .map(|metadata| metadata.name.clone())
            .unwrap_or_default(),
        sequence: run.lifecycle.events.len() as u64,
        created_at: run.updated_at,
        completion_mode: run.completion_mode,
        kind,
    });
    Ok(())
}
