//! Pause eligibility follows the owning tree's work rather than foreground readiness.

use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_features::Feature;
use codex_protocol::AgentPath;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecutionControlState;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::InternalSessionSource;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use std::time::Duration;

fn input(text: &str) -> TurnInputRequest {
    TurnInputRequest::user_input(vec![UserInput::Text {
        text: text.to_owned(),
        text_elements: Vec::new(),
    }])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_pause_and_unpause_do_not_activate_storage_or_emit_transitions() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = test_codex().build_with_auto_env(&server).await?;
    let running = test.codex.execution_state().await?;
    for _ in 0..2 {
        assert!(!test.codex.can_pause_execution().await?);
        assert_eq!(test.codex.pause_execution().await?, running);
        assert_eq!(test.codex.unpause_execution().await?, running);
    }
    assert!(!test.home.path().join("execution-control").exists());
    assert!(!test.home.path().join("workflow-runs").exists());
    assert!(
        tokio::time::timeout(
            Duration::from_millis(150),
            wait_for_event(&test.codex, |event| {
                matches!(event, EventMsg::ExecutionControlChanged(_))
            })
        )
        .await
        .is_err()
    );
    assert!(
        responses::received_responses_requests(&server)
            .await
            .is_empty()
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_mail_makes_an_idle_conversation_pause_eligible_without_inference() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = test_codex()
        .with_config(|config| {
            config
                .features
                .disable(Feature::WakeOnAgentMessages)
                .expect("queue-only delivery");
        })
        .build_with_auto_env(&server)
        .await?;
    assert!(!test.codex.can_pause_execution().await?);
    test.codex
        .submit(Op::InterAgentCommunication {
            communication: InterAgentCommunication::new(
                AgentPath::root().join("worker").expect("worker path"),
                AgentPath::root(),
                Vec::new(),
                "Retained worker report.".to_owned(),
                /*trigger_turn*/ false,
            ),
            start_options: Default::default(),
        })
        .await?;
    test.codex
        .update_thread_settings(ThreadSettingsOverrides::default())
        .await?;
    assert!(test.codex.can_pause_execution().await?);
    assert!(
        responses::received_responses_requests(&server)
            .await
            .is_empty()
    );
    assert_eq!(
        test.codex.pause_execution().await?.state,
        ExecutionControlState::Paused
    );
    assert!(!test.codex.can_pause_execution().await?);
    test.codex.unpause_execution().await?;
    assert!(
        test.codex.can_pause_execution().await?,
        "unread work still belongs to the conversation"
    );
    assert!(
        responses::received_responses_requests(&server)
            .await
            .is_empty()
    );
    let model = responses::mount_sse_once(&server, responses::sse_completed("read-report")).await;
    test.submit_text_turn("Read the report.").await?;
    assert!(
        model
            .single_request()
            .body_contains_text("Retained worker report.")
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while test.codex.can_pause_execution().await? {
            wait_for_event(&test.codex, |event| {
                matches!(event, EventMsg::ExecutionControlChanged(_))
            })
            .await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreground_idle_with_a_running_child_is_eligible_but_other_roots_are_not() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let (release, gate) = tokio::sync::oneshot::channel();
    let (streaming, _) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("child-open")]),
            },
            StreamingSseChunk {
                gate: Some(gate),
                body: responses::sse(vec![responses::ev_completed("child-open")]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse_completed("child-resumed"),
        }],
    ])
    .await;
    let server = responses::start_mock_server().await;
    let url = format!("{}/v1", streaming.uri());
    let test = test_codex()
        .with_config(move |config| config.model_provider.base_url = Some(url))
        .build_with_auto_env(&server)
        .await?;
    let other = test
        .thread_manager
        .start_thread(test.start_thread_options().await)
        .await?
        .thread;
    let mut options = test.start_thread_options().await;
    options.session_source = Some(SessionSource::Internal(InternalSessionSource::Guardian));
    let child = test
        .thread_manager
        .spawn_internal_session(test.session_configured.thread_id, options)
        .await?
        .thread;
    assert!(!test.codex.can_pause_execution().await?);
    child
        .start_or_steer_turn(input("Keep this child active.").from_agent())
        .await?;
    tokio::time::timeout(
        Duration::from_secs(10),
        streaming.wait_for_request_count(/*count*/ 1),
    )
    .await?;
    assert!(test.codex.can_pause_execution().await?);
    assert!(child.can_pause_execution().await?);
    assert!(!other.can_pause_execution().await?);
    assert_eq!(
        test.codex.pause_execution().await?.state,
        ExecutionControlState::Paused
    );
    assert!(!test.codex.can_pause_execution().await?);
    assert!(!child.can_pause_execution().await?);
    assert_eq!(
        other.execution_state().await?.state,
        ExecutionControlState::Running
    );
    test.codex.unpause_execution().await?;
    wait_for_event(&child, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while test.codex.can_pause_execution().await? {
            wait_for_event(&test.codex, |event| {
                matches!(event, EventMsg::ExecutionControlChanged(_))
            })
            .await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    assert_eq!(streaming.requests().await.len(), 2);
    let _ = release.send(());
    other.shutdown_and_wait().await?;
    test.codex.shutdown_and_wait().await?;
    streaming.shutdown().await;
    Ok(())
}
