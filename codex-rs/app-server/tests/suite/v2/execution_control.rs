//! Conversation inference controls remain available with dynamic workflows disabled.

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadExecutionControl;
use codex_app_server_protocol::ThreadExecutionControlChangedNotification;
use codex_app_server_protocol::ThreadExecutionControlReadResponse;
use codex_app_server_protocol::ThreadPauseResponse;
use codex_app_server_protocol::ThreadQueueAddResponse;
use codex_app_server_protocol::ThreadQueueDeleteResponse;
use codex_app_server_protocol::ThreadQueueListResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadUnpauseResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_features::Feature;
use codex_protocol::protocol::ExecutionControlState;
use core_test_support::responses;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(/*secs*/ 30);

#[tokio::test]
async fn execution_control_pause_read_unpause_and_human_message_resume() -> Result<()> {
    let home = TempDir::new()?;
    let (first_release, first_gate) = tokio::sync::oneshot::channel();
    let (second_release, second_gate) = tokio::sync::oneshot::channel();
    let (server, _) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("first")]),
            },
            StreamingSseChunk {
                gate: Some(first_gate),
                body: responses::sse(vec![responses::ev_completed("first")]),
            },
        ],
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("second")]),
            },
            StreamingSseChunk {
                gate: Some(second_gate),
                body: responses::sse(vec![responses::ev_completed("second")]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_response_created("resumed"),
                responses::ev_assistant_message("answer", "Resumed by the human message"),
                responses::ev_completed("resumed"),
            ]),
        }],
    ])
    .await;
    MockResponsesConfig::new(server.uri())
        .disable_feature(Feature::DynamicWorkflows)
        .write(home.path())?;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .without_managed_config()
        .build_initialized()
        .await?;
    let thread_id = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let request=app.send_request("turn/start",Some(json!({"threadId":thread_id,"input":[{"type":"text","text":"Start real work before pausing"}]}))).await?;
    let _: TurnStartResponse = timeout(WAIT, app.read_response(request)).await??;
    timeout(WAIT, server.wait_for_request_count(/*count*/ 1)).await?;
    let running = ThreadExecutionControl {
        root_thread_id: thread_id.clone(),
        state: ExecutionControlState::Running,
        can_pause: true,
    };
    let paused = ThreadExecutionControl {
        root_thread_id: thread_id.clone(),
        state: ExecutionControlState::Paused,
        can_pause: false,
    };
    loop {
        let activity: ThreadExecutionControlChangedNotification = timeout(
            WAIT,
            app.read_notification("thread/executionControl/changed"),
        )
        .await??;
        if activity.execution_control.can_pause {
            assert_eq!(activity.execution_control, running);
            break;
        }
    }
    let request = app
        .send_request("thread/pause", Some(json!({"threadId": thread_id})))
        .await?;
    let response: ThreadPauseResponse = timeout(WAIT, app.read_response(request)).await??;
    assert_eq!(response.execution_control, paused);
    loop {
        let notification: ThreadExecutionControlChangedNotification = timeout(
            WAIT,
            app.read_notification("thread/executionControl/changed"),
        )
        .await??;
        assert_eq!(notification.thread_id, thread_id);
        assert_eq!(notification.execution_control.root_thread_id, thread_id);
        if notification.execution_control.state == ExecutionControlState::Paused {
            break;
        }
    }
    let request = app
        .send_request(
            "thread/executionControl/read",
            Some(json!({"threadId": thread_id})),
        )
        .await?;
    let response: ThreadExecutionControlReadResponse =
        timeout(WAIT, app.read_response(request)).await??;
    assert_eq!(response.execution_control, paused);
    let request = app
        .send_request(
            "thread/queue/add",
            Some(json!({
                "threadId": thread_id,
                "clientUserMessageId": "paused-queued-message",
                "input": [{"type":"text","text":"Keep this queued while paused"}],
            })),
        )
        .await?;
    let queued: ThreadQueueAddResponse = timeout(WAIT, app.read_response(request)).await??;
    let request = app
        .send_request(
            "thread/queue/start",
            Some(json!({"threadId":thread_id,"queuedSubmissionId":queued.queued_submission.id})),
        )
        .await?;
    let error = timeout(
        WAIT,
        app.read_stream_until_error_message(RequestId::Integer(request)),
    )
    .await??;
    assert_eq!(error.error.code, -32600);
    assert_eq!(
        error.error.data,
        Some(json!({"reason":"conversationPaused"}))
    );
    let request = app
        .send_request("thread/queue/list", Some(json!({"threadId":thread_id})))
        .await?;
    let queue: ThreadQueueListResponse = timeout(WAIT, app.read_response(request)).await??;
    assert_eq!(queue.data, vec![queued.queued_submission.clone()]);
    let request = app
        .send_request(
            "thread/queue/delete",
            Some(json!({"threadId":thread_id,"queuedSubmissionId":queued.queued_submission.id})),
        )
        .await?;
    let deleted: ThreadQueueDeleteResponse = timeout(WAIT, app.read_response(request)).await??;
    assert!(deleted.deleted);
    let request = app
        .send_request("thread/unpause", Some(json!({"threadId": thread_id})))
        .await?;
    let response: ThreadUnpauseResponse = timeout(WAIT, app.read_response(request)).await??;
    assert_eq!(response.execution_control, running);
    timeout(WAIT, server.wait_for_request_count(/*count*/ 2)).await?;
    let request = app
        .send_request("thread/pause", Some(json!({"threadId": thread_id})))
        .await?;
    let _: ThreadPauseResponse = timeout(WAIT, app.read_response(request)).await??;
    let request = app
        .send_request(
            "turn/start",
            Some(json!({
                "threadId": thread_id,
                "input": [{"type": "text", "text": "Continue", "text_elements": []}],
            })),
        )
        .await?;
    let _: TurnStartResponse = timeout(WAIT, app.read_response(request)).await??;
    let completion: TurnCompletedNotification =
        timeout(WAIT, app.read_notification("turn/completed")).await??;
    assert_eq!(completion.turn.status, TurnStatus::Completed);
    // Completion is published before task cleanup releases the final activity
    // lease. Observe authoritative settlement before reading idle eligibility.
    loop {
        let activity: ThreadExecutionControlChangedNotification = timeout(
            WAIT,
            app.read_notification("thread/executionControl/changed"),
        )
        .await??;
        if activity.thread_id == thread_id
            && activity.execution_control.state == ExecutionControlState::Running
            && !activity.execution_control.can_pause
        {
            break;
        }
    }
    let request = app
        .send_request(
            "thread/executionControl/read",
            Some(json!({"threadId": thread_id})),
        )
        .await?;
    let response: ThreadExecutionControlReadResponse =
        timeout(WAIT, app.read_response(request)).await??;
    assert_eq!(
        response.execution_control,
        ThreadExecutionControl {
            can_pause: false,
            ..running
        }
    );
    drop(app);
    drop(first_release);
    drop(second_release);
    server.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn execution_control_idle_actions_are_noops_and_missing_capability_defaults_false()
-> Result<()> {
    let home = TempDir::new()?;
    let server = responses::start_mock_server().await;
    MockResponsesConfig::new(&server.uri())
        .disable_feature(Feature::DynamicWorkflows)
        .write(home.path())?;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .without_managed_config()
        .build_initialized()
        .await?;
    let thread_id = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let expected = ThreadExecutionControl {
        root_thread_id: thread_id.clone(),
        state: ExecutionControlState::Running,
        can_pause: false,
    };
    let request = app
        .send_request(
            "thread/executionControl/read",
            Some(json!({"threadId":thread_id})),
        )
        .await?;
    let read: ThreadExecutionControlReadResponse =
        timeout(WAIT, app.read_response(request)).await??;
    assert_eq!(read.execution_control, expected);
    let initial: ThreadExecutionControlChangedNotification = timeout(
        WAIT,
        app.read_notification("thread/executionControl/changed"),
    )
    .await??;
    assert_eq!(initial.execution_control, expected);
    let request = app
        .send_request("thread/pause", Some(json!({"threadId":thread_id})))
        .await?;
    let paused: ThreadPauseResponse = timeout(WAIT, app.read_response(request)).await??;
    assert_eq!(paused.execution_control, expected);
    let request = app
        .send_request("thread/unpause", Some(json!({"threadId":thread_id})))
        .await?;
    let unpaused: ThreadUnpauseResponse = timeout(WAIT, app.read_response(request)).await??;
    assert_eq!(unpaused.execution_control, expected);
    assert!(
        timeout(
            Duration::from_millis(/*millis*/ 150),
            app.read_notification::<ThreadExecutionControlChangedNotification>(
                "thread/executionControl/changed"
            )
        )
        .await
        .is_err(),
        "idle no-op actions must not announce a false transition"
    );
    let older: ThreadExecutionControl =
        serde_json::from_value(json!({"rootThreadId":thread_id,"state":"running"}))?;
    assert_eq!(older, expected);
    assert_eq!(serde_json::to_value(&older)?["canPause"], json!(false));
    Ok(())
}
