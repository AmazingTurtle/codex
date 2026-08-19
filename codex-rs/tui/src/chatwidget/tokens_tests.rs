use super::*;
use codex_app_server_protocol::AccountTokenUsageSummary;
use pretty_assertions::assert_eq;

#[test]
fn loaded_state_freezes_chart_anchor_date_at_completion() {
    let state = Arc::new(RwLock::new(TokenActivityState::Loading));
    let handle = TokenActivityHandle {
        state: Arc::clone(&state),
    };
    let today =
        NaiveDate::from_ymd_opt(/*year*/ 2026, /*month*/ 5, /*day*/ 29).expect("valid date");

    handle.finish_with_today(
        Ok(GetAccountTokenUsageResponse {
            summary: AccountTokenUsageSummary {
                lifetime_tokens: None,
                peak_daily_tokens: None,
                longest_running_turn_sec: None,
                current_streak_days: None,
                longest_streak_days: None,
            },
            daily_usage_buckets: None,
            thread_usage: None,
        }),
        today,
    );

    let state = state.read().expect("token activity state poisoned");
    match &*state {
        TokenActivityState::Loaded {
            today: loaded_today,
            ..
        } => {
            assert_eq!(*loaded_today, today);
        }
        other => panic!("expected loaded state, got {other:?}"),
    }
}

#[test]
fn account_chart_cards() {
    let today = NaiveDate::from_ymd_opt(/*year*/ 2026, /*month*/ 5, /*day*/ 29).unwrap();
    let mut rendered = Vec::new();
    for target in [
        TokenActivityTarget::Active,
        TokenActivityTarget::Stored {
            account_id: "other-account".to_string(),
            label: "other@example.com".to_string(),
        },
    ] {
        for view in [
            TokenActivityView::Daily,
            TokenActivityView::Weekly,
            TokenActivityView::Cumulative,
        ] {
            let (cell, handle) = new_token_activity_output(view, target.clone());
            for width in [32, 100] {
                rendered.push(format!("{target:?} {view:?} width={width} loading"));
                rendered.extend(cell.display_lines(width).iter().map(ToString::to_string));
            }
            handle.finish_with_today(
                Ok(GetAccountTokenUsageResponse {
                    summary: AccountTokenUsageSummary {
                        lifetime_tokens: Some(4000),
                        peak_daily_tokens: Some(3000),
                        longest_running_turn_sec: Some(60),
                        current_streak_days: Some(2),
                        longest_streak_days: Some(2),
                    },
                    daily_usage_buckets: Some(vec![
                        codex_app_server_protocol::AccountTokenUsageDailyBucket {
                            start_date: "2026-05-28".to_string(),
                            tokens: 1000,
                        },
                        codex_app_server_protocol::AccountTokenUsageDailyBucket {
                            start_date: "2026-05-29".to_string(),
                            tokens: 3000,
                        },
                    ]),
                    thread_usage: None,
                }),
                today,
            );
            for width in [32, 100] {
                rendered.push(format!("{target:?} {view:?} width={width} loaded"));
                rendered.extend(cell.display_lines(width).iter().map(ToString::to_string));
            }
            handle.finish(Err("account unavailable".to_string()));
            rendered.extend(
                cell.display_lines(/*width*/ 32)
                    .iter()
                    .map(ToString::to_string),
            );
        }
    }
    insta::assert_snapshot!(rendered.join("\n"));
}
