//! Rebinds an inherited native workspace while preserving and translating its denials.

use super::ThreadSpawnError;
use super::ThreadSpawnWorkspace;
use crate::config::Config;
use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::environment_selection::TurnEnvironmentState;
use codex_protocol::models::ManagedFileSystemPermissions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::PermissionProfileSnapshot;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::protocol::EnvironmentConfigState;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;

pub(super) fn apply(
    config: &mut Config,
    environments: &mut TurnEnvironmentSnapshot,
    workspace: &ThreadSpawnWorkspace,
) -> Result<(), ThreadSpawnError> {
    let remap = |root: &AbsolutePathBuf| {
        source_relative(root.as_path(), workspace).map(|relative| {
            relative.map_or_else(|| root.clone(), |relative| workspace.root.join(relative))
        })
    };
    let mut snapshot = config.permissions.permission_profile_state().snapshot();
    snapshot = translate_denials(&snapshot, workspace)?;
    config
        .permissions
        .set_permission_profile_from_session_snapshot(snapshot)
        .map_err(|error| ThreadSpawnError::Preparation(error.to_string()))?;
    config.workspace_roots = config
        .workspace_roots
        .iter()
        .map(remap)
        .collect::<Result<_, _>>()?;
    config
        .permissions
        .set_workspace_roots(config.workspace_roots.clone());
    config.cwd = workspace.cwd.clone();
    let [TurnEnvironmentState::Ready(environment)] = environments.environments.as_mut_slice()
    else {
        return Err(ThreadSpawnError::Preparation(
            "worktree isolation requires one ready local environment".into(),
        ));
    };
    if environment.environment.is_remote() {
        return Err(ThreadSpawnError::Preparation(
            "worktree isolation is unavailable for remote environments".into(),
        ));
    }
    environment.selection.cwd = PathUri::from_abs_path(&workspace.cwd);
    let remap_uri = |root: &PathUri| -> Result<PathUri, ThreadSpawnError> {
        let root = root
            .to_abs_path()
            .map_err(|error| ThreadSpawnError::Preparation(error.to_string()))?;
        Ok(PathUri::from_abs_path(&remap(&root)?))
    };
    environment.selection.workspace_roots = environment
        .selection
        .workspace_roots
        .iter()
        .map(remap_uri)
        .collect::<Result<_, _>>()?;
    if let EnvironmentConfigState::Ready(environment_config) = &mut environment.selection.config {
        environment_config.workspace_roots = environment_config
            .workspace_roots
            .iter()
            .map(remap_uri)
            .collect::<Result<_, _>>()?;
        environment_config.permission_profile =
            translate_denials(&environment_config.permission_profile, workspace)?;
    }
    Ok(())
}

fn translate_denials(
    snapshot: &PermissionProfileSnapshot,
    workspace: &ThreadSpawnWorkspace,
) -> Result<PermissionProfileSnapshot, ThreadSpawnError> {
    let mut profile = snapshot.permission_profile().clone();
    if let PermissionProfile::Managed {
        file_system: ManagedFileSystemPermissions::Restricted { entries, .. },
        ..
    } = &mut profile
    {
        let mut translated = Vec::new();
        for entry in entries
            .iter()
            .filter(|entry| entry.access == FileSystemAccessMode::Deny)
        {
            let path = match &entry.path {
                FileSystemPath::Path { path } => {
                    let path = path
                        .to_abs_path()
                        .map_err(|error| ThreadSpawnError::Preparation(error.to_string()))?;
                    let Some(relative) = source_relative(path.as_path(), workspace)? else {
                        continue;
                    };
                    FileSystemPath::Path {
                        path: PathUri::from_abs_path(&workspace.root.join(relative)),
                    }
                }
                FileSystemPath::GlobPattern { pattern } => {
                    let Some(relative) = source_relative(std::path::Path::new(pattern), workspace)?
                    else {
                        continue;
                    };
                    FileSystemPath::GlobPattern {
                        pattern: workspace
                            .root
                            .as_path()
                            .join(relative)
                            .to_str()
                            .ok_or_else(|| {
                                ThreadSpawnError::Preparation(
                                    "worktree denial glob must be valid UTF-8".into(),
                                )
                            })?
                            .to_string(),
                    }
                }
                FileSystemPath::Special { .. } => continue,
            };
            let mut denial = entry.clone();
            denial.path = path;
            translated.push(denial);
        }
        entries.extend(translated);
    }
    Ok(match snapshot.active_permission_profile() {
        Some(active) => PermissionProfileSnapshot::active_with_profile_workspace_roots(
            profile,
            active,
            snapshot.profile_workspace_roots().to_vec(),
        ),
        None => PermissionProfileSnapshot::legacy(profile),
    })
}

fn source_relative(
    path: &std::path::Path,
    workspace: &ThreadSpawnWorkspace,
) -> Result<Option<std::path::PathBuf>, ThreadSpawnError> {
    for anchor in [&workspace.logical_source_root, &workspace.source_root] {
        if let Ok(relative) = path.strip_prefix(anchor.as_path()) {
            return Ok(Some(relative.to_path_buf()));
        }
    }
    if !path.is_absolute() {
        return Ok(None);
    }
    // Resolve the existing prefix, retaining nonexistent/glob suffixes. A
    // denial through another source alias must protect its checkout counterpart.
    let mut existing = path;
    let mut suffix = std::path::PathBuf::new();
    loop {
        match dunce::canonicalize(existing) {
            Ok(canonical) => {
                if workspace.source_root.as_path().starts_with(&canonical)
                    && suffix
                        .to_string_lossy()
                        .chars()
                        .any(|character| matches!(character, '*' | '?' | '['))
                {
                    return Err(ThreadSpawnError::Preparation("cannot safely translate a source-spanning absolute denial glob into a worktree".into()));
                }
                return Ok(canonical
                    .strip_prefix(workspace.source_root.as_path())
                    .ok()
                    .map(|relative| relative.join(&suffix)));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name() else {
                    return Ok(None);
                };
                suffix = std::path::Path::new(name).join(suffix);
                let Some(parent) = existing.parent() else {
                    return Ok(None);
                };
                existing = parent;
            }
            Err(error) => {
                return Err(ThreadSpawnError::Preparation(format!(
                    "cannot resolve worktree policy path: {error}"
                )));
            }
        }
    }
}
