use super::*;
use crate::app::App;
use crate::app::startup::account_limit_poll_is_active;
use crate::app::tests::make_test_app_with_channels;
use chrono::Utc;
use codex_protocol::ThreadId;
use codex_rollout::StateDbHandle;
use pretty_assertions::assert_eq;
use tokio::time::Instant;

const POLL_INTERVAL: chrono::Duration = chrono::Duration::minutes(5);

async fn test_state_db(app: &App) -> anyhow::Result<StateDbHandle> {
    Ok(
        crate::init_state_db_for_app_server_target(&app.config, &crate::AppServerTarget::Embedded)
            .await?
            .expect("embedded TUI tests have a state database"),
    )
}

#[tokio::test]
async fn account_limit_poll_requires_local_active_work() -> anyhow::Result<()> {
    let (mut app, _, _) = make_test_app_with_channels().await;
    app.state_db = Some(test_state_db(&app).await?);
    let thread_id = ThreadId::new();
    app.agent_navigation
        .upsert(thread_id, None, None, /*is_closed*/ false);
    app.agent_navigation.mark_running(thread_id);
    assert!(account_limit_poll_is_active(&app));

    app.reconnect.offline = true;
    assert!(!account_limit_poll_is_active(&app));
    app.reconnect.offline = false;
    app.app_server_target = crate::AppServerTarget::Remote {
        endpoint: crate::RemoteAppServerEndpoint::WebSocket {
            websocket_url: "ws://localhost".to_string(),
            auth_token: None,
        },
    };
    assert!(!account_limit_poll_is_active(&app));
    app.app_server_target = crate::AppServerTarget::Embedded;
    app.state_db = None;
    assert!(!account_limit_poll_is_active(&app));
    Ok(())
}

#[tokio::test]
async fn poller_schedules_once_until_the_next_due_interval() -> anyhow::Result<()> {
    let (app, _, _) = make_test_app_with_channels().await;
    let state = test_state_db(&app).await?;
    let app_server = crate::start_embedded_app_server_for_picker(&app.config)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut poller = AccountLimitPoller::default();

    poller.start_if_due(state.clone(), app_server.request_handle());
    let first_deadline = poller.next_attempt;
    assert!(first_deadline > Instant::now());

    // A second loop iteration before the deadline cannot start another request.
    poller.start_if_due(state.clone(), app_server.request_handle());
    assert_eq!(poller.next_attempt, first_deadline);
    poller
        .pending
        .as_mut()
        .expect("due poll should spawn a task")
        .await
        .expect("poll task should complete");
    assert!(
        !state
            .try_claim_account_limit_poll(Utc::now(), POLL_INTERVAL)
            .await?
    );

    // Once due, the poller advances its deadline again. The shared claim still prevents a
    // duplicate HTTP read in this interval.
    poller.next_attempt = Instant::now();
    poller.start_if_due(state.clone(), app_server.request_handle());
    assert!(poller.next_attempt > Instant::now());
    poller
        .pending
        .as_mut()
        .expect("due poll should spawn a task")
        .await
        .expect("duplicate-prevention task should complete");
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn dropping_poller_aborts_unstarted_request_before_claiming() -> anyhow::Result<()> {
    let (app, _, _) = make_test_app_with_channels().await;
    let state = test_state_db(&app).await?;
    let app_server = crate::start_embedded_app_server_for_picker(&app.config)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    {
        let mut poller = AccountLimitPoller::default();
        poller.start_if_due(state.clone(), app_server.request_handle());
    }

    // No await occurred between spawn and Drop, so the aborted task cannot have claimed the
    // interval. This guards the shutdown path from leaking a background request.
    assert!(
        state
            .try_claim_account_limit_poll(Utc::now(), POLL_INTERVAL)
            .await?
    );
    app_server.shutdown().await?;
    Ok(())
}
