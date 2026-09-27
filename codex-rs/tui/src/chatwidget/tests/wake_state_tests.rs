use super::*;
use codex_app_server_protocol::ThreadWakeState;
use codex_app_server_protocol::ThreadWakeStateChangedNotification;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn wake_activity_keeps_idle_input_available() {
    let (mut chat, _events, mut operations) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.set_wake_state(Some(ThreadWakeState {
        pending_count: 1,
        waiting: true,
    }));
    assert_eq!(chat.run_state_status_text(), "Waiting");
    assert!(!chat.bottom_pane.is_task_running());
    assert!(!chat.bottom_pane.status_indicator_visible());
    chat.handle_server_notification(
        ServerNotification::ThreadClosed(codex_app_server_protocol::ThreadClosedNotification {
            thread_id: chat.thread_id.unwrap().to_string(),
        }),
        Some(ReplayKind::ResumeInitialMessages),
    );
    assert_eq!(chat.run_state_status_text(), "Waiting");
    chat.apply_external_edit("continue here".to_string());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let Op::UserTurn { items, .. } = next_submit_op(&mut operations) else {
        panic!("expected a new user turn while waiting");
    };
    assert_eq!(
        items,
        vec![UserInput::Text {
            text: "continue here".to_string(),
            text_elements: Vec::new(),
        }]
    );
}

#[tokio::test]
async fn wake_activity_renders_idle_active_and_narrow_layouts() {
    let mut layouts = Vec::new();
    for (name, width, count, running, waiting) in [
        ("idle", 100, 1, false, true),
        ("idle_narrow", 34, 2, false, true),
        ("working", 100, 2, true, false),
        ("sleeping", 100, 1, true, true),
    ] {
        let (mut chat, _events, _operations) =
            make_chatwidget_manual(/*model_override*/ None).await;
        chat.thread_id = Some(ThreadId::new());
        chat.local_settings.tui.status_line = Some(vec!["run-state".to_string()]);
        if running {
            chat.on_task_started();
        }
        if running && waiting {
            chat.bottom_pane
                .set_hook_status_message(Some("Running hook".to_string()));
        }
        chat.track_unified_exec_process_begin("command", Some("process"), "sleep 60");
        chat.handle_server_notification(
            ServerNotification::ThreadWakeStateChanged(ThreadWakeStateChangedNotification {
                thread_id: chat.thread_id.unwrap().to_string(),
                wake_state: ThreadWakeState {
                    pending_count: count,
                    waiting,
                },
            }),
            /*replay_kind*/ None,
        );
        assert_eq!(
            chat.run_state_status_text(),
            if waiting { "Waiting" } else { "Working" }
        );
        let height = chat.desired_height(width);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| chat.render(frame.area(), frame.buffer_mut()))
            .unwrap();
        layouts.push(format!(
            "{name}:\n{}",
            normalized_backend_snapshot(terminal.backend())
        ));
        chat.set_wake_state(Some(ThreadWakeState::default()));
        assert_eq!(
            chat.run_state_status_text(),
            if running { "Working" } else { "Ready" }
        );
    }
    insta::assert_snapshot!(layouts.join("\n\n"));
}
