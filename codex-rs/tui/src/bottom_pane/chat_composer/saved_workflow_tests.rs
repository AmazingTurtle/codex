use super::*;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::WidgetRef;

fn composer(text: &str) -> ChatComposer {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        AppEventSender::new(tx),
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".into(),
        /*disable_paste_burst*/ true,
    );
    composer.set_saved_workflow_commands(vec![crate::workflows::saved_commands::SavedCommand {
        id: "project:source-file".into(),
        name: "saved-review".into(),
        description: "Review the current source".into(),
    }]);
    composer.set_text_content(text.into(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor(text.len());
    composer
}

#[test]
fn native_saved_command_uses_the_shared_popup_and_dispatches_scoped_id_with_arguments() {
    let mut composer = composer("/saved-review {\"n\":2}");
    let ActivePopup::Command(popup) = &composer.popups.active else {
        panic!("saved workflow appears in the native command popup");
    };
    assert!(
        matches!(popup.selected_item(), Some(CommandItem::Workflow(command)) if command.id == "project:source-file")
    );
    let area = Rect::new(0, 0, 80, 4);
    let mut buffer = Buffer::empty(area);
    popup.render_ref(area, &mut buffer);
    let rendered = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("saved_workflow_native_command_popup", rendered);
    let result = composer.handle_key_event(KeyCode::Enter.into()).0;
    assert!(
        matches!(&result, InputResult::WorkflowCommand { command, arguments: Some(arguments) } if command.id == "project:source-file" && arguments == "{\"n\":2}"),
        "{result:?}"
    );
    assert_eq!(
        composer.current_text(),
        "/saved-review {\"n\":2}",
        "inline input remains available to the normal preparation path"
    );
}

#[test]
fn bare_saved_command_is_not_model_input_and_readonly_child_keeps_its_blocked_draft() {
    let mut bare = composer("/saved-review");
    assert!(matches!(
        bare.handle_key_event(KeyCode::Enter.into()).0,
        InputResult::WorkflowCommand {
            arguments: None,
            ..
        }
    ));
    assert!(bare.current_text().is_empty());
    let mut blocked = composer("/saved-review human guidance");
    blocked.set_parent_owned_thread();
    assert!(matches!(
        blocked.handle_key_event(KeyCode::Enter.into()).0,
        InputResult::ParentOwnedInputBlocked
    ));
    assert_eq!(blocked.current_text(), "/saved-review human guidance");
}
