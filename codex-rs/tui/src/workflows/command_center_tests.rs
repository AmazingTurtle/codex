//! Keyboard journeys keep filtering, library selection, and detail focus independent.
use super::*;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;

#[test]
fn default_scope_is_current_and_project_filters_preserve_selected_identity() {
    let state = fixture();
    {
        let mut state = state.lock().unwrap();
        for (id, status, owner) in [
            ("failed", "Failed", 7),
            ("interrupted", "Interrupted", 8),
            ("completed", "Completed", 7),
            ("stopped", "Stopped", 7),
            ("pending", "Pending", 9),
        ] {
            state.rows.push(WorkflowRow {
                id: id.into(),
                title: id.into(),
                status: status.into(),
                owner: Some(ThreadId::from_u128(owner)),
                project_root: Some("/project".into()),
                ..Default::default()
            });
        }
        assert_eq!(
            (state.scope, state.center.grouping),
            (WorkflowListScope::Current, WorkflowGrouping::Conversation)
        );
        state.scope = WorkflowListScope::Project;
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    let screen = render(&center, 140, 22);
    for label in [
        "All 6",
        "Needs you 2",
        "Working 2",
        "Completed 1",
        "Inactive 1",
    ] {
        assert!(screen.contains(label), "missing {label}: {screen}");
    }
    insta::assert_snapshot!("workflow_status_count_filters", screen);
    center.handle_key_event(KeyCode::Tab.into());
    {
        let state = state.lock().unwrap();
        assert!(!state.center.saved);
        assert_eq!(
            state
                .visible_run_indices()
                .iter()
                .map(|index| state.rows[*index].id.as_str())
                .collect::<Vec<_>>(),
            vec!["failed", "interrupted"]
        );
        assert_eq!(state.rows[state.selected].id, "failed");
    }
    center.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
    {
        let state = state.lock().unwrap();
        assert_eq!(state.rows[state.selected].id, "failed");
    }
    center.handle_key_event(KeyCode::Char('f').into());
    assert!(matches!(
        rx.try_recv().unwrap(),
        AppEvent::Workflow(WorkflowEvent::ChooseScope)
    ));
    assert_eq!(state.lock().unwrap().scope, WorkflowListScope::Project);
    center.handle_key_event(KeyCode::Char('g').into());
    assert_eq!(
        state.lock().unwrap().center.grouping,
        WorkflowGrouping::Status
    );
    center.handle_key_event(KeyCode::Char('g').into());
    assert_eq!(
        state.lock().unwrap().center.grouping,
        WorkflowGrouping::Project
    );
    assert!(
        rx.try_recv().is_err(),
        "grouping never changes the loaded scope"
    );
}

#[test]
fn paused_current_work_needs_attention_without_relabeling_completed_or_foreign_runs() {
    let state = fixture();
    let mut state = state.lock().unwrap();
    state.execution = Some(codex_protocol::protocol::ExecutionControlState::Paused);
    let completed = WorkflowRow {
        status: "Completed".into(),
        ..state.rows[0].clone()
    };
    let foreign = WorkflowRow {
        owner: Some(ThreadId::from_u128(8)),
        ..state.rows[0].clone()
    };
    assert_eq!(
        (
            state.row_status(&state.rows[0]),
            state.status_group(&state.rows[0])
        ),
        ("Paused", 1)
    );
    assert_eq!(
        (state.row_status(&completed), state.status_group(&completed)),
        ("Completed", 3)
    );
    assert_eq!(
        (state.row_status(&foreign), state.status_group(&foreign)),
        ("Running", 2)
    );
}

#[test]
fn empty_status_filter_cannot_read_or_control_an_invisible_run() {
    let state = fixture();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Tab.into());
    for key in [
        KeyCode::Enter,
        KeyCode::Char('s'),
        KeyCode::Char('r'),
        KeyCode::Char('c'),
        KeyCode::Char('o'),
        KeyCode::Char('2'),
    ] {
        center.handle_key_event(key.into());
    }
    assert!(
        rx.try_recv().is_err(),
        "an empty filter has no action target"
    );
    assert!(!state.lock().unwrap().center.details_focused);
    let screen = render(&center, 120, 18);
    assert!(!screen.contains("Review source") && !screen.contains("Review changes"));
    insta::assert_snapshot!("workflow_empty_status_filter", screen);
}

#[test]
fn library_filters_do_not_replace_run_filters_and_escape_returns_before_closing() {
    let state = fixture();
    state.lock().unwrap().saved.push(SavedWorkflowRow {
        id: "user-script".into(),
        name: "User script".into(),
        scope: Some(codex_app_server_protocol::WorkflowScope::User),
        ..Default::default()
    });
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Char('l').into());
    center.handle_key_event(KeyCode::End.into());
    assert_eq!(
        state.lock().unwrap().center.saved_id.as_deref(),
        Some("user-script")
    );
    center.handle_key_event(KeyCode::Home.into());
    assert_eq!(
        state.lock().unwrap().center.saved_id.as_deref(),
        Some("saved-review")
    );
    center.handle_key_event(KeyCode::Tab.into());
    {
        let state = state.lock().unwrap();
        assert_eq!(
            (state.center.status_filter, state.center.library_filter),
            (0, 1)
        );
        assert_eq!(state.visible_saved_indices(), vec![0]);
    }
    center.handle_key_event(KeyCode::Tab.into());
    {
        let state = state.lock().unwrap();
        assert_eq!(state.visible_saved_indices(), vec![1]);
        assert_eq!(state.center.saved_id.as_deref(), Some("user-script"));
    }
    insta::assert_snapshot!("workflow_library_user_filter", render(&center, 100, 14));
    center.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
    assert_eq!(state.lock().unwrap().visible_saved_indices(), vec![0]);
    center.handle_key_event(KeyCode::Esc.into());
    assert!(!state.lock().unwrap().center.saved);
    assert!(!center.is_complete());
    center.handle_key_event(KeyCode::Esc.into());
    assert!(center.is_complete());
}

#[test]
fn numeric_detail_shortcuts_work_from_list_and_do_not_steal_json_input() {
    let state = fixture();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Down.into());
    center.handle_key_event(KeyCode::Down.into());
    center.handle_key_event(KeyCode::Char('3').into());
    {
        let state = state.lock().unwrap();
        assert_eq!(
            (state.center.detail_tab, state.center.details_focused),
            (DetailTab::Source, false)
        );
    }
    assert!(render(&center, 120, 18).contains("const result"));
    center.handle_key_event(KeyCode::Char('2').into());
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::ReadTranscript { target, older: false }) if target.target == codex_app_server_protocol::WorkflowTranscriptTarget::Step {step_id:"review".into()})
    );
    center.handle_key_event(KeyCode::Char('1').into());
    center.handle_key_event(KeyCode::Char('l').into());
    center.handle_key_event(KeyCode::Enter.into());
    center.handle_paste("[".into());
    for character in ['1', ',', '2', ',', '3'] {
        center.handle_key_event(KeyCode::Char(character).into());
    }
    center.handle_paste("]".into());
    center.handle_key_event(KeyCode::Enter.into());
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::StartSaved { input, .. }) if input == serde_json::json!([1, 2, 3]))
    );
    assert_eq!(state.lock().unwrap().center.detail_tab, DetailTab::Overview);
}

#[test]
fn narrow_numeric_view_reveals_details_and_escape_restores_the_selected_list_row() {
    let state = fixture();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Down.into());
    render(&center, 42, 14);
    center.handle_key_event(KeyCode::Char('3').into());
    {
        let state = state.lock().unwrap();
        assert_eq!(
            (state.center.detail_tab, state.center.details_focused),
            (DetailTab::Source, true)
        );
    }
    let source = render(&center, 42, 14);
    assert!(source.contains("const result"), "{source}");
    insta::assert_snapshot!("workflow_narrow_numeric_source", source);
    center.handle_key_event(KeyCode::Esc.into());
    {
        let state = state.lock().unwrap();
        assert!(!state.center.details_focused);
        assert_eq!(state.selected_node_key(), "workflow-1:step:review");
    }
    assert!(render(&center, 42, 14).contains("Review changes"));
}

#[test]
fn center_help_and_narrow_detail_back_action_remain_discoverable() {
    let state = fixture();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state, AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Char('?').into());
    let help = render(&center, 100, 24);
    let labels = help.to_lowercase();
    assert!(
        labels.contains("tab") && labels.contains("filter"),
        "{help}"
    );
    assert!(
        labels.contains("scope") && labels.contains("group"),
        "{help}"
    );
    insta::assert_snapshot!("workflow_center_help", help);
    center.handle_key_event(KeyCode::Esc.into());
    assert!(!center.is_complete());
    render(&center, 42, 10);
    center.handle_key_event(KeyCode::Char('1').into());
    let narrow = render(&center, 42, 10);
    assert!(
        narrow.contains("Esc") && narrow.contains("list"),
        "{narrow}"
    );
    insta::assert_snapshot!("workflow_numeric_views_narrow", narrow);
}

#[test]
fn overview_keeps_all_object_records_as_literal_markdown_and_narrow_values_reachable() {
    let state = fixture();
    {
        let mut state = state.lock().unwrap();
        let overview = state.rows[0].overview.as_mut().unwrap();
        overview.input = Some(
            serde_json::json!({"n":15,"convention":"fib(0)=0, fib(1)=1","literal":"*literal* | [link](not-a-url)"}),
        );
        overview.result = Some(
            serde_json::json!({"results":[{"language":"Python","value":610},{"language":"Rust","value":610},{"language":"JavaScript","value":610}]}),
        );
    }
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state, AppEventSender::new(tx));
    let wide = render(&center, 140, 30);
    assert!(wide.contains("n: 15"), "{wide}");
    assert!(
        wide.contains("*literal* | [link](not-a-url)"),
        "literal values must not become Markdown controls: {wide}"
    );
    assert_eq!(wide.matches("value: 610").count(), 3, "{wide}");
    let mut previous = 0;
    for language in ["Python", "Rust", "JavaScript"] {
        let position = wide[previous..]
            .find(language)
            .map(|offset| previous + offset)
            .expect("each record keeps its identity");
        let value = wide[position..]
            .find("value: 610")
            .expect("each record keeps its value");
        previous = position + value + "value: 610".len();
    }
    assert!(!wide.contains("{\"results\""));
    insta::assert_snapshot!("workflow_compact_structured_result", wide);
    render(&center, 42, 20);
    center.handle_key_event(KeyCode::Char('1').into());
    let narrow = render(&center, 42, 20);
    center.handle_key_event(KeyCode::PageDown.into());
    let bottom = render(&center, 42, 20);
    assert!(
        bottom.contains("JavaScript") && bottom.contains("value: 610"),
        "the final record remains reachable in a narrow pane: {bottom}"
    );
    insta::assert_snapshot!(
        "workflow_compact_structured_result_narrow",
        format!("Beginning\n{narrow}\n\nEnd\n{bottom}")
    );
}

#[test]
fn arrows_select_rows_j_k_scroll_four_rows_and_page_keys_jump_to_detail_boundaries() {
    let state = fixture();
    {
        let mut state = state.lock().unwrap();
        state.rows[0].children = (0..20)
            .map(|index| WorkflowChild {
                key: format!("step:{index}"),
                label: format!("Review file {index} · Running"),
                depth: 1,
                overview: Some(WorkflowOverview {
                    title: format!("Review file {index}"),
                    prompt: Some(
                        (0..80)
                            .map(|line| format!("Prompt line {line}\n"))
                            .collect(),
                    ),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect();
    }
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Down.into());
    let details = |screen: String| {
        screen
            .lines()
            .filter_map(|line| line.split_once('│').map(|(_, detail)| detail.to_owned()))
            .collect::<Vec<_>>()
    };
    let initial = details(render(&center, 120, 12));
    center.handle_key_event(KeyCode::Char('k').into());
    let scrolled = details(render(&center, 120, 12));
    assert_ne!(scrolled, initial);
    assert_eq!(state.lock().unwrap().child, Some(0));
    center.handle_key_event(KeyCode::End.into());
    assert_eq!(state.lock().unwrap().child, Some(19));
    center.handle_key_event(KeyCode::Home.into());
    assert_eq!(state.lock().unwrap().selected_node_key(), "run:workflow-1");
    center.handle_key_event(KeyCode::Down.into());
    assert_eq!(details(render(&center, 120, 12)), scrolled);
    center.handle_key_event(KeyCode::Char('j').into());
    assert_eq!(details(render(&center, 120, 12)), initial);
    for _ in 0..12 {
        center.handle_key_event(KeyCode::Down.into());
    }
    let selected = state.lock().unwrap().child.unwrap();
    let page = render(&center, 120, 12);
    assert!(page.contains(&format!("Review file {selected}")), "{page}");
    assert!(state.lock().unwrap().center.list_scroll > 0);
    insta::assert_snapshot!("workflow_center_paged_list", page);
    let selected_initial = details(render(&center, 120, 12));
    center.handle_key_event(KeyCode::Char('k').into());
    let selected_scrolled = details(render(&center, 120, 12));
    assert_ne!(selected_scrolled, selected_initial);
    center.handle_key_event(KeyCode::Up.into());
    render(&center, 120, 12);
    center.handle_key_event(KeyCode::Down.into());
    assert_eq!(details(render(&center, 120, 12)), selected_scrolled);
    center.handle_key_event(KeyCode::PageDown.into());
    let bottom = details(render(&center, 120, 12));
    assert!(
        bottom.iter().any(|line| line.contains("Prompt line 79")),
        "{bottom:?}"
    );
    for _ in 0..8 {
        center.handle_key_event(KeyEvent::new_with_kind(
            KeyCode::PageDown,
            KeyModifiers::NONE,
            crossterm::event::KeyEventKind::Repeat,
        ));
    }
    assert_eq!(details(render(&center, 120, 12)), bottom);
    center.handle_key_event(KeyCode::PageUp.into());
    assert_eq!(state.lock().unwrap().child, Some(selected));
    assert_eq!(details(render(&center, 120, 12)), selected_initial);
    center.handle_key_event(KeyCode::Esc.into());
    assert!(center.is_complete());
}

#[test]
fn configured_navigation_preserves_fixed_library_and_detail_shortcuts() {
    let state = fixture();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut keymap = crate::keymap::RuntimeKeymap::defaults().list;
    keymap.move_down = vec![crate::key_hint::ctrl(KeyCode::Char('n'))];
    keymap.move_right = vec![crate::key_hint::plain(KeyCode::Char('l'))];
    let mut center =
        WorkflowCenter::new_with_keymap(state.clone(), AppEventSender::new(tx), keymap);
    center.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    assert_eq!(state.lock().unwrap().child, Some(0));
    center.handle_key_event(KeyCode::Char('l').into());
    {
        let state = state.lock().unwrap();
        assert!(
            state.center.saved,
            "Saved remains available with a vim-style right binding"
        );
        assert_eq!(state.center.detail_tab, DetailTab::Overview);
    }
    center.handle_key_event(KeyCode::Char('3').into());
    {
        let state = state.lock().unwrap();
        assert!(state.center.saved);
        assert_eq!(
            (state.center.detail_tab, state.center.saved_detail_tab),
            (DetailTab::Overview, DetailTab::Source)
        );
    }
    center.handle_key_event(KeyCode::Esc.into());
    {
        let state = state.lock().unwrap();
        assert!(!state.center.saved);
        assert_eq!(state.selected_node_key(), "workflow-1:step:review");
    }
    center.handle_key_event(KeyCode::Char('3').into());
    assert_eq!(state.lock().unwrap().center.detail_tab, DetailTab::Source);
}

#[test]
fn brackets_always_page_the_list_and_p_reads_older_transcript_without_losing_selection() {
    let state = fixture();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Char(']').into());
    assert!(matches!(
        rx.try_recv().unwrap(),
        AppEvent::Workflow(WorkflowEvent::NextPage { saved: false })
    ));
    center.handle_key_event(KeyCode::Down.into());
    center.handle_key_event(KeyCode::Down.into());
    center.handle_key_event(KeyCode::Char('2').into());
    assert!(matches!(
        rx.try_recv().unwrap(),
        AppEvent::Workflow(WorkflowEvent::ReadTranscript { older: false, .. })
    ));
    let transcript_key = selected_transcript_key(&state);
    state
        .lock()
        .unwrap()
        .transcripts
        .insert(transcript_key, WorkflowTranscript::default());
    center.handle_key_event(KeyCode::PageDown.into());
    assert!(
        rx.try_recv().is_err(),
        "scrolling a cached transcript does not fetch it again or require Enter"
    );
    for key in ['[', ']'] {
        center.handle_key_event(KeyCode::Char(key).into());
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::Workflow(WorkflowEvent::PreviousPage { saved: false })
                | AppEvent::Workflow(WorkflowEvent::NextPage { saved: false })
        ));
    }
    center.handle_key_event(KeyCode::Char('p').into());
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::ReadTranscript { target, older: true }) if target.target == codex_app_server_protocol::WorkflowTranscriptTarget::Step {step_id:"review".into()})
    );
    center.handle_key_event(KeyCode::Char('R').into());
    assert!(matches!(
        rx.try_recv().unwrap(),
        AppEvent::Workflow(WorkflowEvent::Refresh)
    ));
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::ReadTranscript { target, older: false }) if target.target == codex_app_server_protocol::WorkflowTranscriptTarget::Step {step_id:"review".into()})
    );
    assert!(matches!(
        rx.try_recv().unwrap(),
        AppEvent::Workflow(WorkflowEvent::RefreshSavedCatalog)
    ));
    assert_eq!(
        state.lock().unwrap().selected_node_key(),
        "workflow-1:step:review"
    );
    center.handle_key_event(KeyCode::Char('l').into());
    center.handle_key_event(KeyCode::Char('[').into());
    assert!(matches!(
        rx.try_recv().unwrap(),
        AppEvent::Workflow(WorkflowEvent::PreviousPage { saved: true })
    ));
}

#[test]
fn attempt_history_opens_the_selected_attempt_and_hiding_it_returns_to_the_latest_step() {
    let state = fixture();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Down.into());
    assert_eq!(
        state.lock().unwrap().selected_thread(),
        Some(ThreadId::from_u128(42))
    );
    assert!(!render(&center, 140, 20).contains("Attempt 1"));
    center.handle_key_event(KeyCode::Char('a').into());
    center.handle_key_event(KeyCode::Down.into());
    assert_eq!(
        state.lock().unwrap().selected_thread(),
        Some(ThreadId::from_u128(43))
    );
    let history = render(&center, 140, 20);
    assert!(
        history.contains("Review tool failed before producing a result."),
        "{history}"
    );
    assert!(
        !history.contains("Code is ready") && !history.contains("Result"),
        "a failed attempt has no successful result: {history}"
    );
    insta::assert_snapshot!("workflow_earlier_attempt_history", history);
    center.handle_key_event(KeyCode::Char('a').into());
    {
        let state = state.lock().unwrap();
        assert_eq!(
            (state.selected_node_key(), state.selected_thread()),
            (
                "workflow-1:step:review".into(),
                Some(ThreadId::from_u128(42))
            )
        );
    }
    center.handle_key_event(KeyCode::Char('a').into());
    center.handle_key_event(KeyCode::Down.into());
    center.handle_key_event(KeyCode::Char('o').into());
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::SelectAgent(id)) if id == ThreadId::from_u128(43))
    );
}

#[test]
fn hiding_cached_history_requests_the_latest_steps_uncached_transcript() {
    let state = fixture();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Char('a').into());
    center.handle_key_event(KeyCode::Down.into());
    center.handle_key_event(KeyCode::Down.into());
    center.handle_key_event(KeyCode::Char('2').into());
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::ReadTranscript { target, older: false }) if target.target == codex_app_server_protocol::WorkflowTranscriptTarget::Attempt {step_id:"review".into(),attempt_id:"reviewer".into()})
    );
    let transcript_key = selected_transcript_key(&state);
    state
        .lock()
        .unwrap()
        .transcripts
        .insert(transcript_key, WorkflowTranscript::default());
    center.handle_key_event(KeyCode::Char('a').into());
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::ReadTranscript { target, older: false }) if target.target == codex_app_server_protocol::WorkflowTranscriptTarget::Step {step_id:"review".into()})
    );
    let state = state.lock().unwrap();
    assert_eq!(
        (state.selected_node_key(), state.center.detail_tab),
        ("workflow-1:step:review".into(), DetailTab::Transcript)
    );
}

#[test]
fn filtering_to_another_run_requests_its_uncached_transcript() {
    let state = fixture();
    state.lock().unwrap().rows.push(WorkflowRow {
        id: "failed-run".into(),
        title: "Failed run".into(),
        status: "Failed".into(),
        owner: Some(ThreadId::from_u128(8)),
        ..Default::default()
    });
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Down.into());
    center.handle_key_event(KeyCode::Char('2').into());
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::ReadTranscript { target, older: false }) if target.target == codex_app_server_protocol::WorkflowTranscriptTarget::Step {step_id:"review".into()})
    );
    let transcript_key = selected_transcript_key(&state);
    state
        .lock()
        .unwrap()
        .transcripts
        .insert(transcript_key, WorkflowTranscript::default());
    center.handle_key_event(KeyCode::Tab.into());
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::ReadTranscript { target, older: false }) if target.owner == ThreadId::from_u128(8) && target.run_id == "failed-run" && target.target == codex_app_server_protocol::WorkflowTranscriptTarget::Run)
    );
    assert_eq!(state.lock().unwrap().selected_node_key(), "run:failed-run");
}

#[test]
fn center_keeps_workflow_bold_step_identity_colors_and_history_dim_in_both_palettes() {
    use ratatui::style::Modifier;
    for (fg, bg) in [
        ((240, 240, 240), (20, 20, 20)),
        ((20, 20, 20), (250, 250, 250)),
    ] {
        crate::terminal_palette::with_test_default_colors(
            crate::terminal_probe::DefaultColors { fg, bg },
            || {
                let state = fixture();
                state.lock().unwrap().center.show_attempt_history = true;
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                let center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
                let area = Rect::new(0, 0, 140, 20);
                let mut buffer = Buffer::empty(area);
                center.render(area, &mut buffer);
                let locate = |buffer: &Buffer, needle: &str| {
                    (0..area.height)
                        .find_map(|y| {
                            let text = (0..area.width)
                                .map(|x| buffer[(x, y)].symbol())
                                .collect::<String>();
                            text.find(needle).map(|x| (x as u16, y))
                        })
                        .expect(needle)
                };
                let root = locate(&buffer, "Review changes");
                let step = locate(&buffer, "review · Running");
                let history = locate(&buffer, "review · Attempt 1");
                assert!(buffer[root].modifier.contains(Modifier::BOLD));
                assert_eq!(buffer[step].fg, buffer[history].fg);
                assert!(!buffer[step].modifier.contains(Modifier::DIM));
                assert!(buffer[history].modifier.contains(Modifier::DIM));
                assert_eq!(
                    buffer[step].fg,
                    crate::style::readable_color_on(buffer[step].fg, None)
                );
                let original = buffer[step].fg;
                state.lock().unwrap().rows[0].children[0].thread_id = Some(ThreadId::from_u128(99));
                center.render(area, &mut buffer);
                assert_eq!(
                    buffer[step].fg, original,
                    "retry identities do not change the step color"
                );
                let divider = (0..area.width)
                    .find(|x| buffer[(*x, 4)].symbol() == "│")
                    .unwrap();
                assert!(
                    (28..=48).contains(&divider),
                    "the list occupies about 30%: {divider}"
                );
            },
        );
    }
}

#[test]
fn space_captures_selected_step_and_actual_caller_but_never_intercepts_saved_json_input() {
    let state = fixture();
    state.lock().unwrap().displayed_thread = Some(ThreadId::from_u128(42));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state, AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Down.into());
    center.handle_key_event(KeyCode::Char(' ').into());
    assert!(
        matches!(rx.try_recv().unwrap(),AppEvent::Workflow(WorkflowEvent::ActionsOpen {owner,caller,run_id,target:actions::ActionTarget::Step {logical_id,expected_attempt_id,expected_turn_id:None},..}) if owner==ThreadId::from_u128(7) && caller==ThreadId::from_u128(42) && run_id=="workflow-1" && logical_id=="review" && expected_attempt_id.as_deref()==Some("reviewer-latest"))
    );
    center.handle_key_event(KeyEvent::new_with_kind(
        KeyCode::Char(' '),
        KeyModifiers::NONE,
        crossterm::event::KeyEventKind::Repeat,
    ));
    assert!(
        rx.try_recv().is_err(),
        "holding Space does not reopen the action picker"
    );
    center.handle_key_event(KeyCode::Char('l').into());
    center.handle_key_event(KeyCode::Enter.into());
    center.handle_paste("{\"text\":\"hello".into());
    center.handle_key_event(KeyCode::Char(' ').into());
    center.handle_paste("world\"}".into());
    center.handle_key_event(KeyCode::Enter.into());
    assert!(
        matches!(rx.try_recv().unwrap(),AppEvent::Workflow(WorkflowEvent::StartSaved {input,..}) if input==serde_json::json!({"text":"hello world"}))
    );
}

#[test]
fn foreign_project_row_actions_require_opening_its_owning_conversation() {
    let state = fixture();
    state.lock().unwrap().rows[0].owner = Some(ThreadId::from_u128(8));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state, AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Char(' ').into());
    assert!(rx.try_recv().is_err());
    let screen = render(&center, 120, 18);
    assert!(screen.contains("Open the owning conversation"), "{screen}");
    insta::assert_snapshot!("workflow_foreign_actions_require_owner", screen);
}
