//! Drives reminder dismissal through keyboard events, host persistence and reload.

use super::*;
use crate::security_setup::Identity;
use crate::security_setup::Notice;
use app_test_support::ChatGptAuthFixture;
use app_test_support::write_chatgpt_auth;
use codex_login::AuthCredentialsStoreMode;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn security_setup_permanent_dismissal_survives_reload_and_keeps_other_identities()
-> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let path = app.local_settings.user_config_path.clone();
    let backend = wiremock::MockServer::start().await;
    app.config.chatgpt_base_url = backend.uri();
    app.config.cli_auth_credentials_store_mode = AuthCredentialsStoreMode::File;
    std::fs::write(
        &path,
        format!(
            "personality = \"pragmatic\"\nchatgpt_base_url = {:?}\n",
            backend.uri()
        ),
    )?;
    write_chatgpt_auth(
        &app.config.codex_home,
        ChatGptAuthFixture::new("test-token")
            .account_id("account")
            .chatgpt_user_id("user"),
        AuthCredentialsStoreMode::File,
    )
    .expect("write fixture auth");
    app_test_support::mount_workspace_routing(&backend).await;
    wiremock::Mock::given(wiremock::matchers::path("/wham/security-setup"))
        .respond_with(wiremock::ResponseTemplate::new(500))
        .expect(0)
        .mount(&backend)
        .await;
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.handle_event(
        &mut tui,
        &mut server,
        AppEvent::PersistRateLimitSwitchPromptHidden,
    )
    .await?;
    let identity = Identity {
        account: "account".into(),
        user: "user".into(),
    };
    let notice: Notice = serde_json::from_value(serde_json::json!({
        "title": "Set up security for Daybreak mode", "description": "Set up account security.",
        "action": {"label": "Set up security", "url": "https://chatgpt.com/cyber"}
    }))?;
    for expected_account in [
        identity.clone(),
        Identity {
            account: "other-account".into(),
            ..identity.clone()
        },
    ] {
        app.chat_widget
            .show_security_setup(expected_account.clone(), notice.clone());
        assert!(render_bottom_popup(&app.chat_widget, /*width*/ 70).contains("Don't show again"));
        while events.try_recv().is_ok() {}
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        let event = events.try_recv().expect("permanent dismissal action");
        assert!(
            matches!(&event, AppEvent::DismissSecuritySetupPermanently { identity, .. } if identity == &expected_account)
        );
        // Input queued after choosing the action must survive the asynchronous save.
        app.chat_widget.apply_external_edit("Keep my draft.".into());
        app.handle_event(&mut tui, &mut server, event).await?;
        assert_eq!(
            app.chat_widget.composer_text_with_pending(),
            "Keep my draft."
        );
        assert_eq!(
            app.local_settings.notices.hide_rate_limit_model_nudge,
            Some(true)
        );
        app.chat_widget.apply_external_edit(String::new());
        assert!(!render_bottom_popup(&app.chat_widget, /*width*/ 70).contains("Set up security"));
    }
    // A newly loaded config and widget share no dismissal state with the old widget.
    let config = app
        .rebuild_config_for_cwd(app.config.cwd.to_path_buf())
        .await?;
    assert_eq!(config.notices.security_setup_dismissed.len(), 2);
    assert_eq!(config.notices.hide_rate_limit_model_nudge, Some(true));
    assert!(std::fs::read_to_string(&path)?.contains("personality = \"pragmatic\""));
    app.local_settings = crate::local_settings::LocalSettings::from(&config);
    let (tx, mut fetch_events) = mpsc::unbounded_channel();
    crate::security_setup::prefetch(
        &config,
        &app.local_settings.notices,
        &server,
        AppEventSender::new(tx),
        uuid::Uuid::new_v4(),
    );
    // Closing the only sender proves the task finished without fetching the reminder.
    assert!(
        tokio::time::timeout(Duration::from_secs(5), fetch_events.recv())
            .await?
            .is_none()
    );
    backend.verify().await;
    let init = app.chatwidget_init_for_forked_or_resumed_thread(
        &mut tui, config, /*initial_user_message*/ None,
    );
    app.chat_widget = ChatWidget::new_with_app_event(init);
    for dismissed in [
        identity.clone(),
        Identity {
            account: "other-account".into(),
            ..identity.clone()
        },
    ] {
        app.chat_widget
            .show_security_setup(dismissed, notice.clone());
        assert!(!render_bottom_popup(&app.chat_widget, /*width*/ 70).contains("Set up security"));
    }
    app.chat_widget.show_security_setup(
        Identity {
            user: "another-user".into(),
            ..identity
        },
        notice,
    );
    assert!(render_bottom_popup(&app.chat_widget, /*width*/ 70).contains("Don't show again"));
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn security_setup_failed_save_is_retryable_and_stale_actions_do_not_persist() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let path = app.local_settings.user_config_path.clone();
    std::fs::write(&path, "")?;
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let identity = Identity {
        account: "account".into(),
        user: "user".into(),
    };
    let notice: Notice = serde_json::from_value(serde_json::json!({
        "title": "Set up security", "description": "Set up account security.",
        "action": {"label": "Set up security", "url": "https://chatgpt.com/cyber"}
    }))?;
    app.chat_widget
        .show_security_setup(identity.clone(), notice.clone());
    render_bottom_popup(&app.chat_widget, /*width*/ 70);
    while events.try_recv().is_ok() {}
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
    let action = events.try_recv().expect("permanent dismissal action");
    std::fs::write(&path, "invalid = [\n")?;
    app.handle_event(&mut tui, &mut server, action).await?;
    let cell = match events.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        event => panic!("expected save error, got {event:?}"),
    };
    assert!(
        lines_to_single_string(&cell.display_lines(/*width*/ 100))
            .contains("Couldn't save your security reminder preference")
    );
    assert!(render_bottom_popup(&app.chat_widget, /*width*/ 70).contains("Don't show again"));
    assert!(
        app.local_settings
            .notices
            .security_setup_dismissed
            .is_empty()
    );
    assert_eq!(std::fs::read_to_string(&path)?, "invalid = [\n");

    std::fs::write(&path, "")?;
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
    let stale = events.try_recv().expect("retry action");
    app.chat_widget.invalidate_security_setup();
    app.chat_widget.show_security_setup(
        Identity {
            account: "switched".into(),
            ..identity
        },
        notice,
    );
    app.handle_event(&mut tui, &mut server, stale).await?;
    assert_eq!(std::fs::read_to_string(&path)?, "");
    assert!(render_bottom_popup(&app.chat_widget, /*width*/ 70).contains("Don't show again"));
    while events.try_recv().is_ok() {}
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
    app.handle_event(
        &mut tui,
        &mut server,
        events.try_recv().expect("retry for current account"),
    )
    .await?;
    assert_eq!(app.local_settings.notices.security_setup_dismissed.len(), 1);
    assert!(!render_bottom_popup(&app.chat_widget, /*width*/ 70).contains("Set up security"));
    server.shutdown().await?;
    Ok(())
}
