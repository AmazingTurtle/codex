//! Durable progress refreshes only the visible current-conversation worker accounting.
use super::*;
use crate::workflows::DetailTab;
use crate::workflows::OverviewTarget;
use crate::workflows::WorkflowState;
use codex_app_server_protocol::WorkflowProgressNotification;
use codex_app_server_protocol::WorkflowReadParams;
use codex_app_server_protocol::WorkflowReadResponse;
use codex_app_server_protocol::WorkflowRun;
use codex_app_server_protocol::WorkflowTranscriptTarget;

fn selected_target(state: &WorkflowState, caller: ThreadId) -> Option<OverviewTarget> {
    if state.center.saved
        || state.active_detail_tab() != DetailTab::Overview
        || (state.center.narrow && !state.center.details_focused)
        || state.displayed_thread != Some(caller)
        || !state.visible_run_indices().contains(&state.selected)
    {
        return None;
    }
    let row = state.rows.get(state.selected)?;
    let owner = state.owner?;
    if row.owner != Some(owner) || !matches!(row.status.as_str(), "Running" | "Pending") {
        return None;
    }
    let child = row.children.get(state.child?)?;
    if !matches!(
        &child.transcript_target,
        Some(WorkflowTranscriptTarget::Step { .. } | WorkflowTranscriptTarget::Attempt { .. })
    ) {
        return None;
    }
    Some(OverviewTarget {
        owner,
        caller,
        run_id: row.id.clone(),
        execution_id: row.execution_id.clone(),
        node_key: child.key.clone(),
        detail_revision: row.detail_revision,
        request_id: Uuid::new_v4(),
    })
}

impl App {
    pub(in crate::app) fn refresh_workflow_overview_after_progress(
        &mut self,
        app_server: &AppServerSession,
        update: &WorkflowProgressNotification,
    ) {
        if !self.chat_widget.active_view_is(crate::workflows::CENTER_ID) {
            return;
        }
        let Some(caller) = self.current_displayed_thread_id() else {
            return;
        };
        let mut state = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(target) = selected_target(&state, caller) else {
            return;
        };
        if target.owner.to_string() != update.thread_id
            || target.run_id != update.workflow.id
            || target.execution_id != update.workflow.execution_id
        {
            return;
        }
        state.overview_dirty = true;
        drop(state);
        self.refresh_visible_workflow_overview(app_server);
    }

    fn refresh_visible_workflow_overview(&mut self, app_server: &AppServerSession) {
        if !self.chat_widget.active_view_is(crate::workflows::CENTER_ID) {
            return;
        }
        let Some(caller) = self.current_displayed_thread_id() else {
            return;
        };
        let target = {
            let mut state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.overview_request.is_some() || !state.overview_dirty {
                return;
            }
            let Some(target) = selected_target(&state, caller) else {
                state.overview_dirty = false;
                return;
            };
            state.overview_dirty = false;
            state.overview_request = Some(target.request_id);
            target
        };
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = handle
                .request_typed::<WorkflowReadResponse>(ClientRequest::WorkflowRead {
                    request_id: codex_app_server_protocol::RequestId::String(
                        target.request_id.to_string(),
                    ),
                    params: WorkflowReadParams {
                        thread_id: target.caller.to_string(),
                        workflow_id: target.run_id.clone(),
                    },
                })
                .await
                .map(|response| Box::new(response.workflow))
                .map_err(|error| error.to_string());
            tx.send(AppEvent::Workflow(WorkflowEvent::OverviewLoaded {
                target,
                result,
            }));
        });
    }

    pub(super) fn finish_workflow_overview(
        &mut self,
        app_server: &AppServerSession,
        target: OverviewTarget,
        result: Result<Box<WorkflowRun>, String>,
    ) {
        let visible = self.chat_widget.active_view_is(crate::workflows::CENTER_ID);
        let caller = self.current_displayed_thread_id();
        {
            let mut state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.overview_request != Some(target.request_id) {
                return;
            }
            state.overview_request = None;
            let selected = caller
                .filter(|_| visible)
                .and_then(|caller| selected_target(&state, caller));
            let current = selected.is_some_and(|current| {
                current.owner == target.owner
                    && current.caller == target.caller
                    && current.run_id == target.run_id
                    && current.execution_id == target.execution_id
                    && current.node_key == target.node_key
                    && current.detail_revision == target.detail_revision
            });
            if current {
                match result {
                    Ok(run)
                        if run.id == target.run_id
                            && run.execution_id == target.execution_id
                            && run.owner_thread_id == target.owner.to_string() =>
                    {
                        let incoming = crate::workflows::project(*run);
                        let selected = state.selected;
                        let child = state.child;
                        if let Some(existing) = state
                            .rows
                            .get_mut(selected)
                            .and_then(|row| child.and_then(|index| row.children.get_mut(index)))
                            && let Some(fresh) = incoming.children.into_iter().find(|child| {
                                child.key == target.node_key
                                    && child.action_target == existing.action_target
                            })
                            && let (Some(overview), Some(incoming)) =
                                (existing.overview.as_mut(), fresh.overview)
                        {
                            overview.metrics = incoming.metrics;
                            overview.refresh_error = None;
                            existing.updated_at = fresh.updated_at;
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        let selected = state.selected;
                        let child = state.child;
                        if let Some(overview) = state
                            .rows
                            .get_mut(selected)
                            .and_then(|row| child.and_then(|index| row.children.get_mut(index)))
                            .and_then(|child| child.overview.as_mut())
                        {
                            overview.refresh_error =
                                Some(format!("Could not refresh accounting: {error}"));
                        }
                    }
                }
            }
        }
        self.refresh_visible_workflow_overview(app_server);
    }
}
