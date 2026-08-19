use super::*;
use crate::app::tests::make_test_app_with_channels;
use crate::chatwidget::ChatWidget;
use crate::chatwidget::ChatWidgetInit;
use app_test_support::ChatGptAuthFixture;
use app_test_support::write_chatgpt_auth;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::load_auth_dot_json;
use codex_login::save_auth;
use color_eyre::eyre::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::tempdir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn status_all_remains_responsive_and_shows_fast_accounts_before_a_slow_account_times_out()
-> Result<()> {
    // Synthetic persisted accounts exercise real auth selection and JSON-RPC routing. Only the
    // backend HTTP boundary is replaced; no operator accounts or live provider are involved.
    let backend = MockServer::start().await;
    let alternate_home = tempdir()?;
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    app.config.cli_auth_credentials_store_mode = AuthCredentialsStoreMode::File;
    app.config.chatgpt_base_url = backend.uri();
    std::fs::write(
        app.config.codex_home.join("config.toml"),
        format!(
            "chatgpt_base_url = {:?}\ncli_auth_credentials_store = \"file\"\n",
            backend.uri()
        ),
    )?;
    for (home, id, email) in [
        (
            app.config.codex_home.as_path(),
            "slow-account",
            "slow@example.com",
        ),
        (alternate_home.path(), "fast-account", "fast@example.com"),
    ] {
        write_chatgpt_auth(
            home,
            ChatGptAuthFixture::new("synthetic-status-token")
                .account_id(id)
                .chatgpt_account_id(id)
                .email(email)
                .plan_type("pro"),
            AuthCredentialsStoreMode::File,
        )
        .expect("write synthetic account auth");
    }
    let mut auth = load_auth_dot_json(
        &app.config.codex_home,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?
    .expect("active synthetic auth");
    auth.accounts.push(
        load_auth_dot_json(
            alternate_home.path(),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )?
        .expect("alternate synthetic auth"),
    );
    save_auth(
        &app.config.codex_home,
        &auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    for (id, delay) in [
        ("slow-account", Duration::from_secs(/*secs*/ 20)),
        ("fast-account", Duration::ZERO),
    ] {
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .and(header("chatgpt-account-id", id))
            .respond_with(
                ResponseTemplate::new(/*s*/ 200)
                    .set_delay(delay)
                    .set_body_json(json!({
                        "plan_type": "pro",
                        "rate_limit": {"allowed": true, "limit_reached": false,
                            "primary_window": {"used_percent": 42, "limit_window_seconds": 18000,
                                "reset_after_seconds": 1800, "reset_at": 2000000000}}
                    })),
            )
            .expect(/*r*/ 1)
            .mount(&backend)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
            "credits": [], "available_count": 0, "total_earned_count": 0
        })))
        .expect(/*r*/ 2)
        .mount(&backend)
        .await;
    let mut session = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    while events.try_recv().is_ok() {}
    app.chat_widget.insert_str("/status all");
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let mut card = None;
    let mut request = None;
    while let Ok(event) = events.try_recv() {
        match event {
            AppEvent::InsertHistoryCell(cell) => card = Some(cell),
            event @ AppEvent::ShowChatgptAccountStatus { .. } => request = Some(event),
            _ => {}
        }
    }
    let card = card.expect("slash command inserts status card");
    // This is a harness bound: the dispatcher must return while the backend is still pending.
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 1),
        app.handle_event(
            &mut tui,
            &mut session,
            request.expect("slash command requests account status"),
        ),
    )
    .await??;
    app.chat_widget.insert_str("input remains usable");
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "input remains usable"
    );

    let mut saw_partial = false;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(/*secs*/ 15), async {
            loop {
                let event = events.recv().await.expect("app event channel");
                if matches!(event, AppEvent::ChatgptAccountStatusLoaded { .. }) {
                    break event;
                }
            }
        })
        .await?;
        let AppEvent::ChatgptAccountStatusLoaded {
            result,
            pending_accounts,
            ..
        } = &event
        else {
            unreachable!();
        };
        let response = result.as_ref().expect("account list succeeds");
        let pending_accounts = *pending_accounts;
        if pending_accounts == 1 {
            assert_eq!(response.data.len(), 1);
            assert_eq!(response.data[0].account.account_id.as_str(), "fast-account");
            assert!(response.data[0].rate_limits.is_some());
            saw_partial = true;
        }
        if pending_accounts == 0 {
            assert!(
                saw_partial,
                "fast account should be visible before the stalled account settles"
            );
            assert_eq!(response.data.len(), 2);
            assert_eq!(
                response.data[1].error.as_deref(),
                Some("account limits timed out")
            );
        }
        app.handle_event(&mut tui, &mut session, event).await?;
        let rendered = card
            .display_lines(/*width*/ 110)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        if pending_accounts == 1 {
            assert!(rendered.contains("fast@example.com"));
            assert!(rendered.contains("58% left"));
            assert!(rendered.contains("1 pending…"));
        } else if pending_accounts == 0 {
            assert!(rendered.contains("fast@example.com"));
            assert!(rendered.contains("slow@example.com"));
            assert!(rendered.contains("unavailable — account limits timed out"));
            assert!(!rendered.contains("loading…"));
            break;
        }
    }
    let accounts = session.list_chatgpt_accounts().await?;
    assert_eq!(
        accounts
            .data
            .iter()
            .filter(|account| account.is_active)
            .map(|account| account.account_id.as_str())
            .collect::<Vec<_>>(),
        vec!["slow-account"]
    );
    session.shutdown().await?;
    backend.verify().await;
    Ok(())
}

#[tokio::test]
async fn status_all_without_stored_accounts_finishes_with_an_empty_state() -> Result<()> {
    for api_key in [None, Some("synthetic-status-api-key")] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        app.config.cli_auth_credentials_store_mode = AuthCredentialsStoreMode::File;
        std::fs::write(
            app.config.codex_home.join("config.toml"),
            "cli_auth_credentials_store = \"file\"\n",
        )?;
        if let Some(api_key) = api_key {
            codex_login::login_with_api_key(
                &app.config.codex_home,
                api_key,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
            )?;
        }
        let mut session =
            Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
        let mut tui = crate::tui::test_support::make_test_tui()?;
        while events.try_recv().is_ok() {}
        app.chat_widget.insert_str("/status all");
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let mut card = None;
        while let Ok(event) = events.try_recv() {
            match event {
                AppEvent::InsertHistoryCell(cell) => card = Some(cell),
                event @ AppEvent::ShowChatgptAccountStatus { .. } => {
                    app.handle_event(&mut tui, &mut session, event).await?;
                }
                _ => {}
            }
        }
        let event = tokio::time::timeout(Duration::from_secs(/*secs*/ 5), async {
            loop {
                let event = events.recv().await.expect("app event channel");
                if matches!(event, AppEvent::ChatgptAccountStatusLoaded { .. }) {
                    break event;
                }
            }
        })
        .await?;
        assert!(matches!(&event, AppEvent::ChatgptAccountStatusLoaded {
        result: Ok(response), pending_accounts: 0, ..
    } if response.data.is_empty()));
        app.handle_event(&mut tui, &mut session, event).await?;
        let rendered = card
            .expect("slash command inserts status card")
            .display_lines(/*width*/ 100)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("no eligible managed ChatGPT accounts"));
        assert!(!rendered.contains("loading…"));
        session.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn status_all_widget_replacement_finalizes_cards_and_rejects_old_account_results()
-> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut session = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    while events.try_recv().is_ok() {}
    app.chat_widget.insert_str("/status all");
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let mut old_card = None;
    while let Ok(event) = events.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            old_card = Some(cell);
        }
    }
    app.chat_widget.insert_str("/status old@example.com");
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    // Composer submissions resume transcript following before dispatching the command.
    assert!(matches!(events.try_recv()?, AppEvent::FollowTranscript));
    assert!(matches!(
        events.try_recv()?,
        AppEvent::ShowChatgptAccountStatus {
            request: ChatgptAccountStatusRequest::SelectedAccount { request_id: 1, selector }
        } if selector == "old@example.com"
    ));
    let replacement = ChatWidget::new_with_app_event(ChatWidgetInit {
        requires_openai_auth: true,
        local_settings: crate::local_settings::LocalSettings::from(&app.config),
        config: app.config.clone(),
        frame_requester: crate::tui::FrameRequester::test_dummy(),
        app_event_tx: app.app_event_tx.clone(),
        workspace_command_runner: None,
        initial_user_message: None,
        enhanced_keys_supported: app.enhanced_keys_supported,
        has_chatgpt_account: app.chat_widget.has_chatgpt_account(),
        has_codex_backend_auth: app.chat_widget.has_codex_backend_auth(),
        model_catalog: app.model_catalog.clone(),
        feedback: app.feedback.clone(),
        is_first_run: false,
        status_account_display: app.chat_widget.status_account_display().cloned(),
        initial_plan_type: app.chat_widget.current_plan_type(),
        model: Some(app.chat_widget.current_model().to_string()),
        startup_tooltip_override: None,
        status_line_invalid_items_warned: app.status_line_invalid_items_warned.clone(),
        terminal_title_invalid_items_warned: app.terminal_title_invalid_items_warned.clone(),
        session_telemetry: app.session_telemetry.clone(),
    });
    app.replace_chat_widget(replacement);
    let old_card = old_card.expect("old status card");
    let canceled = old_card
        .display_lines(/*width*/ 100)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(canceled.contains("status view changed while limits were loading"));
    assert!(!canceled.contains("loading…"));
    while events.try_recv().is_ok() {}
    app.chat_widget.insert_str("/status all");
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let mut new_card = None;
    while let Ok(event) = events.try_recv() {
        match event {
            AppEvent::InsertHistoryCell(cell) => new_card = Some(cell),
            AppEvent::ShowChatgptAccountStatus { request } => assert_eq!(
                request,
                ChatgptAccountStatusRequest::AllAccountsStatusCard { request_id: 2 },
            ),
            _ => {}
        }
    }
    app.chat_widget.insert_str("/status new@example.com");
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(events.try_recv()?, AppEvent::FollowTranscript));
    assert!(matches!(
        events.try_recv()?,
        AppEvent::ShowChatgptAccountStatus {
            request: ChatgptAccountStatusRequest::SelectedAccount { request_id: 3, selector }
        } if selector == "new@example.com"
    ));
    // Deliver transport completions through the real dispatcher after the view is replaced.
    for request in [
        ChatgptAccountStatusRequest::AllAccountsStatusCard { request_id: 0 },
        ChatgptAccountStatusRequest::SelectedAccount {
            request_id: 1,
            selector: "old@example.com".to_string(),
        },
    ] {
        app.handle_event(
            &mut tui,
            &mut session,
            AppEvent::ChatgptAccountStatusLoaded {
                request,
                result: Err("obsolete account result".to_string()),
                pending_accounts: 0,
            },
        )
        .await?;
    }
    assert!(
        events.try_recv().is_err(),
        "old selected output must not enter replacement history"
    );
    let new_card = new_card.expect("new status card");
    let still_pending = new_card
        .display_lines(/*width*/ 100)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(still_pending.contains("loading…"));
    assert!(!still_pending.contains("obsolete"));
    assert_eq!(
        old_card
            .display_lines(/*width*/ 100)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        canceled
    );
    app.handle_event(
        &mut tui,
        &mut session,
        AppEvent::ChatgptAccountStatusLoaded {
            request: ChatgptAccountStatusRequest::AllAccountsStatusCard { request_id: 2 },
            result: Ok(AccountRateLimitsReadManyResponse { data: Vec::new() }),
            pending_accounts: 0,
        },
    )
    .await?;
    app.handle_event(
        &mut tui,
        &mut session,
        AppEvent::ChatgptAccountStatusLoaded {
            request: ChatgptAccountStatusRequest::SelectedAccount {
                request_id: 3,
                selector: "new@example.com".to_string(),
            },
            result: Err("current selected account unavailable".to_string()),
            pending_accounts: 0,
        },
    )
    .await?;
    let completed = new_card
        .display_lines(/*width*/ 100)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(completed.contains("no eligible managed ChatGPT accounts"));
    assert!(!completed.contains("loading…"));
    let selected_output = match events.try_recv()? {
        AppEvent::InsertHistoryCell(cell) => cell,
        other => panic!("expected current selected output, got {other:?}"),
    };
    assert!(
        selected_output
            .display_lines(/*width*/ 100)
            .iter()
            .any(|line| line
                .to_string()
                .contains("current selected account unavailable"))
    );
    session.shutdown().await?;
    Ok(())
}
