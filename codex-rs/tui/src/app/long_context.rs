//! Save the default and apply long context to the active thread between turns.

use super::App;
use crate::app_server_session::AppServerSession;
use codex_app_server_protocol::ThreadSettingsUpdateParams;

impl App {
    pub(super) async fn update_long_context(
        &mut self,
        app_server: &mut AppServerSession,
        enabled: bool,
    ) {
        let edits = vec![crate::config_update::replace_config_value(
            "long_context",
            serde_json::json!(enabled),
        )];
        if let Err(error) = self
            .persist_model_defaults(app_server.request_handle(), edits, "long context")
            .await
        {
            self.chat_widget
                .add_error_message(format!("Failed to save long context: {error}"));
            return;
        }
        self.config.long_context = enabled;
        if let Some(thread_id) = self.active_thread_id {
            let params = ThreadSettingsUpdateParams {
                thread_id: thread_id.to_string(),
                long_context: Some(enabled),
                ..Default::default()
            };
            if !self.send_thread_settings_update(app_server, params).await {
                return;
            }
        }
        self.chat_widget.set_long_context(enabled);
        let state = if enabled { "on" } else { "off" };
        let message = if enabled && !self.chat_widget.model_supports_long_context() {
            "Long context saved as on; unavailable for the current model.".to_string()
        } else {
            format!("Long context {state}")
        };
        self.chat_widget.add_info_message(message, /*hint*/ None);
    }
}
