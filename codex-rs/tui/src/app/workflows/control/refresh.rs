//! Lifecycle updates refresh a visible Actions picker; drafts and historical documents stay put.

use super::*;

impl App {
    pub(in crate::app) fn refresh_workflow_actions_after_update(
        &mut self,
        app_server: &AppServerSession,
        update: &codex_app_server_protocol::WorkflowUpdatedNotification,
    ) {
        if !self.chat_widget.active_view_is(actions::ACTIONS_VIEW_ID) {
            return;
        }
        {
            let mut state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(snapshot) = state.action_snapshot.as_ref() else {
                return;
            };
            if snapshot.scope.workflow_id != update.workflow.id
                || snapshot.scope.owner.to_string() != update.thread_id
            {
                return;
            }
            state.action_refresh_pending = true;
        }
        self.refresh_workflow_action_context(app_server);
    }

    pub(super) fn refresh_workflow_action_context(&mut self, app_server: &AppServerSession) {
        if !self.chat_widget.active_view_is(actions::ACTIONS_VIEW_ID) {
            return;
        }
        let snapshot = {
            let state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.action_pending || !state.action_refresh_pending {
                return;
            }
            let Some(snapshot) = state.action_snapshot.clone() else {
                return;
            };
            snapshot
        };
        if !self.workflow_action_current(&snapshot.scope, ScopeCheck::Attachment) {
            return;
        }
        {
            let mut state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.action_pending = true;
            state.action_refresh_pending = false;
        }
        let scope = snapshot.scope.clone();
        let mut loading = *snapshot;
        for available in &mut loading.availability {
            available.unavailable_reason = Some("Refreshing workflow controls".into());
        }
        self.chat_widget.replace_selection_view_if_present(
            actions::ACTIONS_VIEW_ID,
            actions::menu(loading, self.workflow_intent_sender()),
        );
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = requests::actions(&handle, scope.clone()).await;
            tx.send(AppEvent::Workflow(WorkflowEvent::ControlResult {
                scope,
                outcome: Box::new(WorkflowUiOutcome::Actions(result)),
            }));
        });
    }
}
