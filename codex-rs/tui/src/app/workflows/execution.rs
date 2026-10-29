//! Authoritative execution controls and local transition notices for one conversation.
use super::*;
use codex_protocol::protocol::ExecutionControlState;

impl App {
    pub(in crate::app) async fn request_execution_control(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        pause: bool,
    ) {
        let control = match transport::execution_control(&app_server.request_handle(), thread_id)
            .await
        {
            Ok(control) => control,
            Err(error) => {
                self.chat_widget
                    .add_error_message(format!("Could not read conversation execution: {error}"));
                return;
            }
        };
        self.apply_execution_snapshot(
            codex_app_server_protocol::ThreadExecutionControlChangedNotification {
                thread_id: thread_id.to_string(),
                execution_control: control.clone(),
            },
        );
        if pause && (control.state != ExecutionControlState::Running || !control.can_pause) {
            let message = match control.state {
                ExecutionControlState::Running => "Nothing to pause.",
                ExecutionControlState::Pausing => "Conversation is pausing.",
                ExecutionControlState::Paused => "Conversation is already paused.",
            };
            self.chat_widget
                .add_info_message(message.into(), /*hint*/ None);
            return;
        }
        if !pause && control.state != ExecutionControlState::Paused {
            self.chat_widget
                .add_info_message("Conversation isn’t paused.".into(), /*hint*/ None);
            return;
        }
        let Ok(root) = ThreadId::from_string(&control.root_thread_id) else {
            return;
        };
        if self.current_displayed_thread_id() != Some(root)
            || self.chat_widget.active_view_is(crate::workflows::CENTER_ID)
        {
            let name = app_server
                .thread_read(root, /*include_turns*/ false)
                .await
                .ok()
                .and_then(|thread| thread.name)
                .unwrap_or_else(|| root.to_string());
            self.show_execution_confirmation(root, name, pause);
        } else {
            self.set_confirmed_execution(app_server, root, pause).await;
        }
    }

    pub(in crate::app) fn show_execution_confirmation(
        &mut self,
        root: ThreadId,
        name: String,
        pause: bool,
    ) {
        let action = if pause { "Pause" } else { "Resume" };
        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some(format!("{action} entire conversation?")),
            subtitle: Some(format!(
                "{action} “{name}”, including its agents and workflows."
            )),
            items: vec![
                SelectionItem {
                    name: "Cancel".into(),
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: format!("{action} conversation"),
                    dismiss_on_select: true,
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::Workflow(WorkflowEvent::ConfirmExecution {
                            root,
                            pause,
                        }))
                    })],
                    ..Default::default()
                },
            ],
            ..SelectionViewParams::picker()
        });
    }

    pub(in crate::app) async fn set_confirmed_execution(
        &mut self,
        app_server: &AppServerSession,
        root: ThreadId,
        pause: bool,
    ) {
        if let Err(error) =
            transport::set_execution(&app_server.request_handle(), root, pause).await
        {
            self.chat_widget
                .add_error_message(format!("Could not change conversation execution: {error}"));
            return;
        }
        if let Ok(control) = transport::execution_control(&app_server.request_handle(), root).await
        {
            if pause && control.state == ExecutionControlState::Running {
                self.chat_widget
                    .add_info_message("Nothing to pause.".into(), /*hint*/ None);
            }
            self.apply_execution_snapshot(
                codex_app_server_protocol::ThreadExecutionControlChangedNotification {
                    thread_id: self
                        .current_displayed_thread_id()
                        .unwrap_or(root)
                        .to_string(),
                    execution_control: control,
                },
            );
        }
    }

    pub(in crate::app) fn apply_execution_control(
        &mut self,
        notification: codex_app_server_protocol::ThreadExecutionControlChangedNotification,
    ) {
        self.apply_execution_update(notification, ExecutionUpdate::Transition);
    }

    pub(in crate::app) fn apply_execution_snapshot(
        &mut self,
        notification: codex_app_server_protocol::ThreadExecutionControlChangedNotification,
    ) {
        self.apply_execution_update(notification, ExecutionUpdate::Snapshot);
    }

    fn apply_execution_update(
        &mut self,
        notification: codex_app_server_protocol::ThreadExecutionControlChangedNotification,
        source: ExecutionUpdate,
    ) {
        let current = self.current_displayed_thread_id().map(|id| id.to_string());
        let mut state = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root_matches = state.owner.map(|id| id.to_string()).as_deref()
            == Some(notification.execution_control.root_thread_id.as_str());
        if current.as_deref() != Some(notification.thread_id.as_str()) && !root_matches {
            return;
        }
        let execution = notification.execution_control.state;
        let notice = match (source, state.execution_notice_state, execution) {
            (ExecutionUpdate::Transition, Some(previous), ExecutionControlState::Paused)
                if previous != execution =>
            {
                Some("Conversation paused.")
            }
            (
                ExecutionUpdate::Transition,
                Some(ExecutionControlState::Paused | ExecutionControlState::Pausing),
                ExecutionControlState::Running,
            ) => Some("Conversation resumed."),
            _ => None,
        };
        if matches!(source, ExecutionUpdate::Transition) || state.execution_notice_state.is_none() {
            state.execution_notice_state = Some(execution);
        }
        state.execution = Some(execution);
        state.can_pause = notification.execution_control.can_pause;
        let can_pause = state.can_pause;
        drop(state);
        self.chat_widget
            .set_execution_control(Some(execution), can_pause);
        if let Some(message) = notice {
            self.chat_widget
                .add_info_message(message.into(), /*hint*/ None);
        }
    }
}

#[derive(Clone, Copy)]
enum ExecutionUpdate {
    Snapshot,
    Transition,
}
