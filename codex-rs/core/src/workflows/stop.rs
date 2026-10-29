//! Interrupt only the current turns proven to belong to this run's accepted attempts.

use super::host::Children;
use crate::session::session::Session;
use codex_protocol::protocol::TurnAbortReason;
use std::sync::Arc;
use std::sync::PoisonError;

pub(super) async fn children(
    owner: &Session,
    children: &Children,
    run_id: &str,
    scopes: &super::usage_ownership::Scopes,
) -> Result<(), String> {
    if owner.services.local_agent_runtime.shutdown.is_cancelled() {
        // Tree shutdown already joins every Session teardown, including its
        // terminal write and writer close. A competing targeted abort could
        // take its task before that teardown and write the terminal too late.
        return Ok(());
    }
    let tree = &owner.services.local_agent_runtime.execution_control;
    let current = super::usage_ownership::members(owner, children, run_id, scopes).await?;
    let direct = children
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .keys()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    // Preserve the native FIFO fence: an accepted child's initial mail can be
    // queued before its task has registered, including while the tree is paused.
    for (session, _, selected) in current {
        if selected.complete || !direct.contains(&session.thread_id) {
            continue;
        }
        if let Some(turn_id) = selected.owned_turn_id {
            super::usage_ownership::stop_selected(scopes, session.thread_id);
            session
                .abort_turn_if_active(&turn_id, TurnAbortReason::Interrupted, /*error*/ None)
                .await;
        } else if let Ok(thread) = owner
            .services
            .local_agent_runtime
            .workflow_thread(session.thread_id)
            .await
        {
            cancel_initial(&thread, scopes).await?;
        }
    }
    let mut membership = tree.subscribe_members();
    loop {
        membership.borrow_and_update();
        let mut targets = Vec::new();
        let mut pending = Vec::new();
        for (session, _, selected) in
            super::usage_ownership::members(owner, children, run_id, scopes).await?
        {
            if selected.complete {
                continue;
            }
            let Some(turn_id) = selected.owned_turn_id else {
                if session.current_agent_status()
                    == codex_protocol::protocol::AgentStatus::PendingInit
                    && let Ok(thread) = owner
                        .services
                        .local_agent_runtime
                        .workflow_thread(session.thread_id)
                        .await
                {
                    pending.push(thread);
                }
                continue;
            };
            // The shared selector has checked this review's actual parent turn,
            // or this native thread's unsealed first task. Revalidate its ID at abort.
            targets.push((session, turn_id));
        }
        // Stop parent producers before their descendants. The existing abort
        // path settles registered tools; a new member forces another snapshot.
        if targets.is_empty() && pending.is_empty() && !membership.has_changed().unwrap_or(false) {
            return Ok(());
        }
        for (session, turn) in targets {
            super::usage_ownership::stop_selected(scopes, session.thread_id);
            session
                .abort_turn_if_active(&turn, TurnAbortReason::Interrupted, /*error*/ None)
                .await;
        }
        for thread in pending {
            cancel_initial(&thread, scopes).await?;
        }
    }
}

async fn cancel_initial(
    thread: &Arc<crate::CodexThread>,
    scopes: &super::usage_ownership::Scopes,
) -> Result<(), String> {
    thread
        .update_thread_settings(codex_protocol::protocol::ThreadSettingsOverrides::default())
        .await
        .map_err(|error| error.to_string())?;
    let session = &thread.session;
    match session
        .cancel_pending_workflow_start(|| {
            super::usage_ownership::stop_selected(scopes, session.thread_id)
        })
        .await
        .map_err(|error| error.to_string())?
    {
        crate::session::PendingWorkflowCancellation::Registered { turn_id } => {
            super::usage_ownership::stop_selected(scopes, session.thread_id);
            session
                .abort_turn_if_active(&turn_id, TurnAbortReason::Interrupted, /*error*/ None)
                .await;
        }
        crate::session::PendingWorkflowCancellation::Cancelled
        | crate::session::PendingWorkflowCancellation::Sealed => {}
    }
    Ok(())
}
