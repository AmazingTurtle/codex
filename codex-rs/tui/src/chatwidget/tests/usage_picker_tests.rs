use super::*;
use crate::chatwidget::UsagePickerEvent;
use crate::chatwidget::UsageReadPurpose;
use codex_app_server_protocol::AccountRateLimitsReadManyResponse;
use codex_app_server_protocol::AccountRateLimitsReadResult;
use codex_app_server_protocol::AccountRateLimitsSnapshot;
use codex_app_server_protocol::ChatgptAccountSummary;
use codex_app_server_protocol::RateLimitResetCreditsSummary;
use pretty_assertions::assert_eq;

fn account(id: &str) -> ChatgptAccountSummary {
    ChatgptAccountSummary {
        account_id: id.to_string(),
        email: Some(format!("{id}@example.com")),
        plan_type: PlanType::Pro,
        is_active: id == "active",
        is_eligible: true,
    }
}

fn availability(id: &str, count: i64) -> Result<AccountRateLimitsReadManyResponse, String> {
    Ok(AccountRateLimitsReadManyResponse {
        data: vec![AccountRateLimitsReadResult {
            account: account(id),
            error: None,
            rate_limits: Some(AccountRateLimitsSnapshot {
                rate_limits: RateLimitSnapshot {
                    limit_id: None,
                    limit_name: None,
                    primary: None,
                    secondary: None,
                    credits: None,
                    individual_limit: None,
                    spend_control_reached: None,
                    plan_type: None,
                    rate_limit_reached_type: None,
                    normal_model_slug: None,
                },
                rate_limits_by_limit_id: None,
                rate_limit_reset_credits: Some(RateLimitResetCreditsSummary {
                    available_count: count,
                    credits: None,
                }),
                ordinary_usage_allowed: None,
            }),
        }],
    })
}

fn key(chat: &mut ChatWidget, code: KeyCode) {
    chat.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
}

fn load_root(chat: &mut ChatWidget, rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>) {
    let Ok(AppEvent::UsagePicker(UsagePickerEvent::LoadAccounts { request_id })) = rx.try_recv()
    else {
        panic!("expected account list request");
    };
    chat.finish_usage_accounts(request_id, Ok(vec![account("active"), account("other")]));
}

fn read_request(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> (u64, String, UsageReadPurpose) {
    let Ok(AppEvent::UsagePicker(UsagePickerEvent::ReadAccount {
        request_id,
        account_id,
        purpose,
    })) = rx.try_recv()
    else {
        panic!("expected account read");
    };
    (request_id, account_id, purpose)
}

#[tokio::test]
async fn usage_picker_bare_command_and_reset_alias_open_root() {
    for command in ["/usage", "/usage reset", "/usage   "] {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
        set_chatgpt_auth(&mut chat);
        chat.bottom_pane
            .set_composer_text(command.to_string(), Vec::new(), Vec::new());
        key(&mut chat, KeyCode::Esc);
        key(&mut chat, KeyCode::Enter);
        assert!(render_bottom_popup(&chat, /*width*/ 80).contains("Loading accounts"));
        assert_matches!(rx.try_recv(), Ok(AppEvent::FollowTranscript));
        load_root(&mut chat, &mut rx);
        assert_chatwidget_snapshot!(
            "usage_picker_root",
            render_bottom_popup(&chat, /*width*/ 80)
        );
        key(&mut chat, KeyCode::Enter);
        assert_matches!(
            rx.try_recv(),
            Ok(AppEvent::UsagePicker(UsagePickerEvent::ShowUsage {
                view: TokenActivityView::Cumulative,
                account_id: None
            }))
        );
    }
}

#[tokio::test]
async fn usage_picker_account_states_and_time_ranges_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.dispatch_command(SlashCommand::Usage);
    load_root(&mut chat, &mut rx);
    // Choose the non-active account through keyboard navigation.
    key(&mut chat, KeyCode::Down);
    key(&mut chat, KeyCode::Down);
    key(&mut chat, KeyCode::Enter);
    let Ok(AppEvent::UsagePicker(UsagePickerEvent::OpenAccount(selected))) = rx.try_recv() else {
        panic!("expected account selection");
    };
    assert_eq!(selected, account("other"));
    chat.open_usage_account(selected);
    let mut states = vec![render_bottom_popup(&chat, /*width*/ 80)];
    let (request_id, account_id, purpose) = read_request(&mut rx);
    chat.finish_usage_account_read(
        request_id,
        account_id,
        purpose,
        availability("other", /*count*/ 0),
    );
    states.push(render_bottom_popup(&chat, /*width*/ 80));
    key(&mut chat, KeyCode::Down);
    // The disabled reset entry is skipped: Back returns to the same root row.
    key(&mut chat, KeyCode::Enter);
    assert_eq!(
        chat.bottom_pane
            .selected_index_for_active_view("usage-menu"),
        Some(2)
    );
    chat.open_usage_account(account("other"));
    let (request_id, account_id, purpose) = read_request(&mut rx);
    chat.finish_usage_account_read(
        request_id,
        account_id,
        purpose,
        Err("unavailable".to_string()),
    );
    states.push(render_bottom_popup(&chat, /*width*/ 80));
    chat.refresh_usage_account();
    let (request_id, account_id, purpose) = read_request(&mut rx);
    chat.finish_usage_account_read(
        request_id,
        account_id,
        purpose,
        availability("other", /*count*/ 3),
    );
    states.push(render_bottom_popup(&chat, /*width*/ 80));
    chat.show_usage_ranges();
    states.push(render_bottom_popup(&chat, /*width*/ 80));
    assert_chatwidget_snapshot!("usage_picker_account_states", states.join("\n---\n"));
    for view in [
        TokenActivityView::Daily,
        TokenActivityView::Weekly,
        TokenActivityView::Cumulative,
    ] {
        key(&mut chat, KeyCode::Enter);
        let Ok(AppEvent::UsagePicker(UsagePickerEvent::ShowUsage {
            view: actual,
            account_id,
        })) = rx.try_recv()
        else {
            panic!("expected usage selection");
        };
        assert_eq!((actual, account_id), (view, Some("other".to_string())));
        key(&mut chat, KeyCode::Down);
    }
    key(&mut chat, KeyCode::Esc);
    assert!(render_bottom_popup(&chat, /*width*/ 80).contains("Show usage for this account"));
    key(&mut chat, KeyCode::Esc);
    assert_eq!(
        chat.bottom_pane
            .selected_index_for_active_view("usage-menu"),
        Some(2)
    );
    key(&mut chat, KeyCode::Esc);
    assert!(chat.bottom_pane.no_modal_or_popup_active());
}

#[tokio::test]
async fn usage_picker_ignores_late_reads_after_navigation_and_account_changes() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.dispatch_command(SlashCommand::Usage);
    load_root(&mut chat, &mut rx);
    chat.open_usage_account(account("other"));
    let (old_request, old_account, old_purpose) = read_request(&mut rx);
    key(&mut chat, KeyCode::Esc);
    chat.open_usage_account(account("active"));
    let _ = read_request(&mut rx);
    chat.finish_usage_account_read(
        old_request,
        old_account,
        old_purpose,
        availability("other", /*count*/ 5),
    );
    assert!(render_bottom_popup(&chat, /*width*/ 80).contains("loading…"));
    chat.clear_pending_rate_limit_reset_requests();
    assert!(chat.bottom_pane.no_modal_or_popup_active());
}

#[tokio::test]
async fn usage_picker_reset_confirmation_and_retry_keep_selected_account() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.dispatch_command(SlashCommand::Usage);
    load_root(&mut chat, &mut rx);
    chat.open_usage_account(account("other"));
    let (request_id, account_id, purpose) = read_request(&mut rx);
    chat.finish_usage_account_read(
        request_id,
        account_id,
        purpose,
        availability("other", /*count*/ 2),
    );
    chat.available_rate_limit_reset_credits = Some(7);
    let failed_request = chat.show_rate_limit_reset_loading_popup();
    chat.finish_usage_account_read(
        failed_request,
        "other".to_string(),
        UsageReadPurpose::ResetPicker,
        Err("credits unavailable".to_string()),
    );
    key(&mut chat, KeyCode::Enter);
    let Ok(AppEvent::OpenRateLimitResetCredits { account_id }) = rx.try_recv() else {
        panic!("expected targeted retry");
    };
    assert_eq!(account_id, Some("other".to_string()));
    let request_id = chat.show_rate_limit_reset_loading_popup();
    chat.finish_usage_account_read(
        request_id,
        "other".to_string(),
        UsageReadPurpose::ResetPicker,
        availability("other", /*count*/ 2),
    );
    assert_eq!(chat.available_rate_limit_reset_credits, Some(7));
    assert_chatwidget_snapshot!(
        "usage_picker_resets_narrow",
        render_bottom_popup(&chat, /*width*/ 45)
    );
    key(&mut chat, KeyCode::Enter);
    let Ok(AppEvent::OpenRateLimitResetConfirmation {
        picker_request_id,
        confirmation_gate,
        credit_id,
        reset_title,
        reset_detail,
        reset_description,
    }) = rx.try_recv()
    else {
        panic!("expected confirmation");
    };
    assert!(chat.show_rate_limit_reset_confirmation(
        picker_request_id,
        confirmation_gate,
        credit_id,
        reset_title,
        reset_detail,
        reset_description
    ));
    assert_chatwidget_snapshot!(
        "usage_picker_account_confirmation",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    key(&mut chat, KeyCode::Up);
    key(&mut chat, KeyCode::Enter);
    let Ok(AppEvent::ConsumeRateLimitResetCredit {
        account_id,
        idempotency_key,
        credit_id,
    }) = rx.try_recv()
    else {
        panic!("expected consumption");
    };
    let request_id = chat
        .start_rate_limit_reset_consumption(&idempotency_key)
        .expect("valid attempt");
    chat.finish_rate_limit_reset_consume(
        request_id,
        idempotency_key.clone(),
        credit_id.clone(),
        Err("timeout".to_string()),
    );
    key(&mut chat, KeyCode::Enter);
    let Ok(AppEvent::ConsumeRateLimitResetCredit {
        account_id: retry_account,
        idempotency_key: retry_key,
        credit_id: retry_credit,
    }) = rx.try_recv()
    else {
        panic!("expected retry");
    };
    assert_eq!(
        (retry_account, retry_key, retry_credit),
        (account_id, idempotency_key, credit_id)
    );
    assert_eq!(chat.usage_reset_account_id(), Some("other".to_string()));
}

#[tokio::test]
async fn usage_picker_back_from_reset_refreshes_account_and_ignores_dismissed_reads() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.dispatch_command(SlashCommand::Usage);
    load_root(&mut chat, &mut rx);
    chat.open_usage_account(account("other"));
    let (request_id, account_id, purpose) = read_request(&mut rx);
    chat.finish_usage_account_read(
        request_id,
        account_id,
        purpose,
        availability("other", /*count*/ 1),
    );
    key(&mut chat, KeyCode::Down);
    let reset_request = chat.show_rate_limit_reset_loading_popup();
    key(&mut chat, KeyCode::Esc);
    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::UsagePicker(UsagePickerEvent::RefreshAccount))
    );
    chat.refresh_usage_account();
    let (request_id, account_id, purpose) = read_request(&mut rx);
    assert!(!chat.finish_usage_account_read(
        reset_request,
        "other".to_string(),
        UsageReadPurpose::ResetPicker,
        availability("other", /*count*/ 1)
    ));
    chat.finish_usage_account_read(
        request_id,
        account_id,
        purpose,
        availability("other", /*count*/ 1),
    );
    assert_eq!(
        chat.bottom_pane
            .selected_index_for_active_view("usage-account"),
        Some(1)
    );
    key(&mut chat, KeyCode::Esc);
    key(&mut chat, KeyCode::Esc);
    chat.dispatch_command(SlashCommand::Usage);
    let Ok(AppEvent::UsagePicker(UsagePickerEvent::LoadAccounts { request_id })) = rx.try_recv()
    else {
        panic!("expected account load");
    };
    key(&mut chat, KeyCode::Esc);
    chat.finish_usage_accounts(request_id, Ok(vec![account("active")]));
    assert!(chat.bottom_pane.no_modal_or_popup_active());
}

#[tokio::test]
async fn usage_picker_empty_accounts_and_failed_list_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    let mut states = Vec::new();
    for result in [Ok(Vec::new()), Err("service unavailable".to_string())] {
        chat.dispatch_command(SlashCommand::Usage);
        let Ok(AppEvent::UsagePicker(UsagePickerEvent::LoadAccounts { request_id })) =
            rx.try_recv()
        else {
            panic!("expected account load");
        };
        chat.finish_usage_accounts(request_id, result);
        states.push(render_bottom_popup(&chat, /*width*/ 80));
        key(&mut chat, KeyCode::Esc);
    }
    assert_chatwidget_snapshot!("usage_picker_empty_and_error", states.join("\n---\n"));
}

#[tokio::test]
async fn explicit_usage_commands_select_accounts() {
    for (args, view, selector) in [
        ("daily other", TokenActivityView::Daily, "other"),
        ("weekly other", TokenActivityView::Weekly, "other"),
        ("other", TokenActivityView::Cumulative, "other"),
    ] {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
        set_chatgpt_auth(&mut chat);
        chat.dispatch_command_with_args(SlashCommand::Usage, args.to_string(), Vec::new());
        let Ok(AppEvent::ShowChatgptAccountUsage {
            view: actual_view,
            selector: actual_selector,
        }) = rx.try_recv()
        else {
            panic!("expected account usage request");
        };
        assert_eq!(
            (actual_view, actual_selector.as_deref()),
            (view, Some(selector))
        );
    }
}

#[tokio::test]
async fn usage_picker_account_list_updates_beneath_an_overlay() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.dispatch_command(SlashCommand::Usage);
    chat.bottom_pane.show_selection_view(SelectionViewParams {
        title: Some("Overlay".to_string()),
        items: vec![SelectionItem {
            name: "Close".to_string(),
            dismiss_on_select: true,
            ..Default::default()
        }],
        ..Default::default()
    });
    load_root(&mut chat, &mut rx);
    assert!(render_bottom_popup(&chat, /*width*/ 80).contains("Overlay"));
    key(&mut chat, KeyCode::Esc);
    assert_chatwidget_snapshot!(
        "usage_picker_root",
        render_bottom_popup(&chat, /*width*/ 80)
    );
}
