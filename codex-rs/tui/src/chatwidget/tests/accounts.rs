use super::*;
use codex_app_server_protocol::AccountModelsReadResult;
use codex_app_server_protocol::AccountRateLimitsReadManyResponse;
use codex_app_server_protocol::AccountRateLimitsReadResult;
use codex_app_server_protocol::AccountRateLimitsSnapshot;
use codex_app_server_protocol::AccountTokenUsageDailyBucket;
use codex_app_server_protocol::AccountTokenUsageSnapshot;
use codex_app_server_protocol::AccountTokenUsageSummary;
use codex_app_server_protocol::AccountUsageReadManyResponse;
use codex_app_server_protocol::AccountUsageReadResult;
use codex_app_server_protocol::ChatgptAccountSummary;
use codex_app_server_protocol::RateLimitSnapshot;
use codex_app_server_protocol::RateLimitWindow;
use codex_protocol::account::PlanType;

fn account(
    account_id: &str,
    email: &str,
    plan_type: PlanType,
    is_active: bool,
) -> ChatgptAccountSummary {
    ChatgptAccountSummary {
        account_id: account_id.to_string(),
        email: Some(email.to_string()),
        plan_type,
        is_active,
        is_eligible: true,
    }
}

fn model(model: &str, display_name: &str) -> ApiModel {
    ApiModel {
        id: model.to_string(),
        model: model.to_string(),
        upgrade: None,
        upgrade_info: None,
        availability_nux: None,
        display_name: display_name.to_string(),
        description: format!("{display_name} description"),
        model_specialty: None,
        hidden: false,
        supported_reasoning_efforts: Vec::new(),
        default_reasoning_effort: ReasoningEffortConfig::Medium,
        input_modalities: Vec::new(),
        supports_personality: false,
        multi_agent_version: None,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        is_default: false,
    }
}

#[test]
fn account_usage_buckets_are_aggregated_by_calendar_date() {
    let buckets = vec![
        AccountTokenUsageDailyBucket {
            start_date: "2026-08-09".to_string(),
            tokens: 10,
        },
        AccountTokenUsageDailyBucket {
            start_date: "2026-08-03".to_string(),
            tokens: 20,
        },
        AccountTokenUsageDailyBucket {
            start_date: "2026-08-09".to_string(),
            tokens: 5,
        },
        AccountTokenUsageDailyBucket {
            start_date: "2026-08-10".to_string(),
            tokens: 100,
        },
        AccountTokenUsageDailyBucket {
            start_date: "invalid".to_string(),
            tokens: 100,
        },
        AccountTokenUsageDailyBucket {
            start_date: "2026-08-08".to_string(),
            tokens: -50,
        },
    ];
    let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 9).expect("valid date");

    assert_eq!(
        crate::chatwidget::account_status::tokens_in_date_range(&buckets, today, today),
        15
    );
    assert_eq!(
        crate::chatwidget::account_status::tokens_in_date_range(
            &buckets,
            today - chrono::Duration::days(6),
            today,
        ),
        35
    );
}

#[tokio::test]
async fn account_status_output_is_snapshot_covered() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.add_account_status_output(AccountRateLimitsReadManyResponse {
        data: vec![
            AccountRateLimitsReadResult {
                account: account("account-pro", "pro@example.com", PlanType::Pro, true),
                rate_limits: Some(AccountRateLimitsSnapshot {
                    rate_limits: RateLimitSnapshot {
                        limit_id: Some("codex".to_string()),
                        limit_name: None,
                        primary: Some(RateLimitWindow {
                            used_percent: 25,
                            window_duration_mins: Some(300),
                            resets_at: None,
                        }),
                        secondary: Some(RateLimitWindow {
                            used_percent: 60,
                            window_duration_mins: Some(10_080),
                            resets_at: None,
                        }),
                        credits: None,
                        individual_limit: None,
                        spend_control_reached: None,
                        plan_type: Some(PlanType::Pro),
                        rate_limit_reached_type: None,
                    },
                    rate_limits_by_limit_id: None,
                    rate_limit_reset_credits: None,
                }),
                error: None,
            },
            AccountRateLimitsReadResult {
                account: account("account-free", "free@example.com", PlanType::Free, false),
                rate_limits: None,
                error: Some("catalog unavailable".to_string()),
            },
        ],
    });
    let rendered = drain_insert_history(&mut rx)
        .iter()
        .map(|cell| lines_to_single_string(cell))
        .collect::<Vec<_>>()
        .join("\n");

    assert_chatwidget_snapshot!("account_status_with_unavailable_account", rendered);
}

#[tokio::test]
async fn account_usage_daily_output_is_snapshot_covered() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let today = chrono::Utc::now()
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();
    chat.add_account_usage_output(
        TokenActivityView::Daily,
        AccountUsageReadManyResponse {
            data: vec![AccountUsageReadResult {
                account: account("account-pro", "pro@example.com", PlanType::Pro, true),
                usage: Some(AccountTokenUsageSnapshot {
                    summary: AccountTokenUsageSummary {
                        lifetime_tokens: Some(99),
                        peak_daily_tokens: Some(15),
                        longest_running_turn_sec: None,
                        current_streak_days: None,
                        longest_streak_days: None,
                    },
                    daily_usage_buckets: Some(vec![AccountTokenUsageDailyBucket {
                        start_date: today,
                        tokens: 15,
                    }]),
                }),
                error: None,
            }],
        },
    );
    let rendered = drain_insert_history(&mut rx)
        .iter()
        .map(|cell| lines_to_single_string(cell))
        .collect::<Vec<_>>()
        .join("\n");

    assert_chatwidget_snapshot!("account_usage_daily_calendar_bucket", rendered);
}

#[tokio::test]
async fn account_manager_shows_identity_plan_capability_and_management_actions() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.show_account_manager(vec![
        AccountModelsReadResult {
            account: account("account-pro", "pro@example.com", PlanType::Pro, true),
            models: Some(Vec::new()),
            error: None,
        },
        AccountModelsReadResult {
            account: account("account-free", "free@example.com", PlanType::Free, false),
            models: None,
            error: Some("catalog timed out".to_string()),
        },
    ]);

    assert_chatwidget_snapshot!(
        "account_manager_with_mixed_model_capabilities",
        render_bottom_popup(&chat, /*width*/ 100)
    );
}

#[tokio::test]
async fn account_manager_emits_switch_for_a_compatible_account() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let current_model = chat.current_model().to_string();
    chat.show_account_manager(vec![AccountModelsReadResult {
        account: account("account-pro", "pro@example.com", PlanType::Pro, false),
        models: Some(vec![model(&current_model, "Current model")]),
        error: None,
    }]);

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::SwitchChatgptAccount { account_id })
            if account_id == "account-pro"
    );
}

#[tokio::test]
async fn account_manager_emits_model_fallback_for_an_incompatible_account() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let requested_model = chat.current_model().to_string();
    let fallback = model("gpt-5.4-mini", "GPT-5.4 Mini");
    chat.show_account_manager(vec![AccountModelsReadResult {
        account: account("account-free", "free@example.com", PlanType::Free, false),
        models: Some(vec![fallback.clone()]),
        error: None,
    }]);

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let Ok(AppEvent::OpenAccountModelFallback {
        account,
        models,
        requested_model: event_requested_model,
    }) = rx.try_recv()
    else {
        panic!("expected account model fallback event");
    };
    assert_eq!(account.account_id, "account-free");
    assert_eq!(models, vec![fallback]);
    assert_eq!(event_requested_model, requested_model);
}

#[tokio::test]
async fn account_model_fallback_and_removal_emit_selected_actions() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.show_account_model_fallback(
        account("account-free", "free@example.com", PlanType::Free, false),
        vec![model("gpt-5.4-mini", "GPT-5.4 Mini")],
        "gpt-5.5".to_string(),
    );
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::ApplyChatgptAccountModelFallback {
            account_id,
            model,
            ..
        }) if account_id == "account-free" && model == "gpt-5.4-mini"
    );

    chat.show_remove_account_confirmation(account(
        "account-free",
        "free@example.com",
        PlanType::Free,
        false,
    ));
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::RemoveChatgptAccount { account_id })
            if account_id == "account-free"
    );
}

#[tokio::test]
async fn account_manager_without_accounts_shows_login_actions() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.show_account_manager(Vec::new());

    assert_chatwidget_snapshot!(
        "account_manager_without_accounts",
        render_bottom_popup(&chat, /*width*/ 100)
    );
}

#[tokio::test]
async fn remove_account_picker_is_snapshot_covered() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.show_remove_account_picker(vec![
        account("account-pro", "pro@example.com", PlanType::Pro, true),
        account("account-free", "free@example.com", PlanType::Free, false),
    ]);

    assert_chatwidget_snapshot!(
        "remove_account_picker",
        render_bottom_popup(&chat, /*width*/ 100)
    );
}

#[tokio::test]
async fn remove_account_confirmation_is_snapshot_covered() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.show_remove_account_confirmation(account(
        "account-pro",
        "pro@example.com",
        PlanType::Pro,
        true,
    ));

    assert_chatwidget_snapshot!(
        "remove_account_confirmation",
        render_bottom_popup(&chat, /*width*/ 100)
    );
}

#[tokio::test]
async fn account_model_fallback_is_snapshot_covered() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.show_account_model_fallback(
        account("account-free", "free@example.com", PlanType::Free, false),
        vec![
            model("gpt-5.4-mini", "GPT-5.4 Mini"),
            model("gpt-5.3-codex", "GPT-5.3 Codex"),
        ],
        "gpt-5.5".to_string(),
    );

    assert_chatwidget_snapshot!(
        "account_model_fallback",
        render_bottom_popup(&chat, /*width*/ 100)
    );
}
