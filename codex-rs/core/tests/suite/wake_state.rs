use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_core::WakeState;
use codex_features::Feature;
use codex_features::SleepToolMode;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::*;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;
use test_case::test_case;

#[derive(Clone, Copy)]
enum Resolution {
    UserInput,
    Timeout,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_mode_wake_state_counts_only_an_observed_cell_and_clears_on_abort() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let test = test_codex()
        .with_config(|config| {
            config
                .features
                .enable(Feature::CodeMode)
                .expect("enable code mode");
            config
                .features
                .enable(Feature::CodeModeInterrupt)
                .expect("enable code mode interruption");
        })
        .build_with_auto_env(&server)
        .await?;
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("r1"),
                ev_custom_tool_call(
                    "cell",
                    "exec",
                    "yield_control(); await new Promise(() => {});",
                ),
                ev_completed("r1"),
            ]),
            sse(vec![
                ev_assistant_message("done", "Yielded"),
                ev_completed("r2"),
            ]),
        ],
    )
    .await;
    test.submit_turn("start a cell").await?;
    assert_eq!(test.codex.wake_state(), WakeState::default());
    let output = mock.requests()[1].custom_tool_call_output("cell");
    let header = output["output"]
        .as_str()
        .or_else(|| output["output"][0]["text"].as_str())
        .unwrap_or_else(|| panic!("yielded cell header: {output}"));
    let cell_id = header
        .strip_prefix("Script running with cell ID ")
        .and_then(|rest| rest.lines().next())
        .expect("cell ID");
    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("r3"),
            ev_function_call(
                "wait",
                "wait",
                &json!({"cell_id": cell_id, "yield_time_ms": 60_000}).to_string(),
            ),
            ev_completed("r3"),
        ]),
    )
    .await;
    let mut updates = test.codex.subscribe_wake_state();
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "await the cell".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let expected = WakeState {
        pending_count: 1,
        waiting: true,
    };
    tokio::time::timeout(
        Duration::from_secs(10),
        updates.wait_for(|state| *state == expected),
    )
    .await??;
    assert_eq!(test.codex.wake_state(), expected);
    test.codex.submit(Op::Interrupt).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    tokio::time::timeout(
        Duration::from_secs(10),
        updates.wait_for(|state| *state == WakeState::default()),
    )
    .await??;
    assert_eq!(test.codex.wake_state(), WakeState::default());
    Ok(())
}

#[test_case("clock", "sleep", Resolution::UserInput; "sleep_steered")]
#[test_case("collaboration", "wait_agent", Resolution::UserInput; "agent_wait_steered")]
#[test_case("collaboration", "wait_agent", Resolution::Timeout; "agent_wait_timeout")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_wait_is_observable_with_a_queued_tool_and_clears_on_resolution(
    namespace: &str,
    tool: &str,
    resolution: Resolution,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let args = if tool == "sleep" {
        json!({"duration_ms": 60_000})
    } else {
        json!({})
    };
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("r1"),
                ev_function_call_with_namespace("wait", namespace, tool, &args.to_string()),
                ev_function_call_with_namespace("queued", "clock", "curr_time", "{}"),
                ev_completed("r1"),
            ]),
            sse(vec![
                ev_response_created("r2"),
                ev_assistant_message("done", "Finished"),
                ev_completed("r2"),
            ]),
        ],
    )
    .await;
    let test = test_codex()
        .with_config(move |config| {
            for feature in [
                Feature::Collab,
                Feature::MultiAgentV2,
                Feature::SleepTool,
                Feature::CurrentTimeReminder,
            ] {
                config.features.enable(feature).expect("enable wait tools");
            }
            config.sleep_tool_mode = SleepToolMode::AlwaysOn;
            config.multi_agent_v2.min_wait_timeout_ms = 1;
            config.multi_agent_v2.default_wait_timeout_ms = match resolution {
                Resolution::UserInput => 60_000,
                Resolution::Timeout => 1_000,
            };
        })
        .build_with_auto_env(&server)
        .await?;
    let mut updates = test.codex.subscribe_wake_state();
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "wait".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let expected = WakeState {
        pending_count: 1,
        waiting: true,
    };
    tokio::time::timeout(
        Duration::from_secs(10),
        updates.wait_for(|state| *state == expected),
    )
    .await??;
    assert_eq!(test.codex.wake_state(), expected);
    if matches!(resolution, Resolution::UserInput) {
        test.codex
            .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
                text: "continue now".into(),
                text_elements: Vec::new(),
            }]))
            .await?;
    }
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(test.codex.wake_state(), WakeState::default());
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .function_call_output_text("queued")
            .expect("queued tool output")
            .contains("UTC")
    );
    let output = requests[1]
        .function_call_output_text("wait")
        .expect("wait output");
    assert!(
        output.contains(match resolution {
            Resolution::UserInput => "interrupted by new input",
            Resolution::Timeout => "timed out",
        }),
        "{output}"
    );
    Ok(())
}
