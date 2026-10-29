//! Attempt-scoped managed checkouts survive failure and are never merged or cleaned up here.

use crate::session::step_context::StepContext;
use crate::thread_spawn::ThreadSpawnWorkspace;
use codex_dynamic_workflows::AgentRequest;
use codex_dynamic_workflows::AgentStartMode;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_worktree::CreateWorktree;
use codex_worktree::WorktreeManager;
use codex_worktree::WorktreeSettings;
use serde::Deserialize;
use serde::Serialize;
use std::io::Write;

#[derive(Deserialize, Serialize)]
struct CheckoutReceipt {
    root: AbsolutePathBuf,
    cwd: AbsolutePathBuf,
    source_root: AbsolutePathBuf,
    #[serde(default)]
    logical_source_root: Option<AbsolutePathBuf>,
}

pub(super) async fn prepare(
    step: &StepContext,
    request: &AgentRequest,
    mode: &AgentStartMode,
) -> Result<ThreadSpawnWorkspace, String> {
    // The managed-worktree implementation is host-local. Never substitute a
    // shared host cwd for an unsupported executor or an unresolved attachment.
    let environment = step
        .environments
        .single_local_environment()
        .ok_or("worktree isolation requires one ready local execution environment")?;
    let source = environment
        .cwd()
        .to_abs_path()
        .map_err(|error| error.to_string())?;
    let source_root = codex_git_utils::get_git_repo_root(source.as_path())
        .ok_or("worktree isolation requires a Git repository")?;
    let source_root = AbsolutePathBuf::try_from(source_root).map_err(|error| error.to_string())?;
    let sandbox = environment.sandbox_context(None);
    let context = sandbox.policy_context();
    let grants = step
        .turn
        .granted_permissions(&environment.selection().environment_id);
    let policy = codex_sandboxing::policy_transforms::effective_file_system_sandbox_policy(
        &environment
            .permission_profile()
            .file_system_sandbox_policy(),
        grants.as_ref(),
    );
    for path in [&source_root, &source_root.join(".git")] {
        if !policy.can_read_path(
            &codex_utils_path_uri::PathUri::from_abs_path(path),
            &context,
        ) {
            return Err(
                "the workflow caller's filesystem policy does not permit reading this repository"
                    .into(),
            );
        }
    }
    let config = step.turn.config.clone();
    if config.active_project.is_untrusted() {
        return Err("cannot create a worktree from an explicitly untrusted source".into());
    }
    // Allocation settings belong to the host; project or agent-profile config
    // cannot redirect the managed checkout root.
    let host = crate::config::load_config_toml_with_layer_stack(
        &config.codex_home,
        /*cwd*/ None,
        Vec::new(),
        codex_config::ConfigLoadOptions::default(),
    )
    .await
    .map_err(|error| error.to_string())?;
    let settings = WorktreeSettings::for_cli(&config.codex_home, host.config_toml.desktop.as_ref())
        .map_err(|error| error.to_string())?;
    let request = request.clone();
    let mode = mode.clone();
    tokio::task::spawn_blocking(move || {
        prepare_checkout(settings, source, source_root, &request, mode)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn prepare_checkout(
    mut settings: WorktreeSettings,
    source: AbsolutePathBuf,
    logical_source_root: AbsolutePathBuf,
    request: &AgentRequest,
    mode: AgentStartMode,
) -> Result<ThreadSpawnWorkspace, String> {
    // Engine-generated UUIDs are also the filesystem ownership boundary.
    let run = uuid::Uuid::parse_str(&request.run_id).map_err(|error| error.to_string())?;
    let attempt = uuid::Uuid::parse_str(&request.attempt_id).map_err(|error| error.to_string())?;
    settings.root = settings
        .root
        .join("workflows")
        .join(run.to_string())
        .join(attempt.to_string());
    let receipt_path = settings.root.join("checkout.json");
    let allocation_path = settings.root.join("allocating");
    let manager = WorktreeManager::new(settings);
    // Repository discovery happens before creating an allocation directory.
    let existing = manager
        .list(source.as_path())
        .map_err(|error| error.to_string())?;
    let receipt = match std::fs::read(&receipt_path) {
        Ok(bytes) => {
            let receipt: CheckoutReceipt =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            let checkout = existing.iter().find(|checkout| checkout.root == receipt.root.as_path())
                .ok_or("the recorded workflow worktree is unavailable; explicit intervention is required")?;
            if checkout.cwd != receipt.cwd.as_path()
                || checkout.source_root != receipt.source_root.as_path()
            {
                return Err(
                    "workflow worktree receipt does not match its registered checkout".into(),
                );
            }
            receipt
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let checkout = match existing.as_slice() {
                [checkout] => checkout.clone(),
                [] if matches!(mode, AgentStartMode::Start) && !allocation_path.exists() => {
                    std::fs::create_dir_all(manager.settings().root.as_path())
                        .map_err(|error| error.to_string())?;
                    let allocating = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&allocation_path)
                        .map_err(|error| error.to_string())?;
                    allocating.sync_all().map_err(|error| error.to_string())?;
                    manager
                        .create(&CreateWorktree {
                            source_cwd: source.to_path_buf(),
                            base: None,
                        })
                        .map_err(|error| error.to_string())?
                }
                [] => return Err(
                    "workflow worktree allocation is unresolved; explicit intervention is required"
                        .into(),
                ),
                _ => return Err("workflow attempt has multiple managed checkouts".into()),
            };
            let absolute = |path: std::path::PathBuf| {
                AbsolutePathBuf::from_absolute_path(path).map_err(|error| error.to_string())
            };
            let receipt = CheckoutReceipt {
                root: absolute(checkout.root)?,
                cwd: absolute(checkout.cwd)?,
                source_root: absolute(checkout.source_root)?,
                logical_source_root: Some(logical_source_root.clone()),
            };
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&receipt_path)
                .map_err(|error| error.to_string())?;
            file.write_all(&serde_json::to_vec(&receipt).map_err(|error| error.to_string())?)
                .and_then(|()| file.sync_all())
                .map_err(|error| error.to_string())?;
            receipt
        }
        Err(error) => return Err(error.to_string()),
    };
    Ok(ThreadSpawnWorkspace {
        cwd: receipt.cwd,
        root: receipt.root,
        source_root: receipt.source_root,
        logical_source_root: receipt.logical_source_root.unwrap_or(logical_source_root),
    })
}
