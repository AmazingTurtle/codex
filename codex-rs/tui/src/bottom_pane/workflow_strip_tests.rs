use super::*;
use crate::render::renderable::Renderable;
use crate::workflows::WorkflowRow;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;

fn pane_with_workflow() -> BottomPane {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let pane = super::super::tests::test_pane(AppEventSender::new(tx));
    pane.workflow_strip
        .0
        .lock()
        .unwrap()
        .rows
        .push(WorkflowRow {
            id: "review".into(),
            title: "Review source".into(),
            status: "Running".into(),
            ..Default::default()
        });
    pane
}

#[test]
fn down_preserves_drafts_attachments_paste_bursts_and_history_search() {
    let setups: [fn(&mut BottomPane); 4] = [
        |pane| pane.composer.insert_str("draft"),
        |pane| pane.set_remote_image_urls(vec!["https://example.com/image.png".into()]),
        |pane| {
            pane.handle_key_event(KeyCode::Char('x').into());
            assert!(pane.composer.is_in_paste_burst());
            assert_eq!(pane.composer.recovery_snapshot().text, "x");
        },
        |pane| {
            pane.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
            assert!(!pane.no_modal_or_popup_active());
        },
    ];
    for setup in setups {
        let mut pane = pane_with_workflow();
        setup(&mut pane);
        let draft = pane.composer.recovery_snapshot();
        let attachments = pane.remote_image_urls();
        assert!(!pane.handle_workflow_navigation(KeyCode::Down.into()));
        pane.handle_key_event(KeyCode::Down.into());
        assert!(!pane.workflow_strip.0.lock().unwrap().strip_focused);
        let actual = pane.composer.recovery_snapshot();
        assert_eq!(
            (actual.text, actual.text_elements),
            (draft.text, draft.text_elements)
        );
        assert_eq!(pane.remote_image_urls(), attachments);
    }
}

#[test]
fn down_ignores_modified_repeated_and_released_keys_and_empty_workflows() {
    let mut pane = pane_with_workflow();
    for key in [
        KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
        KeyEvent::new_with_kind(KeyCode::Down, KeyModifiers::NONE, KeyEventKind::Repeat),
        KeyEvent::new_with_kind(KeyCode::Down, KeyModifiers::NONE, KeyEventKind::Release),
        KeyCode::Left.into(),
    ] {
        assert!(!pane.handle_workflow_navigation(key));
        assert!(!pane.workflow_strip.0.lock().unwrap().strip_focused);
    }
    pane.workflow_strip.0.lock().unwrap().rows.clear();
    assert!(!pane.handle_workflow_navigation(KeyCode::Down.into()));
}

#[test]
fn down_respects_editor_remaps_and_disabled_input() {
    use codex_config::types::KeybindingSpec;
    use codex_config::types::KeybindingsSpec;
    use codex_config::types::TuiKeymap;

    let mut pane = pane_with_workflow();
    let mut config = TuiKeymap::default();
    config.editor.move_down = Some(KeybindingsSpec::One(KeybindingSpec("ctrl-n".into())));
    pane.set_keymap_bindings(&RuntimeKeymap::from_config(&config).unwrap());
    assert!(!pane.handle_workflow_navigation(KeyCode::Down.into()));
    pane.set_keymap_bindings(&RuntimeKeymap::defaults());
    pane.composer
        .set_input_enabled(/*enabled*/ false, /*placeholder*/ None);
    assert!(!pane.handle_workflow_navigation(KeyCode::Down.into()));
    assert!(!pane.workflow_strip.0.lock().unwrap().strip_focused);
}

#[test]
fn one_down_opens_the_center_from_the_row_or_inspection_hint() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut pane = super::super::tests::test_pane(AppEventSender::new(tx));
    pane.workflow_strip
        .0
        .lock()
        .unwrap()
        .rows
        .push(WorkflowRow {
            id: "review".into(),
            title: "Review source".into(),
            status: "Running".into(),
            ..Default::default()
        });
    assert_eq!(pane.workflow_strip.desired_height(80), 1);
    assert!(pane.handle_workflow_navigation(KeyCode::Down.into()));
    assert!(matches!(
        rx.try_recv().unwrap(),
        AppEvent::Workflow(crate::workflows::WorkflowEvent::Open)
    ));
    {
        let mut state = pane.workflow_strip.0.lock().unwrap();
        state.rows.clear();
        state.inspected_agent = Some(codex_protocol::ThreadId::from_u128(42));
    }
    assert_eq!(pane.workflow_strip.desired_height(80), 1);
    assert!(pane.handle_workflow_navigation(KeyCode::Down.into()));
    assert!(matches!(
        rx.try_recv().unwrap(),
        AppEvent::Workflow(crate::workflows::WorkflowEvent::Open)
    ));
}

#[test]
fn multiple_workflow_rows_render_below_the_configured_status_and_preserve_the_draft() {
    let mut pane = pane_with_workflow();
    pane.composer.insert_str("draft to preserve");
    pane.set_status_line_enabled(/*enabled*/ true);
    pane.set_status_line(Some(ratatui::text::Line::from("Configured status line")));
    {
        let mut state = pane.workflow_strip.0.lock().unwrap();
        for index in 2..=5 {
            state.rows.push(WorkflowRow {
                id: format!("run-{index}"),
                title: format!("Workflow {index}"),
                status: "Running".into(),
                ..Default::default()
            });
        }
    }
    assert_eq!(pane.workflow_strip.desired_height(120), 5);
    let area = ratatui::layout::Rect::new(0, 0, 120, 16);
    let mut buffer = ratatui::buffer::Buffer::empty(area);
    pane.render(area, &mut buffer);
    let lines = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>();
    let draft = lines
        .iter()
        .position(|line| line.contains("draft to preserve"))
        .expect("the composer draft remains visible");
    let status = lines
        .iter()
        .position(|line| line.contains("Configured status line"))
        .unwrap_or_else(|| panic!("configured status remains visible:\n{}", lines.join("\n")));
    let first_workflow = lines
        .iter()
        .position(|line| line.contains("Review source"))
        .expect("the first workflow has its own row");
    assert!(first_workflow > draft && first_workflow > status);
    assert!(lines[first_workflow + 1].contains("Workflow 2"));
    assert!(lines[first_workflow + 2].contains("Workflow 3"));
    assert!(lines[first_workflow + 3].contains("Workflow 4"));
    assert!(lines[first_workflow + 4].contains("+1 workflow"));
    assert_eq!(pane.composer.recovery_snapshot().text, "draft to preserve");
    insta::assert_snapshot!("workflow_rows_below_configured_status", lines.join("\n"));
}
