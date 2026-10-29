//! Commands have a separate bounded metadata catalog, independent of Saved view pagination.
use super::*;
use crate::workflows::saved_commands::SavedCatalog;
use crate::workflows::saved_commands::SavedCommandCandidate;

impl App {
    pub(in crate::app) fn refresh_saved_workflow_catalog(&mut self, app_server: &AppServerSession) {
        let Some(owner) = self.workflow_attachment_thread_id() else {
            return;
        };
        let request_id = Uuid::new_v4();
        let cancellation = tokio_util::sync::CancellationToken::new();
        {
            let mut state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(previous) = state.catalog_cancel.replace(cancellation.clone()) {
                previous.cancel();
            }
            state.catalog_request_id = Some(request_id);
            state.catalog_loading = true;
        }
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = tokio::select! {
                result = read_catalog(&handle, owner) => result,
                _ = cancellation.cancelled() => return,
            };
            tx.send(AppEvent::Workflow(WorkflowEvent::SavedCatalogLoaded {
                owner,
                request_id,
                result,
            }));
        });
    }

    pub(in crate::app) fn finish_saved_workflow_catalog(
        &mut self,
        owner: ThreadId,
        request_id: Uuid,
        result: Result<SavedCatalog, String>,
    ) {
        let mut state = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.attached_thread != Some(owner) || state.catalog_request_id != Some(request_id) {
            return;
        }
        state.catalog_loading = false;
        match result {
            Ok(catalog) => {
                state.command_catalog = catalog.candidates;
                state.catalog_overflow = catalog.overflow;
            }
            Err(error) => {
                state.command_catalog.clear();
                state.error = Some(format!("Could not discover saved commands: {error}"));
            }
        }
        drop(state);
        self.chat_widget.set_workflow_state(self.workflows.clone());
    }
}

async fn read_catalog(
    handle: &codex_app_server_client::AppServerRequestHandle,
    owner: ThreadId,
) -> Result<SavedCatalog, String> {
    let mut catalog = SavedCatalog {
        candidates: Vec::new(),
        overflow: false,
    };
    let mut cursor = None;
    let mut seen_cursors = std::collections::HashSet::new();
    let mut entries = 0;
    let mut bytes = 0;
    loop {
        let page: codex_app_server_protocol::WorkflowSavedListResponse = handle
            .request_typed(ClientRequest::WorkflowSavedList {
                request_id: codex_app_server_protocol::RequestId::String(format!(
                    "workflow-catalog-{}",
                    Uuid::new_v4()
                )),
                params: codex_app_server_protocol::WorkflowSavedListParams {
                    thread_id: owner.to_string(),
                    cursor,
                    limit: Some(50),
                },
            })
            .await
            .map_err(|error| error.to_string())?;
        for saved in page.data {
            entries += 1;
            if entries > 2048 {
                catalog.overflow = true;
                return Ok(catalog);
            }
            if saved.read_only {
                continue;
            }
            let Some(metadata) = saved.metadata else {
                continue;
            };
            let candidate = SavedCommandCandidate {
                id: saved.id,
                name: metadata.name,
                description: metadata.description,
                scope: Some(saved.scope),
            };
            bytes += candidate.id.len()
                + candidate.name.len()
                + candidate.description.len()
                + std::mem::size_of::<SavedCommandCandidate>();
            if bytes > 4 * 1024 * 1024 {
                catalog.overflow = true;
                return Ok(catalog);
            }
            catalog.candidates.push(candidate);
        }
        let Some(next) = page.next_cursor else {
            return Ok(catalog);
        };
        if entries >= 2048 {
            catalog.overflow = true;
            return Ok(catalog);
        }
        if !seen_cursors.insert(next.clone()) {
            return Err("saved workflow catalog repeated a page cursor".into());
        }
        cursor = Some(next);
    }
}
