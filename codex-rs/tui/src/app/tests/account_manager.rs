use super::*;
use app_test_support::ChatGptAuthFixture;
use app_test_support::write_chatgpt_auth;
use codex_config::types::AuthCredentialsStoreMode;
use codex_protocol::openai_models::ReasoningEffort;

#[tokio::test]
async fn account_manager_shows_ineligible_accounts_and_removal_action() -> Result<()> {
    let mut app = make_test_app().await;
    app.config.cli_auth_credentials_store_mode = AuthCredentialsStoreMode::File;
    app.config.forced_chatgpt_workspace_id = Some(vec!["allowed-account".to_string()]);
    write_chatgpt_auth(
        &app.config.codex_home,
        ChatGptAuthFixture::new("blocked-token")
            .account_id("blocked-account")
            .chatgpt_account_id("blocked-account")
            .email("blocked@example.com")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )
    .expect("write blocked ChatGPT auth");
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.handle_event(&mut tui, &mut app_server, AppEvent::OpenAccountManager)
        .await?;

    insta::assert_snapshot!(
        "account_manager_with_only_ineligible_accounts",
        render_bottom_popup(&app.chat_widget, /*width*/ 100)
    );
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn account_model_fallback_switches_and_persists_the_selected_model() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    app.config.cli_auth_credentials_store_mode = AuthCredentialsStoreMode::File;
    write_chatgpt_auth(
        &app.config.codex_home,
        ChatGptAuthFixture::new("account-token")
            .account_id("account-id")
            .chatgpt_account_id("account-id")
            .email("account@example.com")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )
    .expect("write ChatGPT auth");
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    while app_event_rx.try_recv().is_ok() {}

    app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::ApplyChatgptAccountModelFallback {
            account_id: "account-id".to_string(),
            model: "gpt-5.4-mini".to_string(),
            effort: ReasoningEffort::Medium,
        },
    )
    .await?;

    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| {
            matches!(event, AppEvent::UpdateModel(model) if model == "gpt-5.4-mini")
        })
    );
    assert!(events.iter().any(|event| {
        matches!(
            event,
            AppEvent::PersistModelSelection {
                model,
                effort: Some(ReasoningEffort::Medium),
            } if model == "gpt-5.4-mini"
        )
    }));
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn removing_the_last_account_exits_the_tui() -> Result<()> {
    let mut app = make_test_app().await;
    app.config.cli_auth_credentials_store_mode = AuthCredentialsStoreMode::File;
    write_chatgpt_auth(
        &app.config.codex_home,
        ChatGptAuthFixture::new("account-token")
            .account_id("account-id")
            .chatgpt_account_id("account-id")
            .email("account@example.com")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )
    .expect("write ChatGPT auth");
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    let control = app
        .handle_event(
            &mut tui,
            &mut app_server,
            AppEvent::RemoveChatgptAccount {
                account_id: "account-id".to_string(),
            },
        )
        .await?;

    assert!(matches!(
        control,
        AppRunControl::Exit(ExitReason::UserRequested)
    ));
    app_server.shutdown().await?;
    Ok(())
}
