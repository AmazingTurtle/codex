//! Immutable execution indexes pin original detail records independently of the current run.
use super::files::Loaded;
use super::files::Manifest;
use super::files::StepIndex;
use super::files::{self};
use crate::WorkflowAttempt;
use crate::WorkflowExecutionAttempt;
use crate::WorkflowExecutionSnapshot;
use crate::WorkflowExecutionStepIndex;
use crate::WorkflowResult;
use crate::WorkflowRun;
use crate::WorkflowStatus;
use crate::WorkflowStep;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

#[derive(Deserialize, Serialize)]
struct ExecutionIndex {
    run_id: String,
    execution_id: String,
    definition: String,
    steps: BTreeMap<String, ExecutionStep>,
}

#[derive(Deserialize, Serialize)]
struct ExecutionStep {
    record: String,
    attempts: Vec<AttemptIndex>,
}

#[derive(Deserialize, Serialize)]
struct AttemptIndex {
    id: String,
    record: String,
}

pub(super) fn pin(
    records: &AbsolutePathBuf,
    run: &WorkflowRun,
    prior: Option<&Loaded>,
    definition: &str,
    steps: &BTreeMap<String, StepIndex>,
) -> Result<(BTreeMap<String, String>, BTreeSet<String>), String> {
    let previous = prior.and_then(|prior| prior.manifest.as_ref());
    let mut indexes =
        previous.map_or_else(BTreeMap::new, |manifest| manifest.execution_steps.clone());
    let mut pins = previous.map_or_else(BTreeSet::new, |manifest| manifest.execution_pins.clone());
    for execution in run.lifecycle.snapshots.keys() {
        if indexes.contains_key(execution)
            || previous.is_some_and(|manifest| manifest.execution_snapshots.contains_key(execution))
        {
            continue;
        }
        let index = if execution == &run.execution_id {
            index_from_run(run, definition, steps)?
        } else if let Some(prior) = prior.filter(|prior| &prior.run.execution_id == execution) {
            let manifest = prior
                .manifest
                .as_ref()
                .ok_or("execution has no original detail manifest")?;
            index_from_run(&prior.run, &manifest.definition, &manifest.steps)?
        } else {
            return Err("new execution snapshot has no original detail records".into());
        };
        pins.insert(index.definition.clone());
        for step in index.steps.values() {
            pins.insert(step.record.clone());
            pins.extend(step.attempts.iter().map(|attempt| attempt.record.clone()));
        }
        let record = files::write_record(records, &index)?;
        pins.insert(record.clone());
        indexes.insert(execution.clone(), record);
    }
    Ok((indexes, pins))
}

fn index_from_run(
    run: &WorkflowRun,
    definition: &str,
    steps: &BTreeMap<String, StepIndex>,
) -> Result<ExecutionIndex, String> {
    let steps = steps
        .iter()
        .map(|(id, index)| {
            let step = run
                .steps
                .get(id)
                .ok_or("execution step details are missing")?;
            if step.attempts.len() != index.attempts.len() {
                return Err("execution attempt index does not match its details".to_owned());
            }
            let attempts = step
                .attempts
                .iter()
                .zip(&index.attempts)
                .map(|(attempt, record)| AttemptIndex {
                    id: attempt.id.clone(),
                    record: record.clone(),
                })
                .collect();
            Ok((
                id.clone(),
                ExecutionStep {
                    record: index.record.clone(),
                    attempts,
                },
            ))
        })
        .collect::<Result<_, String>>()?;
    Ok(ExecutionIndex {
        run_id: run.id.clone(),
        execution_id: run.execution_id.clone(),
        definition: definition.into(),
        steps,
    })
}

fn manifest(root: &AbsolutePathBuf, run: &str) -> Result<Manifest, String> {
    let manifest: Manifest = files::read_json(&files::path(root, run)?)?;
    if manifest.storage_version != 2 || manifest.header.id != run {
        return Err("workflow execution manifest identity mismatch".into());
    }
    Ok(manifest)
}

fn record<T: serde::de::DeserializeOwned>(
    records: &AbsolutePathBuf,
    id: &str,
) -> Result<T, String> {
    files::read_record(records, id).map_err(|error| match error {
        files::ReadFailure::MissingRecord(error) | files::ReadFailure::Other(error) => error,
    })
}

/// Reads a pinned terminal execution, or an explicitly partial current projection, without starting agents.
pub fn read_execution_snapshot(
    root: &AbsolutePathBuf,
    run: &str,
    execution: &str,
) -> Result<WorkflowExecutionSnapshot, String> {
    let manifest = manifest(root, run)?;
    if let Some(id) = manifest.execution_snapshots.get(execution) {
        let snapshot: WorkflowExecutionSnapshot = record(&root.join(run).join("records"), id)?;
        if snapshot.run_id != run || snapshot.execution_id != execution {
            return Err("workflow execution snapshot identity mismatch".into());
        }
        return Ok(snapshot);
    }
    if manifest.header.execution_id != execution {
        return Err("workflow execution snapshot not found".into());
    }
    let current = files::read(root, run)?.run;
    let attempts = current
        .steps
        .values()
        .filter_map(|step| {
            step.attempts
                .last()
                .map(|attempt| crate::WorkflowSnapshotAttempt {
                    step_id: step.id.clone(),
                    attempt_id: attempt.id.clone(),
                    child_thread_id: attempt.child_thread_id.clone(),
                    status: attempt.status,
                })
        })
        .collect();
    Ok(WorkflowExecutionSnapshot {
        run_id: current.id,
        execution_id: current.execution_id,
        revision_id: (!current.steering.state.active_revision_id.is_empty())
            .then(|| current.steering.state.active_revision_id.clone()),
        source: current.source,
        source_hash: current.source_hash,
        input: current.input,
        metadata: current.metadata,
        completion_mode: current.completion_mode,
        status: current.status,
        partial: true,
        result: WorkflowResult::from(&current.result),
        error: current.error,
        metrics: current.metrics,
        attempts,
        created_at: current.updated_at,
    })
}

fn load_index(
    root: &AbsolutePathBuf,
    run: &str,
    execution: &str,
) -> Result<ExecutionIndex, String> {
    let manifest = manifest(root, run)?;
    let records = root.join(run).join("records");
    if let Some(id) = manifest.execution_steps.get(execution) {
        if !manifest.execution_snapshots.contains_key(execution) {
            return Err("workflow execution snapshot not found".into());
        }
        let index: ExecutionIndex = record(&records, id)?;
        if index.run_id != run || index.execution_id != execution {
            return Err("workflow execution index identity mismatch".into());
        }
        return Ok(index);
    }
    let loaded = files::read(root, run)?;
    if manifest.header.execution_id == execution
        && !manifest.execution_snapshots.contains_key(execution)
    {
        return index_from_run(&loaded.run, &manifest.definition, &manifest.steps);
    }
    // Pre-index snapshots can prove only their explicitly listed attempt UUIDs.
    // Never substitute an unrelated current attempt after revision or retry.
    let snapshot = read_execution_snapshot(root, run, execution)?;
    if snapshot.source != loaded.run.source || snapshot.input != loaded.run.input {
        return Err(
            "historical execution details were not pinned; original step metadata is unavailable"
                .into(),
        );
    }
    let mut index = index_from_run(&loaded.run, &manifest.definition, &manifest.steps)?;
    index.execution_id = execution.into();
    for (step_id, step) in &mut index.steps {
        step.attempts.retain(|attempt| {
            snapshot
                .attempts
                .iter()
                .any(|known| &known.step_id == step_id && known.attempt_id == attempt.id)
        });
    }
    index.steps.retain(|_, step| !step.attempts.is_empty());
    if index.steps.len()
        != snapshot
            .attempts
            .iter()
            .map(|attempt| &attempt.step_id)
            .collect::<BTreeSet<_>>()
            .len()
    {
        return Err("historical execution attempt details are no longer available".into());
    }
    Ok(index)
}

/// Reports when safe historical membership is known but earlier attempts cannot be enumerated.
pub fn execution_transcript_limitation(
    root: &AbsolutePathBuf,
    run: &str,
    execution: &str,
) -> Result<Option<String>, String> {
    // Apply the same identity, original-definition and retained-membership checks as reads.
    let _index = load_index(root, run, execution)?;
    let manifest = manifest(root, run)?;
    if manifest.execution_steps.contains_key(execution)
        || (manifest.header.execution_id == execution
            && !manifest.execution_snapshots.contains_key(execution))
    {
        return Ok(None);
    }
    if !manifest.execution_snapshots.contains_key(execution) {
        return Err("workflow execution snapshot not found".into());
    }
    Ok(Some("This historical snapshot predates exhaustive attempt indexes; only its explicitly recorded attempts are available.".into()))
}

/// Lists every original step and attempt identity through a bounded metadata page.
pub fn list_execution_steps(
    root: &AbsolutePathBuf,
    run: &str,
    execution: &str,
    offset: usize,
    limit: usize,
) -> Result<Vec<WorkflowExecutionStepIndex>, String> {
    if !(1..=crate::MAX_PAGE_SIZE).contains(&limit) {
        return Err("execution step limit must be between 1 and 50".into());
    }
    let index = load_index(root, run, execution)?;
    let records = root.join(run).join("records");
    let mut steps = index
        .steps
        .into_iter()
        .map(|(id, index)| {
            let step: WorkflowStep = record(&records, &index.record)?;
            if step.id != id {
                return Err("execution step identity mismatch".to_owned());
            }
            Ok(WorkflowExecutionStepIndex {
                step_id: id,
                logical_id: step.logical_id,
                label: step.label,
                phase: step.phase,
                worker_sequence: step.worker_sequence,
                attempt_ids: index
                    .attempts
                    .into_iter()
                    .map(|attempt| attempt.id)
                    .collect(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    steps.sort_by(|a, b| (a.worker_sequence, &a.step_id).cmp(&(b.worker_sequence, &b.step_id)));
    Ok(steps.into_iter().skip(offset).take(limit).collect())
}

/// Reads an exact original attempt after proving its execution and step membership.
pub fn read_execution_attempt(
    root: &AbsolutePathBuf,
    run: &str,
    execution: &str,
    step_id: &str,
    attempt_id: &str,
) -> Result<WorkflowExecutionAttempt, String> {
    let index = load_index(root, run, execution)?;
    let step_index = index
        .steps
        .get(step_id)
        .ok_or("step is not part of this workflow execution")?;
    let attempt_index = step_index
        .attempts
        .iter()
        .find(|attempt| attempt.id == attempt_id)
        .ok_or("attempt is not part of this execution step")?;
    let records = root.join(run).join("records");
    let step: WorkflowStep = record(&records, &step_index.record)?;
    let attempt: WorkflowAttempt = record(&records, &attempt_index.record)?;
    if step.id != step_id || attempt.id != attempt_id {
        return Err("workflow execution detail identity mismatch".into());
    }
    let result = if step_index
        .attempts
        .last()
        .is_some_and(|last| last.id == attempt_id)
        && attempt.status == WorkflowStatus::Succeeded
    {
        WorkflowResult::from(&step.result)
    } else {
        WorkflowResult::Absent
    };
    Ok(WorkflowExecutionAttempt {
        step_id: step.id,
        logical_id: step.logical_id,
        label: step.label,
        phase: step.phase,
        options: step.options,
        attempt,
        result,
    })
}

#[cfg(test)]
#[path = "executions_tests.rs"]
mod tests;
