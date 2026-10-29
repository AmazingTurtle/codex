//! Conversation execution state updates existing activity and configured status surfaces.

use super::*;
use codex_protocol::protocol::ExecutionControlState;

impl ChatWidget {
    pub(crate) fn set_execution_control(
        &mut self,
        state: Option<ExecutionControlState>,
        can_pause: bool,
    ) {
        if self.status_state.execution == state && self.status_state.can_pause == can_pause {
            return;
        }
        self.status_state.execution = state;
        self.status_state.can_pause = can_pause;
        self.bottom_pane.set_execution_control(state, can_pause);
        self.refresh_status_line();
        self.request_redraw();
    }
}
