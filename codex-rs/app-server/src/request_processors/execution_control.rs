//! Public inference controls share core's conversation-tree gate with the TUI.

use super::*;
use codex_app_server_protocol::ThreadExecutionControl;
use codex_app_server_protocol::ThreadExecutionControlReadResponse;
use codex_app_server_protocol::ThreadPauseResponse;
use codex_app_server_protocol::ThreadUnpauseResponse;

pub(crate) struct ExecutionControlRequestProcessor {
    thread_manager: Arc<ThreadManager>,
}

impl ExecutionControlRequestProcessor {
    pub(crate) fn new(thread_manager: Arc<ThreadManager>) -> Self {
        Self { thread_manager }
    }

    pub(crate) async fn pause(
        &self,
        thread_id: &str,
    ) -> Result<ThreadPauseResponse, JSONRPCErrorError> {
        let thread = self.thread(thread_id).await?;
        let state = thread
            .pause_execution()
            .await
            .map_err(|err| internal_error(format!("failed to pause conversation: {err}")))?;
        Ok(ThreadPauseResponse {
            execution_control: ThreadExecutionControl {
                root_thread_id: state.root_thread_id.to_string(),
                state: state.state,
                can_pause: thread.can_pause_execution().await.map_err(|err| {
                    internal_error(format!("failed to read pause availability: {err}"))
                })?,
            },
        })
    }

    pub(crate) async fn unpause(
        &self,
        thread_id: &str,
    ) -> Result<ThreadUnpauseResponse, JSONRPCErrorError> {
        let thread = self.thread(thread_id).await?;
        let state = thread
            .unpause_execution()
            .await
            .map_err(|err| internal_error(format!("failed to unpause conversation: {err}")))?;
        Ok(ThreadUnpauseResponse {
            execution_control: ThreadExecutionControl {
                root_thread_id: state.root_thread_id.to_string(),
                state: state.state,
                can_pause: thread.can_pause_execution().await.map_err(|err| {
                    internal_error(format!("failed to read pause availability: {err}"))
                })?,
            },
        })
    }

    pub(crate) async fn read(
        &self,
        thread_id: &str,
    ) -> Result<ThreadExecutionControlReadResponse, JSONRPCErrorError> {
        let thread = self.thread(thread_id).await?;
        let state = thread
            .execution_state()
            .await
            .map_err(|err| internal_error(format!("failed to read execution control: {err}")))?;
        Ok(ThreadExecutionControlReadResponse {
            execution_control: ThreadExecutionControl {
                root_thread_id: state.root_thread_id.to_string(),
                state: state.state,
                can_pause: thread.can_pause_execution().await.map_err(|err| {
                    internal_error(format!("failed to read pause availability: {err}"))
                })?,
            },
        })
    }

    async fn thread(&self, thread_id: &str) -> Result<Arc<CodexThread>, JSONRPCErrorError> {
        let thread_id = ThreadId::from_string(thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        self.thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|_| invalid_request(format!("thread not found: {thread_id}")))
    }
}

/// Refresh on native activity and queued-wake changes. The
/// listener owns the last snapshot so repeated lifecycle signals are silent.
pub(super) async fn notify(
    thread_id: ThreadId,
    thread: &CodexThread,
    outgoing: &Arc<OutgoingMessageSender>,
    threads: &ThreadStateManager,
    previous: &mut Option<ThreadExecutionControl>,
) {
    let snapshot = async {
        let state = thread.execution_state().await?;
        Ok::<_, codex_protocol::error::CodexErr>(ThreadExecutionControl {
            root_thread_id: state.root_thread_id.to_string(),
            state: state.state,
            can_pause: thread.can_pause_execution().await?,
        })
    }
    .await;
    let Ok(snapshot) = snapshot else {
        return;
    };
    if previous.as_ref() == Some(&snapshot) {
        return;
    }
    *previous = Some(snapshot.clone());
    let subscribers = threads.subscribed_connection_ids(thread_id).await;
    let sender =
        ThreadScopedOutgoingMessageSender::new(Arc::clone(outgoing), subscribers, thread_id);
    sender
        .send_server_notification(ServerNotification::ThreadExecutionControlChanged(
            codex_app_server_protocol::ThreadExecutionControlChangedNotification {
                thread_id: thread_id.to_string(),
                execution_control: snapshot,
            },
        ))
        .await;
}
