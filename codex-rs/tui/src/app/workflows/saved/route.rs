//! Saved launch retains the displayed caller's authority and the owning root's UI state.
use super::*;

impl App {
    pub(in crate::app) fn start_saved_workflow(
        &mut self,
        app_server: &AppServerSession,
        owner: ThreadId,
        caller: ThreadId,
        id: String,
        input: serde_json::Value,
        request_id: Uuid,
    ) {
        {
            let mut state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.owner != Some(owner) {
                drop(state);
                self.chat_widget.add_error_message("Saved workflow request belongs to another conversation. Return to the originating conversation to run it.".into());
                return;
            }
            if let Some(saved) = state
                .saved
                .iter()
                .find(|saved| saved.id == id && saved.read_only)
            {
                let reason = saved
                    .read_only_reason
                    .clone()
                    .unwrap_or_else(|| "This saved workflow is read-only.".into());
                if state.operation_id == Some(request_id) {
                    state.operation_pending = false;
                    state.operation_result = Some(Err(reason.clone()));
                    state.error = Some(reason.clone());
                }
                drop(state);
                self.chat_widget.add_error_message(reason);
                return;
            }
            if state.operation_pending && state.operation_id != Some(request_id) {
                drop(state);
                self.chat_widget.add_error_message(
                    "A saved workflow request is already being accepted.".into(),
                );
                return;
            }
            state.operation_pending = true;
            state.operation_id = Some(request_id);
            state.operation_result = None;
            state.error = None;
        }
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = transport::start_saved(&handle, caller, id, input).await;
            tx.send(AppEvent::Workflow(WorkflowEvent::OperationFinished {
                owner,
                request_id,
                result,
            }));
        });
    }
}
