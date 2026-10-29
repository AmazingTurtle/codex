//! Model tools use the same guarded revision service without echoing edited source.

use anyhow::Context;
use anyhow::Result;
use codex_protocol::protocol::EventMsg;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;

use super::workflow_revision::builder;
use super::workflow_revision::observe;
use super::workflow_revision::source;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_reads_current_controls_and_stages_a_revision_through_the_original_caller()
-> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let provider = responses::start_mock_server().await;
    let test = builder().build_with_auto_env(&provider).await?;
    let original_source = source("return {private:'ORIGINAL_RETAINED_RESULT'};");
    let proposed = source("return {private:'PROPOSED_SOURCE_MUST_NOT_ECHO'};");
    let started = test
        .codex
        .start_workflow(original_source.clone(), json!({"kept":true}))
        .await
        .map_err(anyhow::Error::msg)?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let original = observe(&service, &started.id, |run| {
        run.status == codex_core::workflows::WorkflowStatus::Succeeded
            && run.lifecycle.terminal_ready
    })
    .await?;
    let state = test
        .codex
        .workflow_control_state(&started.id)
        .await
        .map_err(anyhow::Error::msg)?;
    let model=responses::mount_sse_sequence(&provider,vec![
        responses::sse(vec![responses::ev_function_call("read-controls","workflow_read",&json!({"run_id":started.id}).to_string()),responses::ev_completed("read-controls")]),
        responses::sse(vec![responses::ev_function_call("stage-revision","workflow_revision",&json!({
            "action":"stage","run_id":started.id,"command_id":"model-stage","expected_control_revision":state.control_revision,"source":proposed,
        }).to_string()),responses::ev_completed("stage-revision")]),
        responses::sse(vec![responses::ev_assistant_message("proposal-ready","The proposed plan is staged for explicit review."),responses::ev_completed("proposal-ready")]),
    ]).await;
    test.codex.start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
        text:"Inspect the current control identity and stage the requested plan without applying it.".into(),text_elements:Vec::new(),
    }])).await?;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        core_test_support::wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        }),
    )
    .await?;
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    let metadata = requests[1].function_call_output("read-controls");
    let metadata_text = metadata["output"]
        .as_str()
        .context("the actual workflow_read output")?;
    assert!(
        metadata_text.contains("control_revision")
            && metadata_text.contains(&state.active_revision_id)
    );
    assert!(
        !metadata_text.contains("ORIGINAL_RETAINED_RESULT"),
        "status reads expose result availability rather than the complete retained body"
    );
    let output = requests[2].function_call_output("stage-revision");
    let output_text = output["output"]
        .as_str()
        .context("the actual staged-revision tool receipt")?;
    assert!(output_text.contains("draft") && output_text.contains("revision"));
    assert!(!output_text.contains("PROPOSED_SOURCE_MUST_NOT_ECHO"));
    let revisions = test
        .codex
        .workflow_revisions(&started.id, /*offset*/ 0, /*limit*/ 50)
        .await
        .map_err(anyhow::Error::msg)?;
    assert!(revisions.iter().any(|revision| revision.source == proposed
        && revision.status == codex_dynamic_workflows::WorkflowRevisionStatus::Draft
        && revision.input == json!({"kept":true})));
    let retained = service.get(&started.id).map_err(anyhow::Error::msg)?;
    assert_eq!(
        (
            retained.execution_id,
            retained.source,
            retained.result,
            retained.accepted_starts
        ),
        (
            original.execution_id,
            original_source,
            Some(json!({"private":"ORIGINAL_RETAINED_RESULT"})),
            0
        )
    );
    let body = requests[0].body_json();
    let mut specs = body["tools"].as_array().cloned().unwrap_or_default();
    for item in requests[0].inputs_of_type("additional_tools") {
        specs.extend(
            item["tools"]
                .as_array()
                .context("the actual Responses Lite additional-tools catalogue")?
                .iter()
                .cloned(),
        );
    }
    assert!(
        !specs.is_empty(),
        "the actual request must advertise a Responses or Responses Lite catalogue"
    );
    for name in ["workflow_control", "workflow_revision"] {
        let spec = specs
            .iter()
            .find_map(|spec| {
                if spec["name"] == name {
                    Some(spec)
                } else {
                    spec["tools"]
                        .as_array()
                        .and_then(|children| children.iter().find(|child| child["name"] == name))
                }
            })
            .with_context(|| format!("the real model must be offered {name}"))?;
        let serialized = serde_json::to_string(spec)?;
        assert!(
            serialized.len() <= 8000,
            "the complete serialized {name} definition exceeds its hard bound"
        );
        eprintln!(
            "Actual model-visible {name} tool specification ({} bytes): {serialized}",
            serialized.len()
        );
    }
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
