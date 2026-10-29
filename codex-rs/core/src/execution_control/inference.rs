//! Tracks open inference separately from the producers and waiters in its turn.

use super::TreeExecutionControl;
use codex_protocol::protocol::ExecutionControlState;
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub(crate) struct InferenceLease {
    pub(super) control: Arc<TreeExecutionControl>,
}

impl InferenceLease {
    pub(crate) async fn paused(&self) {
        let mut state = self.control.state.subscribe();
        while *state.borrow_and_update() == ExecutionControlState::Running {
            if state.changed().await.is_err() {
                break;
            }
        }
    }
}

impl Drop for InferenceLease {
    fn drop(&mut self) {
        self.control.inference_count.fetch_sub(1, Ordering::AcqRel);
        self.control.settled.notify_waiters();
    }
}
