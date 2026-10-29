//! Full lifecycle facts and compact progress keep their existing owning-conversation boundary.
use super::*;

impl App {
    pub(in crate::app) fn apply_workflow_update(
        &mut self,
        notification: codex_app_server_protocol::WorkflowUpdatedNotification,
    ) {
        let mut state = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.owner.map(|owner| owner.to_string()).as_deref()
            != Some(notification.thread_id.as_str())
        {
            return;
        }
        state.apply_update(crate::workflows::project(notification.workflow));
    }

    pub(in crate::app) fn apply_workflow_progress(
        &mut self,
        notification: codex_app_server_protocol::WorkflowProgressNotification,
    ) {
        let mut state = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.owner.map(|owner| owner.to_string()).as_deref()
            == Some(notification.thread_id.as_str())
        {
            state.apply_progress(notification.workflow);
        }
    }
}
