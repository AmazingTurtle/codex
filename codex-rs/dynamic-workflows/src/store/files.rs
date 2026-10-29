//! Immutable detail records become visible through one atomic compact manifest.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;

use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::metrics::UsageObservation;
use crate::BudgetObservation;
use crate::NestedWorkflow;
use crate::WorkflowRun;
use crate::WorkflowStep;

const MAX_RECORD_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Manifest {
    pub storage_version: u32,
    pub revision: String,
    #[serde(default)]
    pub previous_records: Vec<String>,
    pub header: WorkflowRun,
    pub definition: String,
    pub steps: BTreeMap<String, StepIndex>,
    pub result: Option<String>,
    pub usage: BTreeMap<String, String>,
    #[serde(default)]
    pub lifecycle_events: BTreeMap<String, String>,
    #[serde(default)]
    pub execution_snapshots: BTreeMap<String, String>,
    #[serde(default)]
    pub execution_steps: BTreeMap<String, String>,
    /// Union of immutable execution index, definition, step and attempt record IDs.
    #[serde(default)]
    pub execution_pins: BTreeSet<String>,
    #[serde(default)]
    pub steering: Option<super::steering_files::SteeringIndex>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct StepIndex {
    pub record: String,
    pub attempts: Vec<String>,
}

#[derive(Deserialize, Serialize, PartialEq)]
struct Definition {
    source: String,
    input: Value,
    nested_workflows: BTreeMap<String, NestedWorkflow>,
    #[serde(default)]
    budget_observations: BTreeMap<String, BudgetObservation>,
}

pub(super) struct Loaded {
    pub run: WorkflowRun,
    pub manifest: Option<Manifest>,
    pub usage: BTreeMap<String, UsageObservation>,
}

pub(super) fn path(root: &AbsolutePathBuf, id: &str) -> Result<AbsolutePathBuf, String> {
    uuid::Uuid::parse_str(id).map_err(|_| "invalid workflow id".to_string())?;
    Ok(root.join(format!("{id}.json")))
}

pub(super) fn read_json<T: DeserializeOwned>(path: &AbsolutePathBuf) -> Result<T, String> {
    if fs::metadata(path).map_err(|error| error.to_string())?.len() > MAX_RECORD_BYTES {
        return Err("workflow record exceeds size limit".into());
    }
    serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

pub(super) fn read(root: &AbsolutePathBuf, id: &str) -> Result<Loaded, String> {
    let manifest_path = path(root, id)?;
    let mut value: Value = read_json(&manifest_path)?;
    for _ in 0..3 {
        match hydrate(root, id, value.clone()) {
            Ok(loaded) => return Ok(loaded),
            Err(ReadFailure::Other(error)) => return Err(error),
            Err(ReadFailure::MissingRecord(error)) => {
                let latest: Value = read_json(&manifest_path)?;
                if latest.get("revision") == value.get("revision") {
                    return Err(error);
                }
                value = latest;
            }
        }
    }
    Err("workflow manifest changed repeatedly during detail read; retry the read".into())
}

pub(super) enum ReadFailure {
    MissingRecord(String),
    Other(String),
}
impl From<String> for ReadFailure {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}

fn hydrate(root: &AbsolutePathBuf, id: &str, value: Value) -> Result<Loaded, ReadFailure> {
    if value.get("storageVersion").and_then(Value::as_u64) != Some(2) {
        let mut run: WorkflowRun =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        run.format_version = 1;
        return Ok(Loaded {
            run,
            manifest: None,
            usage: BTreeMap::new(),
        });
    }
    let manifest: Manifest = serde_json::from_value(value).map_err(|error| error.to_string())?;
    let records = root.join(id).join("records");
    let definition: Definition = read_record(&records, &manifest.definition)?;
    let mut run = manifest.header.clone();
    if run.id != id {
        return Err("workflow manifest identity mismatch".to_string().into());
    }
    run.source = definition.source;
    run.input = definition.input;
    run.nested_workflows = definition.nested_workflows;
    run.budget_observations = definition.budget_observations;
    if let Some(index) = &manifest.steering {
        run.steering = super::steering_files::read(&records, index)?;
    }
    run.lifecycle.events = manifest
        .lifecycle_events
        .values()
        .map(|record| read_record(&records, record))
        .collect::<Result<_, _>>()?;
    run.lifecycle.events.sort_by_key(|event| event.sequence);
    run.lifecycle.snapshots = manifest
        .execution_snapshots
        .iter()
        .map(|(id, record)| Ok((id.clone(), read_record(&records, record)?)))
        .collect::<Result<_, ReadFailure>>()?;
    for (id, index) in &manifest.steps {
        let mut step: WorkflowStep = read_record(&records, &index.record)?;
        if step.id != *id {
            return Err("workflow step identity mismatch".to_string().into());
        }
        step.attempts = index
            .attempts
            .iter()
            .map(|record| read_record(&records, record))
            .collect::<Result<_, _>>()?;
        run.steps.insert(id.clone(), step);
    }
    run.result = manifest
        .result
        .as_ref()
        .map(|record| read_record(&records, record))
        .transpose()?;
    let usage = manifest
        .usage
        .iter()
        .map(|(thread, record)| Ok((thread.clone(), read_record(&records, record)?)))
        .collect::<Result<_, ReadFailure>>()?;
    Ok(Loaded {
        run,
        manifest: Some(manifest),
        usage,
    })
}

pub(super) fn header(root: &AbsolutePathBuf, id: &str) -> Result<(WorkflowRun, u32), String> {
    let value: Value = read_json(&path(root, id)?)?;
    if value.get("storageVersion").and_then(Value::as_u64) == Some(2) {
        let manifest: Manifest =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        Ok((manifest.header, manifest.steps.len() as u32))
    } else {
        let mut run: WorkflowRun =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        run.format_version = 1;
        let count = run.steps.len() as u32;
        Ok((run, count))
    }
}

pub(super) fn write(
    root: &AbsolutePathBuf,
    run: &WorkflowRun,
    prior: Option<&Loaded>,
    usage: &BTreeMap<String, UsageObservation>,
) -> Result<Manifest, String> {
    let records = root.join(&run.id).join("records");
    fs::create_dir_all(&records).map_err(|error| error.to_string())?;
    let previous = prior.and_then(|prior| prior.manifest.as_ref());
    let unchanged_definition = prior.is_some_and(|prior| {
        prior.run.source == run.source
            && prior.run.input == run.input
            && prior.run.nested_workflows == run.nested_workflows
            && prior.run.budget_observations == run.budget_observations
    });
    let definition = if unchanged_definition {
        previous.ok_or("missing V2 manifest")?.definition.clone()
    } else {
        write_record(
            &records,
            &Definition {
                source: run.source.clone(),
                input: run.input.clone(),
                nested_workflows: run.nested_workflows.clone(),
                budget_observations: run.budget_observations.clone(),
            },
        )?
    };
    let mut steps = BTreeMap::new();
    for (id, step) in &run.steps {
        let old_step = prior.and_then(|prior| prior.run.steps.get(id));
        let old_index = previous.and_then(|manifest| manifest.steps.get(id));
        let mut detail = step.clone();
        detail.attempts.clear();
        let unchanged = old_step.is_some_and(|old| {
            let mut old = old.clone();
            old.attempts.clear();
            old == detail
        });
        let record = if unchanged {
            old_index
                .ok_or("missing workflow step index")?
                .record
                .clone()
        } else {
            write_record(&records, &detail)?
        };
        let attempts = step
            .attempts
            .iter()
            .enumerate()
            .map(|(index, attempt)| {
                if old_step.and_then(|old| old.attempts.get(index)) == Some(attempt) {
                    return old_index
                        .and_then(|old| old.attempts.get(index))
                        .cloned()
                        .ok_or("missing attempt index".into());
                }
                write_record(&records, attempt)
            })
            .collect::<Result<_, String>>()?;
        steps.insert(id.clone(), StepIndex { record, attempts });
    }
    let result = match &run.result {
        Some(result) if prior.is_some_and(|prior| prior.run.result.as_ref() == Some(result)) => {
            previous.and_then(|manifest| manifest.result.clone())
        }
        Some(result) => Some(write_record(&records, result)?),
        None => None,
    };
    let mut usage_records = BTreeMap::new();
    for (thread, observation) in usage {
        let record = if prior.and_then(|prior| prior.usage.get(thread)) == Some(observation) {
            previous
                .and_then(|manifest| manifest.usage.get(thread))
                .cloned()
                .ok_or("missing usage index")?
        } else {
            write_record(&records, observation)?
        };
        usage_records.insert(thread.clone(), record);
    }
    let mut header = run.clone();
    let steering = if run.steering.version == 1 || !run.steering.revisions.is_empty() {
        Some(super::steering_files::write(
            &records,
            &run.steering,
            prior.map(|prior| &prior.run.steering),
            previous.and_then(|previous| previous.steering.as_ref()),
        )?)
    } else {
        None
    };
    header.steering = crate::steering::WorkflowSteeringState {
        version: run.steering.version,
        state: run.steering.state.clone(),
        ..Default::default()
    };
    let mut lifecycle_events = BTreeMap::new();
    for event in &run.lifecycle.events {
        let record = match previous.and_then(|manifest| manifest.lifecycle_events.get(&event.id)) {
            Some(record) => record.clone(),
            None => write_record(&records, event)?,
        };
        lifecycle_events.insert(event.id.clone(), record);
    }
    let mut execution_snapshots = previous.map_or_else(BTreeMap::new, |manifest| {
        manifest.execution_snapshots.clone()
    });
    for (id, snapshot) in &run.lifecycle.snapshots {
        let record = match previous.and_then(|manifest| manifest.execution_snapshots.get(id)) {
            Some(record) => record.clone(),
            None => write_record(&records, snapshot)?,
        };
        execution_snapshots.insert(id.clone(), record);
    }
    let (execution_steps, execution_pins) =
        super::executions::pin(&records, run, prior, &definition, &steps)?;
    header.lifecycle.events.clear();
    header.lifecycle.snapshots.clear();
    header.source.clear();
    header.input = Value::Null;
    header.steps.clear();
    header.result = None;
    header.nested_workflows.clear();
    header.budget_observations.clear();
    let previous_records = previous.map_or_else(Vec::new, |manifest| {
        // Immutable archive pins are already roots in the new generation. Avoid
        // duplicating their monotonically retained union in every previous list.
        references(manifest)
            .into_iter()
            .filter(|record| !execution_pins.contains(record))
            .collect()
    });
    let manifest = Manifest {
        storage_version: 2,
        revision: uuid::Uuid::new_v4().to_string(),
        previous_records,
        header,
        definition,
        steps,
        result,
        usage: usage_records,
        lifecycle_events,
        execution_snapshots,
        execution_steps,
        execution_pins,
        steering,
    };
    atomic_json(&path(root, &run.id)?, &manifest)?;
    if let Some(previous) = previous {
        let retained = references(&manifest)
            .into_iter()
            .chain(manifest.previous_records.iter().cloned())
            .collect::<BTreeSet<_>>();
        // Only obsolete references from the prior committed generation are
        // eligible. Failed unpublished records are handled on the next open.
        for record in &previous.previous_records {
            if !retained.contains(record) {
                let _ = fs::remove_file(records.join(format!("{record}.json")));
            }
        }
    }
    Ok(manifest)
}

fn references(manifest: &Manifest) -> BTreeSet<String> {
    let mut references = BTreeSet::from([manifest.definition.clone()]);
    for step in manifest.steps.values() {
        references.insert(step.record.clone());
        references.extend(step.attempts.iter().cloned());
    }
    references.extend(manifest.result.iter().cloned());
    references.extend(manifest.usage.values().cloned());
    references.extend(manifest.lifecycle_events.values().cloned());
    references.extend(manifest.execution_snapshots.values().cloned());
    references.extend(manifest.execution_steps.values().cloned());
    references.extend(manifest.execution_pins.iter().cloned());
    if let Some(index) = &manifest.steering {
        references.extend(super::steering_files::references(index));
    }
    references
}

pub(super) fn cleanup_on_open(root: &AbsolutePathBuf, id: &str) -> Result<(), String> {
    let value: Value = read_json(&path(root, id)?)?;
    if value.get("storageVersion").and_then(Value::as_u64) != Some(2) {
        return Ok(());
    }
    let manifest: Manifest = serde_json::from_value(value).map_err(|error| error.to_string())?;
    let records = root.join(id).join("records");
    let retained = references(&manifest)
        .into_iter()
        .chain(manifest.previous_records.iter().cloned())
        .collect::<BTreeSet<_>>();
    for entry in fs::read_dir(&records).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
            && let Some(record) = path.file_stem().and_then(|record| record.to_str())
            && uuid::Uuid::parse_str(record).is_ok()
            && !retained.contains(record)
        {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

pub(super) fn read_record<T: DeserializeOwned>(
    root: &AbsolutePathBuf,
    record: &str,
) -> Result<T, ReadFailure> {
    uuid::Uuid::parse_str(record).map_err(|_| "invalid workflow record reference".to_string())?;
    let bytes = fs::read(root.join(format!("{record}.json"))).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ReadFailure::MissingRecord(error.to_string())
        } else {
            ReadFailure::Other(error.to_string())
        }
    })?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(ReadFailure::Other(
            "workflow record exceeds size limit".into(),
        ));
    }
    serde_json::from_slice(&bytes).map_err(|error| ReadFailure::Other(error.to_string()))
}

pub(super) fn write_record(
    root: &AbsolutePathBuf,
    value: &impl Serialize,
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    atomic_json(&root.join(format!("{id}.json")), value)?;
    Ok(id)
}

pub(super) fn atomic_json(path: &AbsolutePathBuf, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err("workflow record exceeds size limit".into());
    }
    let parent = path.parent().ok_or("workflow record has no parent")?;
    fs::create_dir_all(&parent).map_err(|error| error.to_string())?;
    let temporary = parent.join(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        fs::File::open(&parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result.map_err(|error| error.to_string())
}
