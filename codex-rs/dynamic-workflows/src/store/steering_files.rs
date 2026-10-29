//! Revision definitions and control receipts use independent immutable records.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::files;
use crate::steering::WorkflowSteeringState;

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct SteeringIndex {
    state: WorkflowSteeringState,
    revisions: BTreeMap<String, String>,
    commands: Vec<String>,
    effects: BTreeMap<String, String>,
    mutations: BTreeMap<String, String>,
    effects_by_revision: BTreeMap<String, BTreeMap<String, String>>,
}

pub(super) fn write(
    records: &AbsolutePathBuf,
    state: &WorkflowSteeringState,
    old: Option<&WorkflowSteeringState>,
    prior: Option<&SteeringIndex>,
) -> Result<SteeringIndex, String> {
    let revisions = write_map(
        records,
        &state.revisions,
        old.map(|old| &old.revisions),
        prior.map(|prior| &prior.revisions),
    )?;
    let effects = write_map(
        records,
        &state.effects,
        old.map(|old| &old.effects),
        prior.map(|prior| &prior.effects),
    )?;
    let mutations = write_map(
        records,
        &state.mutations,
        old.map(|old| &old.mutations),
        prior.map(|prior| &prior.mutations),
    )?;
    let commands = state
        .commands
        .iter()
        .enumerate()
        .map(|(index, command)| {
            if old.and_then(|old| old.commands.get(index)) == Some(command) {
                return prior
                    .and_then(|prior| prior.commands.get(index))
                    .cloned()
                    .ok_or("workflow command index missing".into());
            }
            files::write_record(records, command)
        })
        .collect::<Result<_, String>>()?;
    let effects_by_revision = state
        .effects_by_revision
        .iter()
        .map(|(id, effects)| {
            Ok((
                id.clone(),
                write_map(
                    records,
                    effects,
                    old.and_then(|old| old.effects_by_revision.get(id)),
                    prior.and_then(|prior| prior.effects_by_revision.get(id)),
                )?,
            ))
        })
        .collect::<Result<_, String>>()?;
    let mut header = state.clone();
    header.revisions.clear();
    header.commands.clear();
    header.effects.clear();
    header.mutations.clear();
    header.effects_by_revision.clear();
    Ok(SteeringIndex {
        state: header,
        revisions,
        commands,
        effects,
        mutations,
        effects_by_revision,
    })
}

pub(super) fn read(
    records: &AbsolutePathBuf,
    index: &SteeringIndex,
) -> Result<WorkflowSteeringState, files::ReadFailure> {
    let mut state = index.state.clone();
    state.revisions = read_map(records, &index.revisions)?;
    state.commands = index
        .commands
        .iter()
        .map(|record| files::read_record(records, record))
        .collect::<Result<_, _>>()?;
    state.effects = read_map(records, &index.effects)?;
    state.mutations = read_map(records, &index.mutations)?;
    state.effects_by_revision = index
        .effects_by_revision
        .iter()
        .map(|(id, records_index)| Ok((id.clone(), read_map(records, records_index)?)))
        .collect::<Result<_, files::ReadFailure>>()?;
    Ok(state)
}

pub(super) fn references(index: &SteeringIndex) -> BTreeSet<String> {
    index
        .revisions
        .values()
        .chain(&index.commands)
        .chain(index.effects.values())
        .chain(index.mutations.values())
        .chain(
            index
                .effects_by_revision
                .values()
                .flat_map(BTreeMap::values),
        )
        .cloned()
        .collect()
}

fn write_map<T: Serialize + PartialEq>(
    records: &AbsolutePathBuf,
    values: &BTreeMap<String, T>,
    old: Option<&BTreeMap<String, T>>,
    prior: Option<&BTreeMap<String, String>>,
) -> Result<BTreeMap<String, String>, String> {
    values
        .iter()
        .map(|(id, value)| {
            let record = if old.and_then(|old| old.get(id)) == Some(value) {
                prior
                    .and_then(|prior| prior.get(id))
                    .cloned()
                    .ok_or("workflow steering index missing")?
            } else {
                files::write_record(records, value)?
            };
            Ok((id.clone(), record))
        })
        .collect()
}

fn read_map<T: DeserializeOwned>(
    records: &AbsolutePathBuf,
    values: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, T>, files::ReadFailure> {
    values
        .iter()
        .map(|(id, record)| Ok((id.clone(), files::read_record(records, record)?)))
        .collect()
}
