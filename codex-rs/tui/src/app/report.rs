//! Local report generation stays independent of model turns and app-server connectivity.

use super::App;
use crate::app_event::AppEvent;
use crate::history_cell::AgentMarkdownCell;
use std::path::PathBuf;

impl App {
    pub(super) fn generate_report(&mut self) {
        self.chat_widget.add_info_message(
            "Generating telemetry dashboard…".to_string(),
            /*hint*/ None,
        );
        let home = self.config.codex_home.clone();
        let events = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = codex_report::generate_report(&home)
                .await
                .map_err(|error| error.to_string());
            events.send(AppEvent::ReportGenerated { result });
        });
    }

    pub(super) fn finish_report(&mut self, result: Result<PathBuf, String>) {
        match result {
            Ok(path) => match url::Url::from_file_path(&path) {
                Ok(link) => {
                    self.chat_widget.add_to_history(
                        AgentMarkdownCell::new_with_inline_visualizations(
                            format!("[Open telemetry dashboard]({link})"),
                            self.config.cwd.as_path(),
                            /*inline_visualization_context*/ None,
                        ),
                    );
                    self.chat_widget.add_info_message(
                        format!("Report saved to {}", path.display()),
                        /*hint*/ None,
                    );
                }
                Err(()) => self
                    .chat_widget
                    .add_error_message("Report path must be absolute.".to_string()),
            },
            Err(error) => self
                .chat_widget
                .add_error_message(format!("Could not generate telemetry dashboard: {error}")),
        }
    }
}
