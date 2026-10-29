//! Native saved commands share the builtin popup and never send script sources to inference.
use super::SavedWorkflowRow;
use crate::bottom_pane::slash_commands::ServiceTierCommand;
use codex_app_server_protocol::WorkflowScope;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedCommand {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SavedCommandCandidate {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) scope: Option<WorkflowScope>,
}

pub(crate) fn candidates(rows: &[SavedWorkflowRow]) -> Vec<SavedCommandCandidate> {
    rows.iter()
        .filter(|row| !row.read_only)
        .filter_map(|row| {
            let metadata = row.metadata.as_ref()?;
            Some(SavedCommandCandidate {
                id: row.id.clone(),
                name: metadata.name.clone(),
                description: metadata.description.clone(),
                scope: row.scope,
            })
        })
        .collect()
}

#[derive(Debug)]
pub(crate) struct SavedCatalog {
    pub(crate) candidates: Vec<SavedCommandCandidate>,
    pub(crate) overflow: bool,
}

pub(crate) fn catalog(
    rows: &[SavedCommandCandidate],
    tiers: &[ServiceTierCommand],
) -> Vec<SavedCommand> {
    let mut grouped: BTreeMap<String, Vec<&SavedCommandCandidate>> = BTreeMap::new();
    for row in rows {
        grouped
            .entry(row.name.to_ascii_lowercase())
            .or_default()
            .push(row);
    }
    grouped
        .into_iter()
        .filter_map(|(name, rows)| {
            if reserved(&name, tiers) {
                return None;
            }
            if rows.iter().any(|row| row.name != rows[0].name) {
                return None;
            }
            let project: Vec<_> = rows
                .iter()
                .filter(|row| row.scope == Some(WorkflowScope::Project))
                .collect();
            let effective = if project.is_empty() {
                rows.iter().collect::<Vec<_>>()
            } else {
                project
            };
            if effective.len() != 1 {
                return None;
            }
            let row = effective[0];
            Some(SavedCommand {
                id: row.id.clone(),
                name: row.name.clone(),
                description: row.description.clone(),
            })
        })
        .collect()
}

pub(crate) fn reserved(name: &str, tiers: &[ServiceTierCommand]) -> bool {
    name.is_empty()
        || name.len() > 80
        || name
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
        || crate::slash_command::built_in_slash_commands()
            .iter()
            .any(|(builtin, _)| builtin.eq_ignore_ascii_case(name))
        || tiers
            .iter()
            .any(|command| command.name.eq_ignore_ascii_case(name))
        || name
            .to_ascii_lowercase()
            .strip_prefix('g')
            .and_then(|rest| rest.strip_suffix("al"))
            .is_some_and(|middle| !middle.is_empty() && middle.bytes().all(|byte| byte == b'o'))
}

pub(crate) fn command_notice(
    row: &SavedWorkflowRow,
    rows: &[SavedCommandCandidate],
    tiers: &[ServiceTierCommand],
) -> Option<String> {
    if row.read_only {
        return row.read_only_reason.clone();
    }
    let name = &row.metadata.as_ref()?.name;
    if reserved(name, tiers) {
        return Some(format!(
            "/{name} is reserved or unavailable as a command. Run this workflow from /workflows."
        ));
    }
    let candidates: Vec<_> = rows
        .iter()
        .filter(|candidate| candidate.name.eq_ignore_ascii_case(name))
        .collect();
    if candidates.iter().any(|candidate| candidate.name != *name) {
        return Some(format!(
            "/{name} has a case-insensitive command collision. Choose this scoped entry in /workflows."
        ));
    }
    let project: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.scope == Some(WorkflowScope::Project))
        .collect();
    let effective = if project.is_empty() {
        candidates.len()
    } else {
        project.len()
    };
    if effective > 1 {
        return Some(format!(
            "/{name} matches multiple saved workflows. Choose this scoped entry in /workflows."
        ));
    }
    if !project.is_empty() && row.scope != Some(WorkflowScope::Project) {
        return Some(format!(
            "/{name} uses the project workflow. Run this user entry from /workflows."
        ));
    }
    None
}

/// Absence and explicit JSON null stay distinct; non-JSON argument tails are literal input.
pub(crate) fn input(arguments: Option<&str>) -> serde_json::Value {
    match arguments {
        None => serde_json::json!({}),
        Some(arguments) => serde_json::from_str(arguments)
            .unwrap_or_else(|_| serde_json::Value::String(arguments.into())),
    }
}

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
