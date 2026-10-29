//! Read-only activity for the whole independent conversation tree.

use super::TreeExecutionControl;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use std::sync::atomic::Ordering;

impl TreeExecutionControl {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "publish activity changes in observation order"
    )]
    pub(crate) async fn refresh_activity(&self) -> Result<()> {
        let _refresh = self.activity_refresh.lock().await;
        let active = self.can_pause().await?;
        if self.last_activity.swap(active, Ordering::AcqRel) != active {
            self.broadcast_state(self.snapshot().await?.state).await?;
        }
        Ok(())
    }

    pub(crate) async fn can_pause(&self) -> Result<bool> {
        if self.is_paused() {
            return Ok(false);
        }
        Ok(self.has_pending_activity().await? && !self.is_paused())
    }

    pub(super) async fn has_pending_activity(&self) -> Result<bool> {
        if self.inference_count.load(Ordering::Acquire) != 0 {
            return Ok(true);
        }
        {
            let storage = self.storage.lock().await;
            let storage = storage.as_ref().ok_or_else(|| {
                CodexErr::Fatal("execution control has not been initialized".into())
            })?;
            if storage
                .journal
                .signals
                .iter()
                .any(|signal| !signal.awaiting_finalization)
            {
                return Ok(true);
            }
        }
        for member in self.members() {
            if member.has_execution_activity().await? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl crate::session::session::Session {
    pub(crate) async fn refresh_execution_activity(&self) {
        if let Err(error) = self
            .services
            .local_agent_runtime
            .execution_control
            .refresh_activity()
            .await
        {
            tracing::warn!(%error, "failed to publish conversation activity");
        }
    }

    pub(crate) async fn has_execution_activity(&self) -> Result<bool> {
        let has_turn = self.active_turn.lock().await.is_some();
        if has_turn
            || self.wake_tracker.has_pending_work()
            || !self.async_hook_results.is_empty()
            || self.has_outstanding_durable_sleep()
            || self.input_queue.has_pending_mailbox_items().await
            || !self.list_background_terminals().await.is_empty()
        {
            return Ok(true);
        }
        self.services
            .workflows
            .has_pending_work()
            .map_err(CodexErr::Fatal)
    }
}
