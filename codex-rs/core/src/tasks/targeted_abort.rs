//! Select and remove one task atomically; ordinary interruption retains resumable ownership.

use crate::session::session::Session;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::TurnAbortReason;
use std::sync::Arc;

enum OwnershipOnAbort {
    Preserve,
    RetireWorkflow,
}

impl Session {
    pub(crate) async fn abort_turn_if_active(
        self: &Arc<Self>,
        turn_id: &str,
        reason: TurnAbortReason,
        error: Option<ErrorEvent>,
    ) -> bool {
        self.abort_selected_turn(turn_id, reason, error, OwnershipOnAbort::Preserve)
            .await
    }

    pub(crate) async fn abort_workflow_turn_if_active(self: &Arc<Self>, turn_id: &str) -> bool {
        self.abort_selected_turn(
            turn_id,
            TurnAbortReason::Interrupted,
            /*error*/ None,
            OwnershipOnAbort::RetireWorkflow,
        )
        .await
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "serialize owned-range retirement, wake invalidation, and active-turn removal with turn admission"
    )]
    async fn abort_selected_turn(
        self: &Arc<Self>,
        turn_id: &str,
        reason: TurnAbortReason,
        error: Option<ErrorEvent>,
        ownership: OwnershipOnAbort,
    ) -> bool {
        let turn_start = self.turn_start_lock.lock().await;
        let active_turn = {
            let mut active = self.active_turn.lock().await;
            if active
                .as_ref()
                .and_then(|turn| turn.task.as_ref())
                .is_some_and(|task| task.turn_context.sub_id == turn_id)
            {
                if matches!(ownership, OwnershipOnAbort::RetireWorkflow) {
                    self.retire_workflow_owned_turns();
                }
                if matches!(
                    reason,
                    TurnAbortReason::Interrupted | TurnAbortReason::BudgetLimited
                ) {
                    self.mark_interrupted();
                    let generation = self
                        .wake_generation
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                        + 1;
                    self.wake_tracker.invalidate_commands(generation);
                    self.discard_interrupted_execution_input(generation).await;
                }
                active.take()
            } else {
                None
            }
        };
        let Some(active_turn) = active_turn else {
            return false;
        };
        drop(turn_start);
        self.finish_turn_abort(active_turn, reason, error).await;
        true
    }
}
