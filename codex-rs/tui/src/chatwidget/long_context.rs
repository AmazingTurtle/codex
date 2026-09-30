//! Long-context selection and effective model support.

use super::ChatWidget;
use crate::app_event::AppEvent;

pub(super) enum LongContextCommand {
    Toggle,
    On,
    Off,
}

impl ChatWidget {
    pub(crate) fn set_long_context(&mut self, long_context: bool) {
        self.config.long_context = long_context;
        self.refresh_model_dependent_surfaces();
    }

    pub(crate) fn model_supports_long_context(&self) -> bool {
        self.model_catalog
            .models
            .iter()
            .any(|model| model.model == self.current_model() && model.supports_long_context)
    }

    pub(super) fn long_context_active(&self) -> bool {
        self.config.long_context && self.model_supports_long_context()
    }

    pub(crate) fn can_toggle_long_context_from_keybinding(&self) -> bool {
        !self.active_side_conversation
            && !self.is_user_turn_pending_or_running()
            && self.bottom_pane.no_modal_or_popup_active()
    }

    pub(crate) fn toggle_long_context_from_ui(&mut self) {
        self.request_long_context_change(LongContextCommand::Toggle);
    }

    pub(super) fn request_long_context_change(&mut self, command: LongContextCommand) {
        if self.active_side_conversation || self.is_user_turn_pending_or_running() {
            self.add_error_message(
                "Long context can be changed between turns in the main conversation.".to_string(),
            );
            return;
        }
        let enabled = match command {
            LongContextCommand::Toggle => !self.config.long_context,
            LongContextCommand::On => true,
            LongContextCommand::Off => false,
        };
        self.app_event_tx.send(AppEvent::SetLongContext { enabled });
    }
}
