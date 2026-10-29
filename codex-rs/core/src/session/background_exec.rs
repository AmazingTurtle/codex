//! Delivers bounded process completions into active turns or starts an idle turn.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio_util::sync::CancellationToken;

use super::TurnInput as PendingTurnInput;
use super::session::Session;
use crate::state::ActiveTurn;
use crate::tasks::RegularTask;
use codex_history::ResponseItemEnvelope;
use codex_protocol::models::ResponseItem;

impl Session {
    /// Injects model input into an active turn or wakes the session when idle.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn reservation and input delivery must remain atomic"
    )]
    pub(crate) async fn inject_or_start(
        self: &Arc<Self>,
        input: Vec<ResponseItem>,
        wake_generation: u64,
        cancellation_token: &CancellationToken,
    ) {
        let _turn_start_guard = self.turn_start_lock.lock().await;
        if cancellation_token.is_cancelled()
            || self.wake_generation.load(Ordering::Acquire) != wake_generation
        {
            return;
        }
        let mut input = input
            .into_iter()
            .map(ResponseItemEnvelope::new)
            .map(PendingTurnInput::ResponseItem)
            .collect::<Vec<_>>();
        match self
            .services
            .local_agent_runtime
            .execution_control
            .queue_if_paused(
                crate::execution_control::QueuedSignal::new(self.thread_id, input.clone())
                    .with_wake_generation(wake_generation),
            )
            .await
        {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                tracing::error!(%error, "failed to preserve paused process completion");
                return;
            }
        }
        if let Err(error) = self.retain_accepted_input(&mut input).await {
            tracing::error!(%error, "failed to persist process completion");
            return;
        }
        let mut active = self.active_turn.lock().await;
        if let Some(active_turn) = active.as_mut() {
            self.input_queue
                .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                    active_turn.turn_state.as_ref(),
                    input,
                )
                .await;
            return;
        }
        *active = Some(ActiveTurn::default());
        drop(active);
        let turn_context = self.new_default_turn().await;
        self.maybe_emit_model_warnings_for_turn(turn_context.as_ref())
            .await;
        self.start_task(turn_context, input, RegularTask::new())
            .await;
    }
}
