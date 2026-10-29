//! Revision previews make the compiler's reuse boundary visible before an explicit apply.
use super::model::*;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::render::renderable::Renderable;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;

pub(crate) fn preview(
    scope: ActionScope,
    revision: RevisionPreview,
    budget: String,
    can_apply: Result<(), String>,
    send: IntentSender,
) -> SelectionViewParams {
    let reason = can_apply.err();
    let mut lines = vec![
        format!("Revision {} · {}", revision.number, revision.status)
            .bold()
            .into(),
        format!(
            "Preserve {} / {} statements",
            revision.reusable_statements, revision.statement_count
        )
        .into(),
        format!(
            "Rerun {} statements",
            revision
                .statement_count
                .saturating_sub(revision.reusable_statements)
        )
        .into(),
        revision.invalidation_reason.clone().dim().into(),
        format!("Same workflow budget · {budget}").dim().into(),
    ];
    if let Some(error) = &revision.error {
        lines.push(Line::from(error.clone().red()));
    }
    let mut items: Vec<_> = [
        (
            RevisionApplyMode::DrainAccepted,
            "Apply after accepted work",
            "Keep accepted agents running, then replay the safe prefix",
        ),
        (
            RevisionApplyMode::InterruptAndApply,
            "Interrupt and apply",
            "Interrupt accepted agents before replaying the safe prefix",
        ),
    ]
    .into_iter()
    .map(|(mode, title, description)| {
        let scope = scope.clone();
        let revision_id = revision.id.clone();
        let send = send.clone();
        SelectionItem {
            name: title.into(),
            description: Some(description.into()),
            is_disabled: reason.is_some(),
            disabled_reason: reason.clone(),
            require_explicit_confirmation: mode == RevisionApplyMode::InterruptAndApply,
            dismiss_on_select: true,
            actions: vec![Box::new(move |_| {
                send(WorkflowUiIntent::ApplyRevision {
                    scope: scope.clone(),
                    revision_id: revision_id.clone(),
                    mode,
                })
            })],
            ..Default::default()
        }
    })
    .collect();
    for (index, execution_id) in revision.execution_ids.iter().enumerate() {
        let scope = scope.clone();
        let send = send.clone();
        let execution_id = execution_id.clone();
        let revision_id = revision.id.clone();
        items.push(SelectionItem {
            name: format!("Inspect execution {}", index + 1),
            description: Some(
                "Read this execution's immutable result, source, and owned agent chat".into(),
            ),
            dismiss_on_select: true,
            actions: vec![Box::new(move |_| {
                send(WorkflowUiIntent::ViewExecution {
                    scope: scope.clone(),
                    revision_id: revision_id.clone(),
                    execution_id: execution_id.clone(),
                })
            })],
            ..Default::default()
        });
    }
    let inspect_scope = scope;
    let inspect_send = send.clone();
    items.push(SelectionItem {
        name: "Inspect source and arguments".into(),
        description: Some("Read the exact staged source before applying".into()),
        dismiss_on_select: true,
        actions: vec![Box::new(move |_| {
            inspect_send(WorkflowUiIntent::ViewRevisionSource {
                scope: inspect_scope.clone(),
                revision: Box::new(revision.clone()),
            })
        })],
        ..Default::default()
    });
    SelectionViewParams {
        view_id: Some(super::REVISION_PREVIEW_VIEW_ID),
        title: Some("Review staged workflow revision".into()),
        subtitle: Some("Staging has not applied or saved this revision".into()),
        header: Box::new(Paragraph::new(lines).wrap(Wrap { trim: false })) as Box<dyn Renderable>,
        items,
        ..SelectionViewParams::picker()
    }
}

pub(crate) fn history(
    scope: ActionScope,
    revisions: Vec<RevisionPreview>,
    next_cursor: Option<String>,
    send: IntentSender,
) -> SelectionViewParams {
    let mut items: Vec<_> = revisions
        .into_iter()
        .map(|revision| {
            let scope = scope.clone();
            let send = send.clone();
            SelectionItem {
                name: format!("Revision {} · {}", revision.number, revision.status),
                description: Some(format!(
                    "{} / {} statements reusable · {}",
                    revision.reusable_statements,
                    revision.statement_count,
                    revision.invalidation_reason
                )),
                dismiss_on_select: true,
                actions: vec![Box::new(move |_| {
                    send(WorkflowUiIntent::InspectRevision {
                        scope: scope.clone(),
                        revision: Box::new(revision.clone()),
                    })
                })],
                ..Default::default()
            }
        })
        .collect();
    if let Some(cursor) = next_cursor {
        items.push(SelectionItem {
            name: "More revisions".into(),
            dismiss_on_select: true,
            actions: vec![Box::new(move |_| {
                send(WorkflowUiIntent::RevisionHistory {
                    scope: scope.clone(),
                    cursor: Some(cursor.clone()),
                })
            })],
            ..Default::default()
        });
    }
    SelectionViewParams {
        title: Some("Workflow revision history".into()),
        items,
        ..SelectionViewParams::picker()
    }
}
