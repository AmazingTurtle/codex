//! Workflow mutation guards, retained saved input, and opening ordinary agent chats.
use super::*;

impl WorkflowCenter {
    pub(super) fn open_actions(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.center.saved || !state.visible_run_indices().contains(&state.selected) {
            return;
        }
        let Some(row) = state.rows.get(state.selected) else {
            return;
        };
        let Some((owner, caller)) = row.owner.or(state.owner).zip(state.displayed_thread) else {
            return;
        };
        if Some(owner) != state.owner {
            state.error =
                Some("Open the owning conversation before controlling this workflow.".into());
            state.center.details_focused = state.center.narrow;
            return;
        }
        let target = if let Some(index) = state.child {
            let Some(target) = row
                .children
                .get(index)
                .and_then(|child| child.action_target.clone())
            else {
                state.error=Some("This row has no authoritative control target. Refresh the workflow before choosing an action.".into());
                state.center.details_focused = state.center.narrow;
                return;
            };
            target
        } else {
            super::super::actions::ActionTarget::Run
        };
        self.send(WorkflowEvent::ActionsOpen {
            owner,
            caller,
            run_id: row.id.clone(),
            execution_id: row.execution_id.clone(),
            target,
        });
    }

    pub(super) fn accept_saved(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.operation_pending {
            return;
        }
        let saved = if self.input.is_some() {
            state
                .saved
                .iter()
                .find(|saved| Some(&saved.id) == self.input_saved_id.as_ref())
        } else {
            state
                .visible_saved_indices()
                .contains(&state.selected_saved_index())
                .then(|| &state.saved[state.selected_saved_index()])
        };
        let Some(saved) = saved else {
            if self.input.is_some() {
                self.input_error =
                    Some("This saved workflow was removed. Return and choose another.".into());
            }
            return;
        };
        if saved.read_only {
            let reason = saved.read_only_reason.clone().unwrap_or_else(|| "Legacy saved workflow is read-only. Inspect its source before creating a canonical workflow.".into());
            if self.input.is_some() {
                self.input_error = Some(reason);
            } else {
                state.error = Some(reason);
                state.center.details_focused = state.center.narrow;
            }
            return;
        }
        if let Some(input) = &self.input {
            let value = if input.text().trim().is_empty() {
                serde_json::json!({})
            } else {
                match serde_json::from_str(input.text()) {
                    Ok(value) => value,
                    Err(error) => {
                        self.input_error = Some(format!("Enter JSON input: {error}"));
                        return;
                    }
                }
            };
            let request_id = uuid::Uuid::new_v4();
            let Some((owner, caller)) = state.owner.zip(state.displayed_thread) else {
                self.input_error =
                    Some("Start a conversation before running a saved workflow.".into());
                return;
            };
            self.send(WorkflowEvent::StartSaved {
                owner,
                caller,
                id: saved.id.clone(),
                input: value,
                request_id,
            });
            state.operation_pending = true;
            state.operation_id = Some(request_id);
        } else {
            self.input = Some(TextArea::new());
            self.input_saved_id = Some(saved.id.clone());
            state.center.saved_id = self.input_saved_id.clone();
        }
    }

    pub(super) fn open_chat(&mut self) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.center.saved || !state.visible_run_indices().contains(&state.selected) {
            return;
        }
        if state.child.is_none()
            && state
                .rows
                .get(state.selected)
                .is_none_or(|row| row.owner == state.displayed_thread)
        {
            self.complete = state.rows.get(state.selected).is_some();
            return;
        }
        if let Some(thread_id) = state.selected_thread() {
            self.send(if state.child.is_some() || Some(thread_id) == state.owner {
                WorkflowEvent::SelectAgent(thread_id)
            } else {
                WorkflowEvent::OpenOwner(thread_id)
            });
            self.complete = true;
        }
    }

    pub(super) fn selected_action(&self, action: WorkflowAction) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.center.saved || !state.visible_run_indices().contains(&state.selected) {
            return;
        }
        let Some(row) = state.rows.get(state.selected) else {
            return;
        };
        if row.read_only {
            state.error = Some(row.read_only_reason.clone().unwrap_or_else(|| "Legacy workflow is read-only. Inspect its source or transcript to create a new workflow.".into()));
            state.center.details_focused = state.center.narrow;
            return;
        }
        if matches!(action, WorkflowAction::Stop) && row.status == "Completed" {
            state.error = Some("This workflow has completed. No work remains to stop.".into());
            state.center.details_focused = state.center.narrow;
            return;
        }
        if let Some(owner) = row.owner.or(state.owner) {
            self.send(WorkflowEvent::Action {
                owner,
                id: row.id.clone(),
                action,
            });
        }
    }
}
