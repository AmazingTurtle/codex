use super::*;
use codex_features::DynamicWorkflowsMode;

impl App {
    pub(in crate::app) fn open_workflow_policy(&mut self) {
        let selected = self.config.dynamic_workflows_mode;
        let items = [
            (
                DynamicWorkflowsMode::Auto,
                "Auto",
                "Codex chooses when to use workflows (default)",
            ),
            (
                DynamicWorkflowsMode::Explicit,
                "Explicit",
                "Use workflows only when you invoke $dynamic-workflow",
            ),
            (
                DynamicWorkflowsMode::Off,
                "Off",
                "Disable dynamic workflows",
            ),
        ]
        .into_iter()
        .map(|(mode, name, description)| SelectionItem {
            name: name.into(),
            description: Some(description.into()),
            is_current: selected == mode,
            dismiss_on_select: true,
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::Workflow(WorkflowEvent::SetMode(mode)))
            })],
            ..Default::default()
        })
        .collect();
        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Dynamic workflows".into()),
            items,
            ..SelectionViewParams::picker()
        });
    }

    pub(in crate::app) async fn persist_workflow_policy(
        &mut self,
        app_server: &mut AppServerSession,
        mode: DynamicWorkflowsMode,
    ) {
        // Replace the feature value as one table so legacy `dynamic_workflows = true` works too.
        let value = serde_json::json!({ "enabled": true, "mode": mode });
        let edits = vec![crate::config_update::replace_config_value(
            "features.dynamic_workflows",
            value,
        )];
        match self
            .persist_model_defaults(app_server.request_handle(), edits, "dynamic workflow mode")
            .await
        {
            Ok(()) => {
                let cwd = self
                    .chat_widget
                    .config_ref()
                    .cwd
                    .to_string_lossy()
                    .into_owned();
                match crate::config_update::read_effective_config(app_server.request_handle(), cwd)
                    .await
                {
                    Ok(response) => {
                        let feature = response
                            .config
                            .additional
                            .get("features")
                            .and_then(|features| features.get("dynamic_workflows"));
                        let effective = if feature == Some(&serde_json::Value::Bool(false))
                            || feature
                                .and_then(|feature| feature.get("enabled"))
                                .and_then(serde_json::Value::as_bool)
                                == Some(false)
                        {
                            DynamicWorkflowsMode::Off
                        } else {
                            feature
                                .and_then(|feature| feature.get("mode"))
                                .cloned()
                                .and_then(|mode| serde_json::from_value(mode).ok())
                                .unwrap_or_default()
                        };
                        self.config.dynamic_workflows_mode = effective;
                        self.chat_widget.set_dynamic_workflow_mode(effective);
                    }
                    Err(error) => self.chat_widget.add_error_message(format!(
                        "Mode saved, but effective settings could not be read: {error}"
                    )),
                }
            }
            Err(error) => self
                .chat_widget
                .add_error_message(format!("Could not save dynamic workflow mode: {error}")),
        }
    }
}
