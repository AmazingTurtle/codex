//! Execution pauses coexist with modal waits without releasing each other's clocks.

use super::BottomPane;
use codex_protocol::protocol::ExecutionControlState;
use std::time::Instant;

impl BottomPane {
    pub(crate) fn set_execution_control(
        &mut self,
        state: Option<ExecutionControlState>,
        can_pause: bool,
    ) {
        self.execution_state = state;
        self.status_timer.set_execution_paused(
            matches!(
                state,
                Some(ExecutionControlState::Pausing | ExecutionControlState::Paused)
            ),
            Instant::now(),
        );
        self.composer.set_execution_commands(
            can_pause && state == Some(ExecutionControlState::Running),
            state == Some(ExecutionControlState::Paused),
        );
        if self.is_task_running
            && matches!(
                state,
                Some(ExecutionControlState::Pausing | ExecutionControlState::Paused)
            )
        {
            self.ensure_status_indicator();
        }
        self.sync_execution_status();
        self.request_redraw();
    }

    pub(super) fn sync_execution_status(&mut self) {
        if let Some(status) = self.status.as_mut() {
            status.set_execution_state(self.execution_state);
        }
    }
}
