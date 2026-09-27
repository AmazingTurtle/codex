//! Translation of transient runtime waits at the app-server boundary.

pub(crate) fn snapshot(
    thread: &codex_core::CodexThread,
) -> codex_app_server_protocol::ThreadWakeState {
    from_core(thread.wake_state())
}

pub(crate) fn from_core(
    state: codex_core::WakeState,
) -> codex_app_server_protocol::ThreadWakeState {
    codex_app_server_protocol::ThreadWakeState {
        pending_count: state.pending_count,
        waiting: state.waiting,
    }
}
