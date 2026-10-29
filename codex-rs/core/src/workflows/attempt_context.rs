//! Native attempt metadata uses only the turns captured before its first final.

use crate::session::WorkflowUsageSnapshot;
use crate::session::session::Session;
use codex_dynamic_workflows::WorkflowHostEvent;
use codex_dynamic_workflows::WorkflowObserver;

pub(super) async fn publish(
    session: &Session,
    snapshot: &WorkflowUsageSnapshot,
    attempt_id: &str,
    observer: &WorkflowObserver,
) -> Result<(), String> {
    // A resumed runtime's new turns may belong to a later human task. The
    // persisted attempt retains its previously proven range without guessing.
    if snapshot.restored {
        return Ok(());
    }
    let mut turns = snapshot.owned_turns.clone();
    if let Some(terminal) = &snapshot.terminal
        && !turns.contains(&terminal.turn_id)
    {
        turns.push(terminal.turn_id.clone());
    }
    let resolved = match session.active_turn_context_and_strict_auto_review().await {
        Some((turn, settings, _, _)) if turns.contains(&turn.sub_id) => Some((
            settings.model_info.slug.clone(),
            settings
                .effective_reasoning_effort()
                .map(|effort| effort.to_string()),
        )),
        _ => session
            .reference_context_item()
            .await
            .filter(|context| {
                context
                    .turn_id
                    .as_ref()
                    .is_some_and(|id| turns.contains(id))
            })
            .map(|context| {
                (
                    context.model,
                    context.effort.map(|effort| effort.to_string()),
                )
            }),
    };
    let (resolved_model, reasoning_effort) = match resolved {
        Some((model, effort)) => (Some(model), effort),
        None => (None, None),
    };
    observer(WorkflowHostEvent::AttemptContext {
        attempt_id: attempt_id.to_string(),
        owned_turn_ids: turns,
        final_turn_id: snapshot
            .terminal
            .as_ref()
            .filter(|_| !snapshot.restored)
            .map(|terminal| terminal.turn_id.clone()),
        resolved_model,
        reasoning_effort,
    })
}
