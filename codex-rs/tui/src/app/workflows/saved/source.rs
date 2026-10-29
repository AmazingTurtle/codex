//! Source is read on request only; list/discovery never injects source into command metadata.
use super::*;

const MAX_SAVED_SOURCE_BYTES: usize = 24 * 1024;

impl App {
    pub(in crate::app) fn read_saved_workflow_source(
        &mut self,
        app_server: &AppServerSession,
        id: String,
        refresh: bool,
    ) {
        let caller = self.current_displayed_thread_id();
        let request_id = Uuid::new_v4();
        let owner = {
            let mut state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(owner) = caller.or(state.owner) else {
                return;
            };
            if state.saved_sources.len() >= 8
                && !state.saved_sources.contains_key(&id)
                && let Some(old) = state.saved_sources.keys().next().cloned()
            {
                state.saved_sources.remove(&old);
            }
            let source = state.saved_sources.entry(id.clone()).or_default();
            if source.loading || (!refresh && source.source.is_some()) {
                return;
            }
            source.loading = true;
            source.error = None;
            source.request_id = Some(request_id);
            owner
        };
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = async {
                let response: codex_app_server_protocol::WorkflowSavedReadResponse = handle
                    .request_typed(ClientRequest::WorkflowSavedRead {
                        request_id: codex_app_server_protocol::RequestId::String(format!(
                            "workflow-source-{request_id}"
                        )),
                        params: codex_app_server_protocol::WorkflowSavedReadParams {
                            thread_id: owner.to_string(),
                            saved_workflow_id: id.clone(),
                        },
                    })
                    .await
                    .map_err(|error| error.to_string())?;
                let source = response.saved_workflow.source;
                if source.len() > MAX_SAVED_SOURCE_BYTES {
                    return Err("Saved workflow source exceeds the 24KiB preview limit.".into());
                }
                Ok(source)
            }
            .await;
            tx.send(AppEvent::Workflow(WorkflowEvent::SavedSourceLoaded {
                id,
                request_id,
                result,
            }));
        });
    }

    pub(in crate::app) fn finish_saved_workflow_source(
        &mut self,
        id: String,
        request_id: Uuid,
        result: Result<String, String>,
    ) {
        let mut state = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(source) = state.saved_sources.get_mut(&id) else {
            return;
        };
        if source.request_id != Some(request_id) {
            return;
        }
        source.loading = false;
        match result {
            Ok(text) => {
                source.source = Some(text);
                source.error = None;
            }
            Err(error) => source.error = Some(format!("Could not read saved source: {error}")),
        }
    }
}
