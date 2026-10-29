//! A settled targeted cancellation seals only its exact native usage contributor.

use anyhow::Context;
use anyhow::Result;
use codex_dynamic_workflows::WorkflowControlOperation;
use codex_dynamic_workflows::WorkflowControlRequest;
use codex_dynamic_workflows::WorkflowControlStatus;
use codex_dynamic_workflows::WorkflowControlTarget;
use codex_dynamic_workflows::WorkflowResult;
use codex_dynamic_workflows::WorkflowStepDisposition;
use codex_protocol::ThreadId;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::EventMsg;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::turn_input::TurnInputSubmission;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;
use test_case::test_case;

use super::workflow_revision::builder;
use super::workflow_revision::observe;
use super::workflow_revision::source;

#[derive(Clone, Copy)]
enum StopAction {
    Interrupt,
    Skip,
}

#[test_case(StopAction::Interrupt; "targeted_interrupt_retires_only_the_selected_native_turn")]
#[test_case(StopAction::Skip; "accepted_skip_keeps_native_failure_distinct_from_the_logical_replacement")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn targeted_interrupt_settles_only_the_selected_worker_and_preserves_proven_partial_usage(
    action: StopAction,
) -> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    core_test_support::skip_if_remote!(
        Ok(()),
        "the native printf fixture executes in the locally selected environment"
    );
    core_test_support::skip_if_target_windows!(
        Ok(()),
        "uses POSIX printf for a real native command before targeted cancellation"
    );
    let (release_a, gate_a) = tokio::sync::oneshot::channel();
    let (release_b, gate_b) = tokio::sync::oneshot::channel();
    let (release_human, human_gate) = tokio::sync::oneshot::channel();
    let (provider, _) = start_streaming_sse_server(vec![
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_function_call(
                    "proven-effect",
                    "exec_command",
                    &json!({"cmd":"printf PROVEN_NATIVE_OUTPUT","yield_time_ms":1000}).to_string(),
                ),
                responses::ev_completed_with_tokens("proven-sample", /*total_tokens*/ 3),
            ]),
        }],
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("selected-open")]),
            },
            StreamingSseChunk {
                gate: Some(gate_a),
                body: responses::sse(vec![responses::ev_completed("selected-open")]),
            },
        ],
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("sibling-open")]),
            },
            StreamingSseChunk {
                gate: Some(gate_b),
                body: responses::sse(vec![
                    responses::ev_assistant_message("sibling-open", r#"{"sibling":"survived"}"#),
                    responses::ev_completed_with_tokens("sibling-open", /*total_tokens*/ 7),
                ]),
            },
        ],
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("later-human")]),
            },
            StreamingSseChunk {
                gate: Some(human_gate),
                body: responses::sse(vec![
                    responses::ev_assistant_message("later-human", "UNRELATED_HUMAN_COMPLETION"),
                    responses::ev_completed_with_tokens("later-human", /*total_tokens*/ 60),
                ]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_assistant_message(
                    "root-followup",
                    "The independent child reported completion.",
                ),
                responses::ev_completed("root-followup"),
            ]),
        }],
    ])
    .await;
    let bootstrap = responses::start_mock_server().await;
    let base_url = format!("{}/v1", provider.uri());
    let mut extensions =
        codex_extension_api::ExtensionRegistryBuilder::<codex_core::config::Config>::new();
    extensions.thread_lifecycle_contributor(std::sync::Arc::new(core_test_support::ThreadIdle));
    let test = builder()
        .with_extensions(std::sync::Arc::new(extensions.build()))
        .with_config(move |config| {
            config.model_provider.base_url = Some(base_url);
            config
                .permissions
                .set_permission_profile(PermissionProfile::Disabled)
                .expect("allow the authorized native fixture command");
        })
        .build_with_auto_env(&bootstrap)
        .await?;
    let a=test.codex.start_workflow(source("return await workflow.agent('Perform the native command then remain active.',{key:'selected',label:'selected'});"),json!({})).await.map_err(anyhow::Error::msg)?;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        provider.wait_for_request_count(/*count*/ 2),
    )
    .await?;
    let b=test.codex.start_workflow(source("return await workflow.agent('Keep this unrelated sibling native turn open.',{key:'sibling',label:'sibling'});"),json!({})).await.map_err(anyhow::Error::msg)?;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        provider.wait_for_request_count(/*count*/ 3),
    )
    .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let active = observe(&service, &a.id, |run| {
        run.metrics
            .usage
            .as_ref()
            .is_some_and(|usage| usage.total_tokens == 3)
            && run.steps.values().any(|step| {
                step.attempts
                    .last()
                    .is_some_and(|attempt| attempt.child_thread_id.is_some())
            })
    })
    .await?;
    let step = active
        .steps
        .values()
        .find(|step| step.label == "selected")
        .context("the selected accepted native step")?;
    let attempt = step
        .attempts
        .last()
        .context("the original accepted attempt")?;
    let child_id = ThreadId::from_string(
        attempt
            .child_thread_id
            .as_deref()
            .context("the accepted native child")?,
    )?;
    let child = test.thread_manager.get_thread(child_id).await?;
    let turn = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
        loop {
            if let Some(turn) = test
                .codex
                .active_workflow_owned_turn(&a.id, &step.logical_id, &attempt.id)
                .await
                .map_err(anyhow::Error::msg)?
            {
                break Ok::<_, anyhow::Error>(turn);
            }
            ticks.tick().await;
        }
    })
    .await??;
    let state = test
        .codex
        .workflow_control_state(&a.id)
        .await
        .map_err(anyhow::Error::msg)?;
    let operation = match action {
        StopAction::Interrupt => WorkflowControlOperation::Interrupt,
        StopAction::Skip => WorkflowControlOperation::Skip {
            replacement: WorkflowResult::Value {
                value: json!({"logical":"replacement"}),
            },
        },
    };
    let stopped = test
        .codex
        .workflow_control(
            &a.id,
            WorkflowControlRequest {
                command_id: "interrupt-selected".into(),
                expected_control_revision: state.control_revision,
                target: WorkflowControlTarget::Step {
                    logical_id: step.logical_id.clone(),
                    expected_attempt_id: Some(attempt.id.clone()),
                    expected_turn_id: Some(turn.clone()),
                },
                operation,
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(stopped.command.status, WorkflowControlStatus::Applied);
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30),core_test_support::wait_for_event(&child,|event|
        matches!(event,EventMsg::TurnAborted(aborted) if aborted.turn_id.as_deref()==Some(turn.as_str()))
    )).await?;
    let (status, result) = match action {
        StopAction::Interrupt => (codex_core::workflows::WorkflowStatus::Failed, None),
        StopAction::Skip => (
            codex_core::workflows::WorkflowStatus::Succeeded,
            Some(json!({"logical":"replacement"})),
        ),
    };
    let settled = observe(&service, &a.id, |run| {
        run.status == status && run.lifecycle.terminal_ready
    })
    .await?;
    assert_eq!(settled.accepted_starts, 1);
    assert_eq!(settled.result, result);
    assert_eq!(
        settled
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(3)
    );
    assert!(
        !settled.metrics.usage_complete
            && settled
                .metrics
                .usage_incomplete_reason
                .as_ref()
                .is_some_and(|reason| !reason.is_empty())
    );
    let selected = settled
        .steps
        .values()
        .find(|step| step.label == "selected")
        .context("the retained interrupted step")?;
    assert_eq!(selected.attempts.len(), 1);
    assert_eq!(
        (
            &selected.attempts[0].id,
            &selected.attempts[0].child_thread_id
        ),
        (&attempt.id, &attempt.child_thread_id)
    );
    assert_eq!(
        selected.attempts[0].status,
        codex_core::workflows::WorkflowStatus::Failed,
        "a controlled logical replacement cannot invent a successful native attempt"
    );
    if matches!(action, StopAction::Skip) {
        assert_eq!(selected.disposition, WorkflowStepDisposition::Skipped);
        assert!(
            selected.attempts[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("skip"))
        );
        assert_eq!(settled.lifecycle.events.iter().filter(|event|matches!(&event.kind,
            codex_dynamic_workflows::WorkflowLifecycleKind::StepSkipped {step_id,..} if step_id==&selected.id)).count(),1);
        assert!(!settled.lifecycle.events.iter().any(|event|matches!(&event.kind,
            codex_dynamic_workflows::WorkflowLifecycleKind::StepCompleted {step_id,..} if step_id==&selected.id)));
    }
    assert_eq!(
        test.codex
            .active_workflow_owned_turn(&a.id, &step.logical_id, &attempt.id)
            .await
            .map_err(anyhow::Error::msg)?,
        None
    );
    let sibling_before = service.get(&b.id).map_err(anyhow::Error::msg)?;
    assert_eq!(
        sibling_before.status,
        codex_core::workflows::WorkflowStatus::Running
    );
    let sibling_child = sibling_before
        .steps
        .values()
        .next()
        .and_then(|step| step.attempts.last())
        .and_then(|attempt| attempt.child_thread_id.clone())
        .context("the still accepted sibling child")?;
    assert_eq!(provider.requests().await.len(), 3);
    let later = child
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Run the independent human task after native retirement.".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let TurnInputSubmission::Started {
        turn_id: human_turn,
        ..
    } = later
    else {
        anyhow::bail!("retired native work must permit a new ordinary human turn: {later:?}");
    };
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        provider.wait_for_request_count(/*count*/ 4),
    )
    .await?;
    assert_eq!(
        test.codex
            .active_workflow_owned_turn(&a.id, &step.logical_id, &attempt.id)
            .await
            .map_err(anyhow::Error::msg)?,
        None
    );
    release_human
        .send(())
        .expect("complete the actual unrelated human inference");
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30),core_test_support::wait_for_event(&child,|event|
        matches!(event,EventMsg::TurnComplete(done) if done.turn_id==human_turn && done.error.is_none()
            && done.last_agent_message.as_deref()==Some("UNRELATED_HUMAN_COMPLETION"))
    )).await?;
    core_test_support::ThreadIdle::wait(&child).await;
    child.flush_rollout().await?;
    let history = test
        .thread_store
        .load_latest_model_context(codex_thread_store::LoadThreadHistoryParams {
            thread_id: child_id,
            include_archived: true,
        })
        .await?;
    let own = history
        .items
        .iter()
        .rev()
        .find_map(|item| match item {
            codex_history::RolloutItem::TokenUsageRecord(record)
                if record.thread_id == child_id =>
            {
                Some(&record.thread_token_usage)
            }
            _ => None,
        })
        .context("the actual native plus human token records")?;
    assert_eq!(
        own.total_tokens, 63,
        "the later human must really spend its sixty tokens on this same thread"
    );
    let after_human = service.get(&a.id).map_err(anyhow::Error::msg)?;
    assert_eq!(
        &after_human.steps[&selected.id].attempts, &selected.attempts,
        "retirement must freeze native owned turn IDs, final item identity and usage before later human work"
    );
    assert!(
        !after_human.steps[&selected.id].attempts[0]
            .owned_turn_ids
            .contains(&human_turn)
    );
    assert_eq!(after_human.metrics, settled.metrics);
    assert_eq!(
        service.get(&b.id).map_err(anyhow::Error::msg)?.status,
        codex_core::workflows::WorkflowStatus::Running
    );
    release_b
        .send(())
        .expect("targeted cancellation leaves the unrelated sibling stream open");
    let sibling = observe(&service, &b.id, |run| {
        run.status == codex_core::workflows::WorkflowStatus::Succeeded
            && run.lifecycle.terminal_ready
    })
    .await?;
    assert_eq!(sibling.result, Some(json!({"sibling":"survived"})));
    assert_eq!(
        sibling
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(7)
    );
    assert_eq!(sibling.accepted_starts, 1);
    assert_eq!(
        sibling
            .steps
            .values()
            .next()
            .and_then(|step| step.attempts.last())
            .and_then(|attempt| attempt.child_thread_id.as_deref()),
        Some(sibling_child.as_str())
    );
    assert_eq!(
        service.get(&a.id).map_err(anyhow::Error::msg)?.metrics,
        settled.metrics
    );
    assert_eq!(
        settled
            .metrics
            .usage
            .as_ref()
            .context("the selected native worker's proven usage")?
            .total_tokens
            + sibling
                .metrics
                .usage
                .as_ref()
                .context("the surviving sibling's proven usage")?
                .total_tokens,
        10
    );
    assert_eq!(
        provider.requests().await.len(),
        4,
        "ordinary completion queues mail without waking the owner"
    );
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Read the ordinary child completion report.".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        core_test_support::wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        }),
    )
    .await?;
    let requests = provider.requests().await;
    assert_eq!(requests.len(), 5);
    let owner_prompt: serde_json::Value = serde_json::from_slice(&requests[4])?;
    assert_eq!(
        owner_prompt["input"]
            .as_array()
            .context("the owner's explicit normal follow-up")?
            .iter()
            .filter(|item| item["type"] == "agent_message"
                && item.to_string().contains("UNRELATED_HUMAN_COMPLETION"))
            .count(),
        1,
        "the retired child turn must keep ordinary completion routing rather than be suppressed as workflow work"
    );
    assert_eq!(
        service.get(&a.id).map_err(anyhow::Error::msg)?.metrics,
        settled.metrics
    );
    let sampled: serde_json::Value = serde_json::from_slice(&requests[1])?;
    let output = sampled["input"]
        .as_array()
        .context("the actual follow-up native prompt")?
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == "proven-effect")
        .context("the completed native command result in the next provider request")?;
    assert!(
        output["output"]
            .as_str()
            .context("the native command's actual output")?
            .contains("PROVEN_NATIVE_OUTPUT")
    );
    let _ = release_a.send(());
    test.codex.shutdown_and_wait().await?;
    provider.shutdown().await;
    Ok(())
}
