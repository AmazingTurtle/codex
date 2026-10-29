//! Failed commands retain submitted text without changing the observed revision or retrying.

use super::*;

impl App {
    pub(super) fn recover_workflow_control(&mut self, intent: WorkflowUiIntent, error: String) {
        self.workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .error = Some(error.clone());
        let snapshot = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .action_snapshot
            .clone();
        let Some(mut snapshot) = snapshot else {
            self.workflow_control_error(error);
            return;
        };
        snapshot.scope = intent_scope(&intent).clone();
        let draft = match intent {
            WorkflowUiIntent::Control {
                operation: ControlOperation::Guide { message },
                ..
            } => Some((Action::Guide, message)),
            WorkflowUiIntent::Control {
                operation: ControlOperation::EditQueued { prompt },
                ..
            } => Some((Action::EditQueued, prompt)),
            WorkflowUiIntent::Control {
                operation: ControlOperation::Skip { replacement },
                ..
            } => Some((
                Action::Skip,
                serde_json::to_string_pretty(&replacement).unwrap_or_default(),
            )),
            WorkflowUiIntent::StageRevision { source, input, .. } => {
                snapshot.input = input;
                Some((Action::Revise, source))
            }
            _ => None,
        };
        if let Some((action, text)) = draft
            && let Some(view) = actions::form_with_error(
                *snapshot,
                action,
                text,
                error.clone(),
                self.workflow_intent_sender(),
                &self.keymap,
            )
        {
            self.chat_widget.show_bottom_pane_view(Box::new(view));
        } else {
            self.workflow_control_error(error);
        }
    }
}
