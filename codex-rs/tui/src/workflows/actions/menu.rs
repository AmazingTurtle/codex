//! The existing native picker presents explicit service availability and captures immutable scope.
use super::model::*;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;

pub(crate) fn menu(snapshot: ActionSnapshot, send: IntentSender) -> SelectionViewParams {
    let mut items: Vec<_> = snapshot
        .availability
        .iter()
        .map(|availability| {
            let action = availability.action;
            let mut reason = availability.unavailable_reason.clone();
            if reason.is_none() {
                reason = match (action, &snapshot.scope.target) {
                    (
                        Action::Guide | Action::Interrupt,
                        ActionTarget::Step {
                            expected_attempt_id: Some(_),
                            expected_turn_id: Some(_),
                            ..
                        },
                    ) => None,
                    (Action::Guide | Action::Interrupt, _) => {
                        Some("Select an active step with a known current attempt and turn".into())
                    }
                    (Action::EditQueued | Action::Skip, ActionTarget::Step { .. }) => None,
                    (Action::EditQueued | Action::Skip, _) => {
                        Some("Select an exact queued step".into())
                    }
                    _ => None,
                };
            }
            if matches!(action, Action::ApplyDrain | Action::ApplyInterrupt)
                && snapshot.staged_revision_id.is_none()
            {
                reason = Some("Stage a revision before applying it".into());
            }
            if action == Action::Resume && snapshot.staged_revision_id.is_none() {
                reason = Some("No ready revision is available to resume".into());
            }
            let scope = snapshot.scope.clone();
            let snapshot = snapshot.clone();
            let send = send.clone();
            SelectionItem {
                name: action.title().into(),
                description: Some(action.description().into()),
                is_disabled: reason.is_some(),
                disabled_reason: reason,
                require_explicit_confirmation: matches!(
                    action,
                    Action::Interrupt | Action::ApplyInterrupt
                ),
                dismiss_on_select: true,
                actions: vec![Box::new(move |_| {
                    let intent = match action {
                        Action::Guide | Action::EditQueued | Action::Skip | Action::Revise => {
                            WorkflowUiIntent::OpenForm {
                                snapshot: Box::new(snapshot.clone()),
                                action,
                            }
                        }
                        Action::Interrupt => WorkflowUiIntent::Control {
                            scope: scope.clone(),
                            operation: ControlOperation::Interrupt,
                        },
                        Action::ApplyDrain | Action::ApplyInterrupt => {
                            let Some(revision_id) = snapshot.staged_revision_id.clone() else {
                                return;
                            };
                            WorkflowUiIntent::ApplyRevision {
                                scope: scope.clone(),
                                revision_id,
                                mode: if action == Action::ApplyDrain {
                                    RevisionApplyMode::DrainAccepted
                                } else {
                                    RevisionApplyMode::InterruptAndApply
                                },
                            }
                        }
                        Action::Resume => {
                            let Some(revision_id) = snapshot.staged_revision_id.clone() else {
                                return;
                            };
                            WorkflowUiIntent::Resume {
                                scope: scope.clone(),
                                revision_id,
                            }
                        }
                        Action::ShowResult => WorkflowUiIntent::ShowResult {
                            scope: scope.clone(),
                        },
                        Action::Revisions => WorkflowUiIntent::RevisionHistory {
                            scope: scope.clone(),
                            cursor: None,
                        },
                    };
                    send(intent);
                })],
                ..Default::default()
            }
        })
        .collect();
    let footer_scope = snapshot.scope.clone();
    let show_footer = snapshot.footer_hidden;
    items.push(SelectionItem {
        name: if show_footer {
            "Show in footer"
        } else {
            "Hide from footer"
        }
        .into(),
        description: Some("The workflow stays available in this command center".into()),
        dismiss_on_select: true,
        actions: vec![Box::new(move |_| {
            send(WorkflowUiIntent::SetFooterVisibility {
                scope: footer_scope.clone(),
                visible: show_footer,
            })
        })],
        ..Default::default()
    });
    SelectionViewParams {
        view_id: Some(super::ACTIONS_VIEW_ID),
        title: Some(format!("Actions · {}", snapshot.title)),
        subtitle: Some(format!(
            "Control revision {} · {}",
            snapshot.scope.expected_control_revision, snapshot.budget_summary
        )),
        items,
        ..SelectionViewParams::picker()
    }
}
