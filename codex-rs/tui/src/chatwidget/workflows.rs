use super::*;

impl ChatWidget {
    pub(crate) fn handle_workflow_navigation(&mut self, key: KeyEvent) -> bool {
        self.bottom_pane.handle_workflow_navigation(key)
    }
    pub(crate) fn set_workflow_state(&mut self, state: crate::workflows::SharedWorkflows) {
        let tiers = self.saved_command_service_tiers();
        let commands = {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let candidates = if state.catalog_request_id.is_none() {
                crate::workflows::saved_commands::candidates(&state.saved)
            } else {
                state.command_catalog.clone()
            };
            let notices = state
                .saved
                .iter()
                .map(|row| {
                    crate::workflows::saved_commands::command_notice(row, &candidates, &tiers)
                })
                .collect::<Vec<_>>();
            for (row, notice) in state.saved.iter_mut().zip(notices) {
                row.command_notice = notice;
            }
            if state.catalog_overflow || state.catalog_loading {
                Vec::new()
            } else {
                crate::workflows::saved_commands::catalog(&candidates, &tiers)
            }
        };
        self.bottom_pane.set_saved_workflow_commands(commands);
        self.bottom_pane.workflow_strip.0 = state;
    }
    pub(super) fn handle_saved_workflow_command(
        &mut self,
        command: crate::workflows::saved_commands::SavedCommand,
        arguments: Option<String>,
    ) {
        if self
            .saved_workflow_command(&command.name)
            .is_none_or(|current| current.id != command.id)
        {
            self.add_error_message("Saved workflow commands changed or are still loading. Choose the current scoped entry in /workflows.".into());
            self.bottom_pane.record_pending_slash_command_history();
            return;
        }
        let arguments = if let Some(arguments) = arguments {
            if self.bottom_pane.composer_text().is_empty() {
                Some(arguments)
            } else {
                match self
                    .bottom_pane
                    .prepare_inline_args_submission(/*record_history*/ false)
                {
                    Some((arguments, _)) => Some(arguments),
                    None => return,
                }
            }
        } else {
            None
        };
        self.dispatch_saved_workflow(command, arguments.as_deref());
        self.bottom_pane.record_pending_slash_command_history();
    }
    pub(super) fn saved_workflow_command(
        &self,
        name: &str,
    ) -> Option<crate::workflows::saved_commands::SavedCommand> {
        let state = self
            .bottom_pane
            .workflow_strip
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.catalog_overflow || state.catalog_loading {
            return None;
        }
        let candidates = if state.catalog_request_id.is_none() {
            crate::workflows::saved_commands::candidates(&state.saved)
        } else {
            state.command_catalog.clone()
        };
        crate::workflows::saved_commands::catalog(&candidates, &self.saved_command_service_tiers())
            .into_iter()
            .find(|command| command.name == name)
    }
    fn saved_command_service_tiers(
        &self,
    ) -> Vec<crate::bottom_pane::slash_commands::ServiceTierCommand> {
        self.model_catalog
            .try_list_models()
            .unwrap_or_default()
            .into_iter()
            .flat_map(|model| model.service_tiers)
            .map(
                |tier| crate::bottom_pane::slash_commands::ServiceTierCommand {
                    id: tier.id,
                    name: tier.name.to_lowercase(),
                    description: tier.description,
                },
            )
            .collect()
    }
    pub(super) fn dispatch_saved_workflow(
        &mut self,
        command: crate::workflows::saved_commands::SavedCommand,
        arguments: Option<&str>,
    ) {
        let owner = self
            .bottom_pane
            .workflow_strip
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owner;
        let Some((owner, caller)) = owner.zip(self.thread_id) else {
            self.add_error_message("Start a conversation before running a saved workflow.".into());
            return;
        };
        self.app_event_tx.send(AppEvent::Workflow(
            crate::workflows::WorkflowEvent::StartSaved {
                owner,
                caller,
                id: command.id,
                input: crate::workflows::saved_commands::input(arguments),
                request_id: uuid::Uuid::new_v4(),
            },
        ));
    }
    pub(crate) fn set_dynamic_workflow_mode(&mut self, mode: codex_features::DynamicWorkflowsMode) {
        self.config.dynamic_workflows_mode = mode;
    }
}
