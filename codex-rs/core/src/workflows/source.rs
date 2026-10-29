//! Saved sources use a fixed catalog and the original caller's current read authority.

use crate::session::session::Session;
use codex_dynamic_workflows::ResolvedWorkflow;
use codex_dynamic_workflows::SavedWorkflow;
use codex_dynamic_workflows::WorkflowLibrary;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use std::sync::Arc;

pub(super) async fn load(
    caller: &Arc<Session>,
    library: &WorkflowLibrary,
    name: &str,
) -> Result<ResolvedWorkflow, String> {
    let saved = authorized_read(caller, library, name, Lookup::Executable).await?;
    Ok(ResolvedWorkflow {
        id: saved.id,
        source: saved.source,
    })
}

pub(super) async fn read(
    caller: &Arc<Session>,
    library: &WorkflowLibrary,
    id: &str,
) -> Result<SavedWorkflow, String> {
    authorized_read(caller, library, id, Lookup::Source).await
}

enum Lookup {
    Executable,
    Source,
}

async fn authorized_read(
    caller: &Arc<Session>,
    library: &WorkflowLibrary,
    identifier: &str,
    lookup: Lookup,
) -> Result<SavedWorkflow, String> {
    let turn = caller.new_default_turn().await;
    // Erase the deep MCP capture future before nesting it in saved-tool futures.
    let capture: futures::future::BoxFuture<'_, _> = Box::pin(
        caller
            .capture_speculative_step_context(turn, &caller.services.local_agent_runtime.shutdown),
    );
    let step = capture.await.map_err(|error| error.to_string())?;
    let environment = step
        .environments
        .single_local_environment()
        .ok_or("saved workflows require one ready local environment")?;
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
    let authorize = |path: &AbsolutePathBuf| {
        if policy.can_read_path(&PathUri::from_abs_path(path), &context) {
            Ok(())
        } else {
            Err(
                "the workflow caller's filesystem policy does not permit reading this saved script"
                    .into(),
            )
        }
    };
    match lookup {
        Lookup::Executable => library.resolve_with_read_guard(identifier, authorize),
        Lookup::Source => library.read_with_read_guard(identifier, authorize),
    }
}
