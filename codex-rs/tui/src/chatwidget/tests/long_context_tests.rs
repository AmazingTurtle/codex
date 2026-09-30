use super::*;
use pretty_assertions::assert_eq;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

#[tokio::test]
async fn long_context_commands_request_changes_and_reject_bad_args() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.dispatch_command_with_args(SlashCommand::LongContext, "on".to_string(), Vec::new());
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::SetLongContext { enabled: true })
    ));
    chat.set_long_context(/*long_context*/ true);
    chat.dispatch_command(SlashCommand::LongContext);
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::SetLongContext { enabled: false })
    ));
    chat.dispatch_command_with_args(SlashCommand::LongContext, "off".to_string(), Vec::new());
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::SetLongContext { enabled: false })
    ));
    chat.dispatch_command_with_args(SlashCommand::LongContext, "invalid".to_string(), Vec::new());
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::SetLongContext { .. }))
    );
    assert_eq!(chat.config.long_context, true);
}

#[tokio::test]
async fn long_context_status_tracks_effective_support_with_plan_and_fast() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(Some("gpt-5.4")).await;
    set_chatgpt_auth(&mut chat);
    set_fast_mode_test_catalog(&mut chat);
    for model in &mut Arc::make_mut(&mut chat.model_catalog).models {
        model.supports_long_context = model.model == "gpt-5.4";
    }
    chat.show_welcome_banner = false;
    chat.local_settings.tui.status_line = Some(vec![
        "model-with-reasoning".to_string(),
        "long-context".to_string(),
    ]);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::High));
    chat.set_service_tier(Some(ServiceTier::Fast.request_value().to_string()));
    chat.set_long_context(/*long_context*/ true);
    for (model, snapshot) in [
        ("gpt-5.4", "long_context_on"),
        ("gpt-5.2", "long_context_unavailable"),
        ("gpt-5.4", "long_context_restored"),
    ] {
        chat.set_model(model);
        let width = 90;
        let mut terminal =
            Terminal::new(TestBackend::new(width, chat.desired_height(width))).unwrap();
        terminal
            .draw(|frame| chat.render(frame.area(), frame.buffer_mut()))
            .unwrap();
        assert_chatwidget_snapshot!(snapshot, normalized_backend_snapshot(terminal.backend()));
    }
    assert_eq!(chat.config.long_context, true);
    let mask =
        collaboration_modes::plan_mask(chat.model_catalog.as_ref()).expect("plan mode available");
    chat.set_collaboration_mask_from_user_action(mask);
    let width = 90;
    let mut terminal = Terminal::new(TestBackend::new(width, chat.desired_height(width))).unwrap();
    terminal
        .draw(|frame| chat.render(frame.area(), frame.buffer_mut()))
        .unwrap();
    assert_chatwidget_snapshot!(
        "long_context_plan",
        normalized_backend_snapshot(terminal.backend())
    );
    chat.set_long_context(/*long_context*/ false);
    assert_eq!(
        chat.status_line_value_for_item(StatusLineItem::LongContext),
        Some("Long off".to_string())
    );
}
