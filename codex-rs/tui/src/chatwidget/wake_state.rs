//! Wake activity is independent of the composer's running/idle input state.

use super::*;

impl ChatWidget {
    pub(super) fn set_wake_state(
        &mut self,
        state: Option<codex_app_server_protocol::ThreadWakeState>,
    ) {
        if self.status_state.wake_state == state {
            return;
        }
        self.status_state.wake_state = state;
        self.bottom_pane.set_wake_state(state.unwrap_or_default());
        let current = self.status_state.current_status.clone();
        self.set_status(
            current.header,
            current.details,
            StatusDetailsCapitalization::Preserve,
            current.details_max_lines,
        );
        self.refresh_status_surfaces();
        self.request_redraw();
    }
}
