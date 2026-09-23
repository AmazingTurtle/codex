use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_sandbox;
use core_test_support::skip_if_target_windows;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use test_case::test_case;
use tokio::time::Duration;

async fn submit_exec_turn(test: &TestCodex, mode: ModeKind) -> Result<()> {
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, test.config.cwd.as_path());
    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "run the background command".to_string(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                approval_policy: Some(AskForApproval::Never),
                sandbox_policy: Some(sandbox_policy),
                permission_profile,
                collaboration_mode: Some(CollaborationMode {
                    mode,
                    settings: Settings {
                        model: test.session_configured.model.clone(),
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..Default::default()
            }),
        )
        .await?;
    Ok(())
}

#[test_case(ModeKind::Default, "sleep 1; printf IDLE_DONE", 0; "default_mode")]
#[test_case(ModeKind::Plan, "sleep 1; printf IDLE_DONE", 0; "plan_mode")]
#[test_case(ModeKind::Default, "sleep 1; printf IDLE_DONE; exit 7", 7; "failed_command")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_background_exec_wakes_idle_turn(
    mode: ModeKind,
    command: &str,
    exit_code: i32,
) -> Result<()> {
    skip_if_target_windows!(Ok(()), "uses a POSIX-only command fixture");
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;
    let call_id = "background-idle";
    let args = json!({"cmd": command, "yield_time_ms": 250});
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(call_id, "exec_command", &serde_json::to_string(&args)?),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "waiting for completion"),
                ev_completed("resp-2"),
            ]),
            sse(vec![
                ev_response_created("resp-3"),
                ev_assistant_message("msg-2", "saw completion"),
                ev_completed("resp-3"),
            ]),
        ],
    )
    .await;

    submit_exec_turn(&test, mode).await?;
    for _ in 0..2 {
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;
    }

    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    let output = mock
        .function_call_output_text(call_id)
        .expect("exec output");
    assert!(output.contains("Process running with session ID"));
    assert!(!requests[1].body_contains_text("<exec-command-completed"));
    assert!(requests[2].body_contains_text("<exec-command-completed"));
    assert!(requests[2].body_contains_text("IDLE_DONE"));
    assert!(requests[2].body_contains_text(&format!("exit-code=\"{exit_code}\"")));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_background_exec_reaches_new_user_turn() -> Result<()> {
    skip_if_target_windows!(Ok(()), "uses POSIX-only command fixtures");
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;
    let background = json!({"cmd": "sleep 2; printf ACTIVE_DONE", "yield_time_ms": 250});
    let foreground = json!({"cmd": "sleep 3", "yield_time_ms": 4_000});
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(
                    "background-active",
                    "exec_command",
                    &serde_json::to_string(&background)?,
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "waiting"),
                ev_completed("resp-2"),
            ]),
            sse(vec![
                ev_response_created("resp-3"),
                ev_function_call(
                    "foreground",
                    "exec_command",
                    &serde_json::to_string(&foreground)?,
                ),
                ev_completed("resp-3"),
            ]),
            sse(vec![
                ev_response_created("resp-4"),
                ev_assistant_message("msg-2", "done"),
                ev_completed("resp-4"),
            ]),
        ],
    )
    .await;

    submit_exec_turn(&test, ModeKind::Default).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    submit_exec_turn(&test, ModeKind::Default).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    assert!(requests[3].body_contains_text("<exec-command-completed"));
    assert!(requests[3].body_contains_text("ACTIVE_DONE"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inline_exec_result_does_not_push_completion() -> Result<()> {
    skip_if_target_windows!(Ok(()), "uses a POSIX-only command fixture");
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;
    let args = json!({"cmd": "printf INLINE_DONE", "yield_time_ms": 1_000});
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call("inline", "exec_command", &serde_json::to_string(&args)?),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    submit_exec_turn(&test, ModeKind::Default).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(500),
            wait_for_event(&test.codex, |event| {
                matches!(event, EventMsg::TurnComplete(_))
            })
        )
        .await
        .is_err()
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(!requests[1].body_contains_text("<exec-command-completed"));
    Ok(())
}
