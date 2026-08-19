use super::*;
use crate::chatwidget::TokenActivityTarget;
use crate::chatwidget::TokenActivityView;
use crate::chatwidget::UsagePickerEvent;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn usage_charts_route_active_shortcuts_and_selected_picker_ranges() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut session = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    for view in [TokenActivityView::Daily, TokenActivityView::Weekly] {
        app.show_account_usage(view, /*selector*/ None, &mut session)
            .await;
        let target = loop {
            if let AppEvent::RefreshTokenActivity { target, .. } = events.try_recv()? {
                break target;
            }
        };
        assert_eq!(target, TokenActivityTarget::Active);
    }
    for view in [
        TokenActivityView::Daily,
        TokenActivityView::Weekly,
        TokenActivityView::Cumulative,
    ] {
        app.handle_usage_picker_event(
            UsagePickerEvent::ShowUsage {
                view,
                account_id: Some("selected-account".to_string()),
            },
            &mut session,
        )
        .await;
        let target = loop {
            if let AppEvent::RefreshTokenActivity { target, .. } = events.try_recv()? {
                break target;
            }
        };
        assert_eq!(
            target,
            TokenActivityTarget::Stored {
                account_id: "selected-account".to_string(),
                label: "selected-account".to_string(),
            }
        );
    }
    let missing = super::super::background_requests::fetch_account_token_activity(
        session.request_handle(),
        TokenActivityTarget::Stored {
            account_id: "missing-account".to_string(),
            label: "missing@example.com".to_string(),
        },
    )
    .await;
    assert!(
        missing.is_err(),
        "missing selected account must never fall back to active usage"
    );
    session.shutdown().await?;
    Ok(())
}
