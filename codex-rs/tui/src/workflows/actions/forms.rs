//! Shared multiline editors retain input through validation and emit intents, never RPCs.
use super::model::*;
use crate::bottom_pane::custom_prompt_view::CustomPromptView;
use crate::keymap::RuntimeKeymap;
use serde_json::Value;

fn json_input(text: &str, empty: Value) -> Result<Value, String> {
    if text.trim().is_empty() {
        return Ok(empty);
    }
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("Enter JSON: {error}"))?;
    if serde_json::to_vec(&value)
        .map_err(|error| error.to_string())?
        .len()
        > 16 * 1024
    {
        return Err("JSON input must fit 16KiB".into());
    }
    Ok(value)
}

pub(crate) fn form(
    snapshot: ActionSnapshot,
    action: Action,
    send: IntentSender,
    keymap: &RuntimeKeymap,
) -> Option<CustomPromptView> {
    build_form(snapshot, action, None, send, keymap)
}

pub(crate) fn form_with_error(
    snapshot: ActionSnapshot,
    action: Action,
    text: String,
    error: String,
    send: IntentSender,
    keymap: &RuntimeKeymap,
) -> Option<CustomPromptView> {
    build_form(snapshot, action, Some((text, error)), send, keymap)
}

fn build_form(
    snapshot: ActionSnapshot,
    action: Action,
    draft: Option<(String, String)>,
    send: IntentSender,
    keymap: &RuntimeKeymap,
) -> Option<CustomPromptView> {
    let (placeholder, initial, context) = match action {
        Action::Guide => (
            "Guidance for the active agent",
            String::new(),
            "Guide the accepted turn; its model and permissions stay unchanged",
        ),
        Action::EditQueued => (
            "Replacement queued prompt",
            snapshot.queued_prompt.clone().unwrap_or_default(),
            "Only a prompt that has not started can be replaced",
        ),
        Action::Skip => (
            "JSON replacement",
            "null".into(),
            "Use null to skip without a result, or provide the replacement JSON value",
        ),
        Action::Revise => (
            "Canonical workflow JavaScript",
            snapshot.source.clone(),
            "Stage first. The preview identifies preserved work and reruns; the existing budget remains",
        ),
        Action::Interrupt
        | Action::ApplyDrain
        | Action::ApplyInterrupt
        | Action::Resume
        | Action::ShowResult
        | Action::Revisions => return None,
    };
    let (initial, context) = draft.map_or_else(
        || (initial, context.to_owned()),
        |(text, error)| (text, format!("{error} · {context}")),
    );
    let scope = snapshot.scope;
    let input = snapshot.input;
    let mut view = CustomPromptView::new(
        action.title().into(),
        placeholder.into(),
        initial,
        Some(context),
        Box::new(move |text| {
            let intent = match action {
                Action::Guide => WorkflowUiIntent::Control {
                    scope: scope.clone(),
                    operation: ControlOperation::Guide { message: text },
                },
                Action::EditQueued => WorkflowUiIntent::Control {
                    scope: scope.clone(),
                    operation: ControlOperation::EditQueued { prompt: text },
                },
                Action::Skip => {
                    let Ok(replacement) = json_input(&text, Value::Null) else {
                        return;
                    };
                    WorkflowUiIntent::Control {
                        scope: scope.clone(),
                        operation: ControlOperation::Skip { replacement },
                    }
                }
                Action::Revise => WorkflowUiIntent::RevisionArguments {
                    scope: scope.clone(),
                    source: text,
                    initial_input: input.clone(),
                },
                Action::Interrupt
                | Action::ApplyDrain
                | Action::ApplyInterrupt
                | Action::Resume
                | Action::ShowResult
                | Action::Revisions => return,
            };
            send(intent);
        }),
    )
    .preserve_whitespace()
    .with_validation(Box::new(move |text| match action {
        Action::Guide | Action::EditQueued if text.trim().is_empty() => {
            Err("Enter a prompt".into())
        }
        Action::Guide | Action::EditQueued if text.len() > 8192 => {
            Err("The prompt must fit 8192 UTF-8 bytes".into())
        }
        Action::Skip => json_input(text, Value::Null).map(drop),
        Action::Revise if text.trim().is_empty() => Err("Enter workflow source".into()),
        Action::Revise if text.len() > 24 * 1024 => Err("Workflow source must fit 24KiB".into()),
        _ => Ok(()),
    }));
    if action == Action::Revise {
        view = view.with_child_flow("arguments");
    }
    view.set_keymap_bindings(keymap);
    Some(view)
}

pub(crate) fn revision_arguments(
    scope: ActionScope,
    source: String,
    input: Value,
    send: IntentSender,
    keymap: &RuntimeKeymap,
) -> CustomPromptView {
    revision_arguments_draft(
        scope,
        source,
        serde_json::to_string_pretty(&input).unwrap_or_default(),
        "Staging does not apply or save the revision.".into(),
        send,
        keymap,
    )
}

pub(crate) fn revision_arguments_draft(
    scope: ActionScope,
    source: String,
    text: String,
    context: String,
    send: IntentSender,
    keymap: &RuntimeKeymap,
) -> CustomPromptView {
    let mut view = CustomPromptView::new(
        "Revision arguments".into(),
        "JSON arguments".into(),
        text,
        Some(context),
        Box::new(move |text| {
            if let Ok(input) = json_input(&text, serde_json::json!({})) {
                send(WorkflowUiIntent::StageRevision {
                    scope: scope.clone(),
                    source: source.clone(),
                    input,
                });
            }
        }),
    )
    .preserve_whitespace()
    .with_validation(Box::new(|text| {
        json_input(text, serde_json::json!({})).map(drop)
    }));
    view.set_keymap_bindings(keymap);
    view
}
