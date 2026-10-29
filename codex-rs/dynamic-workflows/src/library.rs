use std::collections::BTreeMap;
use std::fs;
use std::io::Write;

use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;

use crate::MAX_PAGE_SIZE;
use crate::MAX_SOURCE_BYTES;
use crate::ParsedScript;
use crate::ScriptMetadata;
use crate::WORKFLOW_FORMAT_VERSION;
use crate::parse_script;
use crate::types::validate_source;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowScope {
    Project,
    User,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedWorkflow {
    pub id: String,
    pub name: String,
    pub scope: WorkflowScope,
    pub source: String,
    pub metadata: Option<ScriptMetadata>,
    pub read_only: bool,
    pub read_only_reason: Option<String>,
    pub format_version: u32,
}

/// Saved sources are inspected without execution; unique project names take precedence.
pub struct WorkflowLibrary {
    project: AbsolutePathBuf,
    user: AbsolutePathBuf,
}

impl WorkflowLibrary {
    pub fn new(project_root: AbsolutePathBuf, codex_home: AbsolutePathBuf) -> Self {
        Self {
            project: project_root,
            user: codex_home,
        }
    }

    /// Host-local watch roots; absent libraries stay absent until the user saves a script.
    pub fn watch_roots(&self) -> Result<Vec<AbsolutePathBuf>, String> {
        [WorkflowScope::Project, WorkflowScope::User]
            .into_iter()
            .map(|scope| self.validated_root(scope))
            .collect()
    }

    /// Returns the validated host-local destination for a policy preflight.
    pub fn save_target(&self, name: &str, scope: WorkflowScope) -> Result<AbsolutePathBuf, String> {
        validate_name(name)?;
        self.path(name, scope)
    }

    /// Returns the validated host-local saved file for a deletion policy preflight.
    pub fn delete_target(&self, id: &str) -> Result<AbsolutePathBuf, String> {
        let (scope, name) = parse_id(id)?;
        self.path(name, scope)
    }

    pub fn resolve(&self, name: &str) -> Result<SavedWorkflow, String> {
        self.resolve_with_read_guard(name, |_| Ok(()))
    }

    /// Check the caller's read policy before reading each candidate source.
    /// A denied candidate fails closed; scoped IDs avoid scanning unrelated files.
    pub fn resolve_with_read_guard(
        &self,
        name: &str,
        mut authorize: impl FnMut(&AbsolutePathBuf) -> Result<(), String>,
    ) -> Result<SavedWorkflow, String> {
        if name.starts_with("project:") || name.starts_with("user:") {
            return executable(self.read_with_read_guard(name, &mut authorize)?);
        }
        validate_name(name)?;
        for scope in [WorkflowScope::Project, WorkflowScope::User] {
            let mut matches = Vec::new();
            for id in self.ids(scope)? {
                let saved = self.inspect_with_read_guard(&id, &mut authorize)?;
                if !saved.read_only
                    && saved
                        .metadata
                        .as_ref()
                        .is_some_and(|meta| meta.name == name)
                {
                    matches.push(saved);
                }
            }
            if matches.len() > 1 {
                return Err(format!(
                    "saved workflow name '{name}' is ambiguous; choose a scoped workflow in /workflows"
                ));
            }
            if let Some(saved) = matches.pop() {
                return Ok(saved);
            }
        }
        Err(format!(
            "no executable saved workflow named '{name}'; inspect /workflows for read-only scripts"
        ))
    }

    pub fn read(&self, id: &str) -> Result<SavedWorkflow, String> {
        self.read_with_read_guard(id, |_| Ok(()))
    }

    /// Read a source snapshot only after the host accepts its validated local path.
    pub fn read_with_read_guard(
        &self,
        id: &str,
        mut authorize: impl FnMut(&AbsolutePathBuf) -> Result<(), String>,
    ) -> Result<SavedWorkflow, String> {
        let (scope, name) = parse_id(id)?;
        let path = self.path(name, scope)?;
        authorize(&path)?;
        if fs::metadata(&path).map_err(|err| err.to_string())?.len() > MAX_SOURCE_BYTES as u64 {
            return Err("saved workflow exceeds source limit".into());
        }
        let source = fs::read_to_string(path).map_err(|err| err.to_string())?;
        validate_source(&source)?;
        let (metadata, read_only_reason, format_version) = match parse_script(&source) {
            Ok(ParsedScript::V2(compiled)) => {
                (Some(compiled.metadata), None, WORKFLOW_FORMAT_VERSION)
            }
            Ok(ParsedScript::LegacyReadOnly) => (
                None,
                Some(
                    "Legacy workflow: add a literal workflow.meta declaration before running."
                        .into(),
                ),
                1,
            ),
            Err(error) => (None, Some(format!("Invalid workflow: {error}")), 0),
        };
        Ok(SavedWorkflow {
            id: id.to_string(),
            name: name.to_string(),
            scope,
            source,
            metadata,
            read_only: read_only_reason.is_some(),
            read_only_reason,
            format_version,
        })
    }

    pub fn list(&self, offset: usize, limit: usize) -> Result<Vec<SavedWorkflow>, String> {
        let mut entries = BTreeMap::new();
        for scope in [WorkflowScope::Project, WorkflowScope::User] {
            for id in self.ids(scope)? {
                entries.insert(id.clone(), id);
            }
        }
        entries
            .into_values()
            .skip(offset)
            .take(limit.min(MAX_PAGE_SIZE))
            .map(|id| self.inspect_with_read_guard(&id, &mut |_| Ok(())))
            .collect()
    }

    pub fn save(
        &self,
        name: &str,
        source: &str,
        scope: WorkflowScope,
    ) -> Result<SavedWorkflow, String> {
        validate_name(name)?;
        validate_source(source)?;
        crate::compile_script(source)?;
        let root = self.validated_root(scope)?;
        let path = self.path(name, scope)?;
        fs::create_dir_all(&root).map_err(|err| err.to_string())?;
        let mut file = tempfile::NamedTempFile::new_in(&root).map_err(|err| err.to_string())?;
        file.write_all(source.as_bytes())
            .and_then(|()| file.as_file().sync_all())
            .map_err(|err| err.to_string())?;
        // Publish a complete file without overwriting a user script or symlink.
        file.persist_noclobber(path)
            .map_err(|err| err.to_string())?;
        let prefix = match scope {
            WorkflowScope::Project => "project",
            WorkflowScope::User => "user",
        };
        self.read(&format!("{prefix}:{name}"))
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let (scope, name) = parse_id(id)?;
        fs::remove_file(self.path(name, scope)?).map_err(|err| err.to_string())
    }

    fn path(&self, name: &str, scope: WorkflowScope) -> Result<AbsolutePathBuf, String> {
        let path = self.validated_root(scope)?.join(format!("{name}.js"));
        reject_symlink(&path)?;
        Ok(path)
    }

    fn inspect_with_read_guard(
        &self,
        id: &str,
        authorize: &mut impl FnMut(&AbsolutePathBuf) -> Result<(), String>,
    ) -> Result<SavedWorkflow, String> {
        let (scope, name) = parse_id(id)?;
        let path = self.path(name, scope)?;
        authorize(&path)?;
        let length = fs::metadata(&path)
            .map_err(|error| error.to_string())?
            .len();
        if length == 0 || length > MAX_SOURCE_BYTES as u64 {
            return Ok(SavedWorkflow {
                id: id.into(),
                name: name.into(),
                scope,
                source: String::new(),
                metadata: None,
                read_only: true,
                format_version: 0,
                read_only_reason: Some(if length == 0 {
                    "Saved workflow source is empty.".into()
                } else {
                    "Saved workflow source exceeds the 24KiB limit.".into()
                }),
            });
        }
        self.read(id)
    }

    fn ids(&self, scope: WorkflowScope) -> Result<Vec<String>, String> {
        let directory = match fs::read_dir(self.validated_root(scope)?) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.to_string()),
        };
        let prefix = match scope {
            WorkflowScope::Project => "project",
            WorkflowScope::User => "user",
        };
        let mut ids = Vec::new();
        for entry in directory {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.extension().is_some_and(|extension| extension == "js")
                && let Some(name) = path.file_stem().and_then(|name| name.to_str())
                && validate_name(name).is_ok()
            {
                self.path(name, scope)?;
                ids.push(format!("{prefix}:{name}"));
            }
        }
        ids.sort();
        Ok(ids)
    }

    fn validated_root(&self, scope: WorkflowScope) -> Result<AbsolutePathBuf, String> {
        // Configured project/CODEX_HOME anchors may themselves be trusted aliases.
        // Only managed descendants are prevented from redirecting library access.
        let root = match scope {
            WorkflowScope::Project => {
                let directory = self.project.join(".codex");
                reject_symlink(&directory)?;
                directory.join("workflows")
            }
            WorkflowScope::User => self.user.join("workflows"),
        };
        reject_symlink(&root)?;
        Ok(root)
    }
}

fn executable(saved: SavedWorkflow) -> Result<SavedWorkflow, String> {
    if saved.read_only {
        Err(saved
            .read_only_reason
            .unwrap_or_else(|| "saved workflow is read-only".into()))
    } else {
        Ok(saved)
    }
}

fn reject_symlink(path: &AbsolutePathBuf) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err("saved workflow library paths cannot be symlinks".into())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 80
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "workflow name must contain 1..=80 ASCII letters, digits, hyphens or underscores"
                .into(),
        );
    }
    Ok(())
}

fn parse_id(id: &str) -> Result<(WorkflowScope, &str), String> {
    let (scope, name) = id
        .split_once(':')
        .ok_or("saved workflow id must include its scope")?;
    validate_name(name)?;
    let scope = match scope {
        "project" => WorkflowScope::Project,
        "user" => WorkflowScope::User,
        _ => return Err("invalid workflow scope".into()),
    };
    Ok((scope, name))
}

#[cfg(test)]
#[path = "library_tests.rs"]
mod tests;
