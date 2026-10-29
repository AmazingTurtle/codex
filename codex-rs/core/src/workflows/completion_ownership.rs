//! Workflow ownership ends at the worker's first authoritative final turn.

use crate::agent::control::LocalAgentRuntime;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;

pub(crate) async fn owns_turn(
    runtime: &LocalAgentRuntime,
    child: ThreadId,
    path: &AgentPath,
    turn_id: &str,
) -> bool {
    let Ok(state) = runtime.execution_control.snapshot().await else {
        return false;
    };
    let Ok(root) = runtime.workflow_thread(state.root_thread_id).await else {
        return false;
    };
    let Some(service) = root.session.services.workflows.service.get() else {
        return false;
    };
    if !service.owns_child(child, path).await {
        return false;
    }
    if let Ok(thread) = runtime.workflow_thread(child).await {
        let snapshot = thread.session.workflow_usage_snapshot().await;
        if !snapshot.restored
            && (snapshot.owned_turns.iter().any(|owned| owned == turn_id)
                || snapshot
                    .terminal
                    .as_ref()
                    .is_some_and(|terminal| terminal.turn_id == turn_id))
        {
            return true;
        }
    }
    let mut offset = 0;
    loop {
        let Ok(runs) = service.list(offset, codex_dynamic_workflows::MAX_PAGE_SIZE) else {
            return false;
        };
        if runs
            .iter()
            .flat_map(|run| run.steps.values())
            .flat_map(|step| &step.attempts)
            .any(|attempt| {
                attempt.child_thread_id.as_deref() == Some(child.to_string().as_str())
                    && attempt.owned_turn_ids.iter().any(|owned| owned == turn_id)
            })
        {
            return true;
        }
        if runs.len() < codex_dynamic_workflows::MAX_PAGE_SIZE {
            return false;
        }
        offset = offset.saturating_add(runs.len());
    }
}
