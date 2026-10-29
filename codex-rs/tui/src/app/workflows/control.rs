//! Dashboard commands keep one captured caller and attachment through forms and background RPCs.

use super::*;
use crate::workflows::actions::*;
use crate::workflows::actions::{self};

mod recovery;
mod refresh;
mod requests;

enum ScopeCheck {
    Attachment,
    Execution,
}

fn intent_scope(intent: &WorkflowUiIntent) -> &ActionScope {
    match intent {
        WorkflowUiIntent::OpenForm { snapshot, .. } => &snapshot.scope,
        WorkflowUiIntent::Control { scope, .. }
        | WorkflowUiIntent::RevisionArguments { scope, .. }
        | WorkflowUiIntent::StageRevision { scope, .. }
        | WorkflowUiIntent::ApplyRevision { scope, .. }
        | WorkflowUiIntent::Resume { scope, .. }
        | WorkflowUiIntent::ShowResult { scope }
        | WorkflowUiIntent::RevisionHistory { scope, .. }
        | WorkflowUiIntent::InspectRevision { scope, .. }
        | WorkflowUiIntent::ViewRevisionSource { scope, .. }
        | WorkflowUiIntent::SetFooterVisibility { scope, .. }
        | WorkflowUiIntent::ViewExecution { scope, .. } => scope,
    }
}

impl App {
    fn workflow_intent_sender(&self) -> IntentSender {
        let tx = self.app_event_tx.clone();
        Arc::new(move |intent| {
            tx.send(AppEvent::Workflow(WorkflowEvent::ControlIntent(Box::new(
                intent,
            ))))
        })
    }

    fn workflow_action_current(&self, scope: &ActionScope, check: ScopeCheck) -> bool {
        let state = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.current_displayed_thread_id() == Some(scope.caller)
            && state.displayed_thread == Some(scope.caller)
            && state.attached_thread.or(state.owner) == Some(scope.owner)
            && state.action_context_id == Some(scope.context_id)
            && match check {
                ScopeCheck::Attachment => true,
                ScopeCheck::Execution => state.rows.iter().any(|row| {
                    row.id == scope.workflow_id
                        && row.owner.unwrap_or(scope.owner) == scope.owner
                        && row.execution_id == scope.execution_id
                }),
            }
    }

    pub(super) fn open_workflow_actions(
        &mut self,
        app_server: &AppServerSession,
        owner: ThreadId,
        caller: ThreadId,
        run_id: String,
        execution_id: String,
        target: ActionTarget,
    ) {
        let scope = ActionScope {
            context_id: Uuid::new_v4(),
            owner,
            caller,
            workflow_id: run_id,
            execution_id,
            expected_control_revision: 0,
            target,
        };
        {
            let mut state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.attached_thread.or(state.owner) != Some(owner)
                || self.current_displayed_thread_id() != Some(caller)
            {
                drop(state);
                self.chat_widget
                    .add_error_message("The conversation changed. Reopen workflow Actions.".into());
                return;
            }
            state.action_context_id = Some(scope.context_id);
            state.action_snapshot = None;
            state.action_pending = true;
            state.action_refresh_pending = false;
            state.error = None;
        }
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = requests::actions(&handle, scope.clone()).await;
            tx.send(AppEvent::Workflow(WorkflowEvent::ControlResult {
                scope,
                outcome: Box::new(WorkflowUiOutcome::Actions(result)),
            }));
        });
    }

    pub(super) fn workflow_control_intent(
        &mut self,
        app_server: &AppServerSession,
        intent: WorkflowUiIntent,
    ) {
        let scope = intent_scope(&intent).clone();
        let check = match &intent {
            WorkflowUiIntent::RevisionHistory { .. }
            | WorkflowUiIntent::InspectRevision { .. }
            | WorkflowUiIntent::ViewRevisionSource { .. }
            | WorkflowUiIntent::ViewExecution { .. }
            | WorkflowUiIntent::SetFooterVisibility { .. }
            | WorkflowUiIntent::ShowResult { .. } => ScopeCheck::Attachment,
            _ => ScopeCheck::Execution,
        };
        if !self.workflow_action_current(&scope, check) {
            self.chat_widget.add_error_message(
                "The selected workflow or conversation changed. Reopen Actions before submitting."
                    .into(),
            );
            return;
        }
        let send = self.workflow_intent_sender();
        match intent {
            WorkflowUiIntent::SetFooterVisibility { scope, visible } => {
                let mut state = self
                    .workflows
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if visible {
                    state.hidden_from_strip.remove(&scope.workflow_id);
                } else {
                    state.hidden_from_strip.insert(scope.workflow_id);
                }
                if let Some(snapshot) = state.action_snapshot.as_mut() {
                    snapshot.footer_hidden = !visible;
                }
            }
            WorkflowUiIntent::OpenForm { snapshot, action } => {
                let pending = self
                    .workflows
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .action_pending;
                if pending {
                    self.workflow_control_error("Workflow controls are refreshing. Reopen Actions when the request finishes.".into());
                    return;
                }
                if let Some(view) = actions::form(*snapshot, action, send, &self.keymap) {
                    self.chat_widget.show_bottom_pane_view(Box::new(view));
                }
            }
            WorkflowUiIntent::RevisionArguments {
                scope,
                source,
                initial_input,
            } => {
                self.chat_widget
                    .show_bottom_pane_view(Box::new(actions::revision_arguments(
                        scope,
                        source,
                        initial_input,
                        send,
                        &self.keymap,
                    )));
            }
            WorkflowUiIntent::InspectRevision { scope, revision } => {
                self.show_workflow_revision(scope, *revision)
            }
            WorkflowUiIntent::ViewRevisionSource { revision, .. } => self
                .chat_widget
                .show_bottom_pane_view(Box::new(InspectionView::revision(*revision))),
            intent => {
                {
                    let mut state = self
                        .workflows
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.action_pending {
                        drop(state);
                        self.workflow_control_error("A workflow action is still pending. Inspect its receipt before submitting another.".into());
                        return;
                    }
                    state.action_pending = true;
                }
                let handle = app_server.request_handle();
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let outcome = match &intent {
                        WorkflowUiIntent::RevisionHistory { cursor, .. } => {
                            WorkflowUiOutcome::Revisions(
                                requests::revisions(&handle, &scope, cursor.clone()).await,
                            )
                        }
                        WorkflowUiIntent::ViewExecution {
                            revision_id,
                            execution_id,
                            ..
                        } => WorkflowUiOutcome::Execution(
                            requests::execution(&handle, &scope, revision_id, execution_id).await,
                        ),
                        _ => {
                            let result = requests::mutate(&handle, &intent).await;
                            WorkflowUiOutcome::Mutation {
                                intent: Box::new(intent),
                                result,
                            }
                        }
                    };
                    tx.send(AppEvent::Workflow(WorkflowEvent::ControlResult {
                        scope,
                        outcome: Box::new(outcome),
                    }));
                });
            }
        }
    }

    pub(super) fn finish_workflow_control(
        &mut self,
        app_server: &AppServerSession,
        scope: ActionScope,
        outcome: WorkflowUiOutcome,
    ) {
        if !self.workflow_action_current(&scope, ScopeCheck::Attachment) {
            return;
        }
        let refreshing = matches!(&outcome, WorkflowUiOutcome::Actions(_))
            && self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .action_snapshot
                .is_some();
        self.workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .action_pending = false;
        if refreshing && !self.chat_widget.active_view_is(actions::ACTIONS_VIEW_ID) {
            return;
        }
        match outcome {
            WorkflowUiOutcome::Actions(Ok(mut snapshot)) => {
                {
                    let mut state = self
                        .workflows
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    snapshot.footer_hidden = state
                        .hidden_from_strip
                        .contains(&snapshot.scope.workflow_id);
                    state.action_snapshot = Some(Box::new(snapshot.clone()));
                }
                let params = actions::menu(snapshot, self.workflow_intent_sender());
                if refreshing {
                    self.chat_widget
                        .replace_selection_view_if_present(actions::ACTIONS_VIEW_ID, params);
                } else {
                    self.chat_widget.show_selection_view(params);
                }
            }
            WorkflowUiOutcome::Actions(Err(error))
            | WorkflowUiOutcome::Revisions(Err(error))
            | WorkflowUiOutcome::Execution(Err(error)) => self.workflow_control_error(error),
            WorkflowUiOutcome::Revisions(Ok(page)) => {
                self.chat_widget.show_selection_view(actions::history(
                    scope,
                    page.data,
                    page.next_cursor,
                    self.workflow_intent_sender(),
                ))
            }
            WorkflowUiOutcome::Execution(Ok(execution)) => {
                self.chat_widget
                    .show_bottom_pane_view(Box::new(InspectionView::execution(
                        scope,
                        execution,
                        self.workflows.clone(),
                        self.app_event_tx.clone(),
                    )))
            }
            WorkflowUiOutcome::Mutation { intent, result } => {
                match result {
                    Ok(receipt) => {
                        {
                            let mut state = self
                                .workflows
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            state.error = None;
                            if let (Some(snapshot), Some(ack)) =
                                (state.action_snapshot.as_mut(), receipt.state)
                            {
                                snapshot.scope.expected_control_revision = ack.control_revision;
                                snapshot.staged_revision_id = ack.pending_revision_id;
                                snapshot.active_revision_id = Some(ack.active_revision_id);
                                if let Some(revision) = receipt.revision.as_ref()
                                    && revision.status == "Draft"
                                {
                                    snapshot.staged_revision_id = Some(revision.id.clone());
                                }
                            }
                        }
                        if let Some(revision) = receipt.revision {
                            let scope = self
                                .workflows
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .action_snapshot
                                .as_ref()
                                .map(|snapshot| snapshot.scope.clone())
                                .unwrap_or(scope);
                            if revision.status == "Draft" {
                                self.show_workflow_revision(scope, revision);
                            } else {
                                let snapshot = self
                                    .workflows
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .action_snapshot
                                    .clone();
                                if let Some(snapshot) = snapshot {
                                    self.chat_widget.show_selection_view(actions::menu(
                                        *snapshot,
                                        self.workflow_intent_sender(),
                                    ));
                                    self.workflows
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                                        .action_refresh_pending = true;
                                }
                            }
                        }
                    }
                    Err(error) => self.recover_workflow_control(*intent, error),
                }
                self.refresh_workflows(app_server);
            }
        }
        self.refresh_workflow_action_context(app_server);
    }

    fn workflow_control_error(&mut self, error: String) {
        self.workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .error = Some(error.clone());
        self.chat_widget.add_error_message(error);
    }

    fn show_workflow_revision(&mut self, scope: ActionScope, revision: RevisionPreview) {
        let snapshot = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .action_snapshot
            .clone();
        let budget = snapshot
            .as_ref()
            .map_or_else(String::new, |snapshot| snapshot.budget_summary.clone());
        let can_apply = if revision.status == "Draft"
            && snapshot.as_ref().is_some_and(|snapshot| {
                revision.parent_id.as_ref() == snapshot.active_revision_id.as_ref()
                    && snapshot.availability.iter().any(|available| {
                        available.action == Action::Revise && available.unavailable_reason.is_none()
                    })
            }) {
            Ok(())
        } else {
            Err("Only an unapplied draft of the current writable plan can be applied".into())
        };
        self.chat_widget.show_selection_view(actions::preview(
            scope,
            revision,
            budget,
            can_apply,
            self.workflow_intent_sender(),
        ));
    }
}
