//! Worker rows expose resolved settings and open the exact ordinary agent conversation.
use super::*;
use pretty_assertions::assert_eq;

#[test]
fn enter_opens_the_selected_worker_while_phase_remains_a_selection_context() {
    let state = fixture();
    {
        let mut state = state.lock().unwrap();
        state.rows[0].children.insert(
            0,
            WorkflowChild {
                key: "phase:Review".into(),
                label: "Review · Active".into(),
                depth: 1,
                transcript_target: Some(
                    codex_app_server_protocol::WorkflowTranscriptTarget::Phase {
                        phase: "Review".into(),
                    },
                ),
                ..Default::default()
            },
        );
        state.rows[0].children[1].depth = 2;
    }
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Down.into());
    center.handle_key_event(KeyCode::Enter.into());
    assert!(events.try_recv().is_err());
    assert!(!center.is_complete());
    center.handle_key_event(KeyCode::Down.into());
    let key = state.lock().unwrap().selected_node_key();
    center.handle_key_event(KeyCode::Char('3').into());
    assert_eq!(state.lock().unwrap().selected_node_key(), key);
    center.handle_key_event(KeyCode::Enter.into());
    assert!(
        matches!(events.try_recv(),Ok(AppEvent::Workflow(WorkflowEvent::SelectAgent(id))) if id==ThreadId::from_u128(42))
    );
    assert!(center.is_complete());
    assert_eq!(state.lock().unwrap().selected_node_key(), key);
}

#[test]
fn pending_step_keeps_its_missing_child_and_explicit_history_opens_its_own_worker() {
    let state = fixture();
    {
        let mut state = state.lock().unwrap();
        state.rows[0].children[0].thread_id = None;
        state.center.show_attempt_history = true;
        state.child = Some(0);
    }
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Enter.into());
    assert!(events.try_recv().is_err());
    assert!(!center.is_complete());
    center.handle_key_event(KeyCode::Down.into());
    assert_eq!(state.lock().unwrap().child, Some(1));
    center.handle_key_event(KeyCode::Enter.into());
    assert!(
        matches!(events.try_recv(),Ok(AppEvent::Workflow(WorkflowEvent::SelectAgent(id))) if id==ThreadId::from_u128(43))
    );
}

fn worker_settings_center() -> WorkflowCenter {
    let state = fixture();
    {
        let mut state = state.lock().unwrap();
        state.child = Some(0);
        state.center.show_attempt_history = true;
        state.rows[0].children[0].resolved_model = Some("gpt-6.1-sol".into());
        state.rows[0].children[0].reasoning_effort = Some("high".into());
    }
    let (tx, _events) = tokio::sync::mpsc::unbounded_channel();
    WorkflowCenter::new(state, AppEventSender::new(tx))
}

#[test]
fn worker_table_shows_resolved_settings_and_honest_unknowns_without_moving_the_split() {
    let center = worker_settings_center();
    let wide = render(&center, 320, 16);
    for text in ["Model", "Reasoning", "gpt-6.1-sol", "high", "Unknown"] {
        assert!(wide.contains(text), "missing {text}: {wide}");
    }
    let divider = wide
        .lines()
        .nth(4)
        .unwrap()
        .chars()
        .position(|value| value == '│')
        .unwrap();
    assert!((94..=99).contains(&divider), "30% list divider: {divider}");
    let area = Rect::new(0, 0, 320, 16);
    let mut buffer = Buffer::empty(area);
    center.render(area, &mut buffer);
    let (y, row) = wide
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("gpt-6.1-sol"))
        .unwrap();
    let locate = |needle| crate::width::display_width(&row[..row.find(needle).unwrap()]) as u16;
    let label = &buffer[(locate("review"), y as u16)];
    assert_eq!(buffer[(locate("gpt-6.1-sol"), y as u16)].fg, label.fg);
    assert_eq!(buffer[(locate("high"), y as u16)].fg, label.fg);
    insta::assert_snapshot!("workflow_worker_settings_wide", wide);
}

#[test]
fn compact_worker_table_keeps_the_actual_model_and_reasoning_columns() {
    let center = worker_settings_center();
    let compact = render(&center, 78, 14);
    for text in ["Model", "Reasoning", "gpt-6.1-sol", "high"] {
        assert!(compact.contains(text), "missing {text}: {compact}");
    }
    insta::assert_snapshot!("workflow_worker_settings_compact", compact);
}

#[test]
fn narrow_worker_table_advertises_the_exact_worker_chat_shortcut() {
    let center = worker_settings_center();
    let narrow = render(&center, 42, 10);
    assert!(narrow.contains("Enter chat"), "{narrow}");
    insta::assert_snapshot!("workflow_worker_settings_narrow", narrow);
}
