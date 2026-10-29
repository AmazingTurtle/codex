//! Workflow focus only intercepts navigation from an otherwise idle, empty editor.
use super::*;

impl BottomPane {
    pub(crate) fn handle_workflow_navigation(&mut self, key: KeyEvent) -> bool {
        if key.kind != KeyEventKind::Press
            || !key.modifiers.is_empty()
            || !self.no_modal_or_popup_active()
            || !self.composer.workflow_navigation_available()
        {
            return false;
        }
        let mut state = self
            .workflow_strip
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let indices = state.current_indices();
        if indices.is_empty() && state.inspected_agent.is_none() {
            return false;
        }
        if key.code != KeyCode::Down {
            return false;
        }
        state.strip_focused = false;
        state.center.details_focused = false;
        self.app_event_tx
            .send(AppEvent::Workflow(crate::workflows::WorkflowEvent::Open));
        drop(state);
        self.request_redraw();
        true
    }
}

#[cfg(test)]
#[path = "workflow_strip_tests.rs"]
mod tests;
