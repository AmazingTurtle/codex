//! Real native results become durable human history without an implicit model turn.

use anyhow::Context;
use anyhow::Result;
use codex_core::workflows::WorkflowStatus;
use codex_features::Feature;
use codex_history::RolloutItem;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::workflow::WorkflowLifecycleEvent;
use codex_protocol::workflow::WorkflowLifecycleKind;
use codex_protocol::workflow::WorkflowLifecycleStatus;
use codex_protocol::workflow::WorkflowResult;
use codex_thread_store::LoadThreadHistoryParams;
use core_test_support::responses;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use test_case::test_case;

#[path = "workflow_conversation_store.rs"]
mod publication_store;
use publication_store::FaultBoundary;
use publication_store::PublicationFaultStore;

fn builder() -> TestCodexBuilder {
    test_codex()
        .with_model("gpt-5.6-sol")
        .with_model_info_override("gpt-5.6-sol", |model| {
            model.tool_mode = Some(ToolMode::Direct);
            model.multi_agent_version = Some(MultiAgentVersion::V2);
        })
        .with_config(|config| {
            for feature in [
                Feature::Collab,
                Feature::MultiAgentV2,
                Feature::CodeModeHost,
                Feature::DynamicWorkflows,
            ] {
                config
                    .features
                    .enable(feature)
                    .expect("enable native workflows");
            }
            config.multi_agent_v2.non_code_mode_only = false;
            config
                .features
                .disable(Feature::WakeOnAgentMessages)
                .expect("a display result does not imply model continuation");
        })
}

async fn presented(test: &TestCodex) -> Result<Vec<WorkflowLifecycleEvent>> {
    test.codex.flush_rollout().await?;
    let history = test
        .thread_store
        .load_latest_model_context(LoadThreadHistoryParams {
            thread_id: test.session_configured.thread_id,
            include_archived: true,
        })
        .await?;
    Ok(history
        .items
        .into_iter()
        .filter_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::WorkflowLifecycle(event)) => Some(event),
            _ => None,
        })
        .collect())
}

#[test_case(FaultBoundary::Append; "completion_append_failure_retries_without_a_new_trigger")]
#[test_case(FaultBoundary::Flush; "completion_flush_failure_retries_without_a_duplicate")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifecycle_publication_retries_the_real_store_boundary_without_another_model_turn(
    boundary: FaultBoundary,
) -> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let store = Arc::new(PublicationFaultStore::new(boundary));
    let configure = Arc::clone(&store);
    let test = builder()
        .with_thread_store(store.clone())
        .with_config(move |config| configure.configure(config))
        .build_with_auto_env(&server)
        .await?;
    let run = test
        .codex
        .start_workflow(
            super::workflow_v2::script("return {retained:'PUBLICATION_RESULT'};"),
            json!({}),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let observed=tokio::time::timeout(Duration::from_secs(/*secs*/ 30),wait_for_event(&test.codex,|event|
        matches!(event,EventMsg::WorkflowLifecycle(fact) if fact.run_id==run.id && matches!(&fact.kind,WorkflowLifecycleKind::Completed {..})))).await?;
    let EventMsg::WorkflowLifecycle(observed) = observed else {
        unreachable!()
    };
    assert!(
        store.failed(),
        "the real lifecycle store operation must actually encounter its injected transient failure"
    );
    let facts = presented(&test).await?;
    assert_eq!(facts.len(), 2);
    assert_eq!(
        facts
            .iter()
            .map(|fact| fact.id.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    assert_eq!(
        facts.iter().filter(|fact| fact.id == observed.id).count(),
        1,
        "a successful append followed by failed flush must not append the same fact twice"
    );
    assert!(
        responses::received_responses_requests(&server)
            .await
            .is_empty(),
        "durable publication retry must not manufacture model work"
    );
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    service.get(&run.id).map_err(anyhow::Error::msg)?;
    test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    assert_eq!(presented(&test).await?, facts);
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicitly_displaying_a_preexisting_current_format_null_result_is_stable_without_backfill()
-> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = builder().build_with_auto_env(&server).await?;
    test.codex.ensure_rollout_materialized().await;
    let run = uuid::Uuid::new_v4().to_string();
    let execution = uuid::Uuid::new_v4().to_string();
    let definition = uuid::Uuid::new_v4().to_string();
    let result = uuid::Uuid::new_v4().to_string();
    let root = test
        .home
        .path()
        .join("workflow-runs")
        .join(test.session_configured.thread_id.to_string());
    let records = root.join(&run).join("records");
    std::fs::create_dir_all(&records)?;
    std::fs::write(
        records.join(format!("{definition}.json")),
        serde_json::to_vec(&json!({
            "source":super::workflow_v2::script("return null;"),"input":{},"nested_workflows":{},
        }))?,
    )?;
    std::fs::write(records.join(format!("{result}.json")), b"null")?;
    let original = serde_json::to_vec(&json!({
        "storageVersion":2,"revision":uuid::Uuid::new_v4().to_string(),"previousRecords":[],
        "definition":definition,"steps":{},"result":result,"usage":{},
        "header":{"id":run,"executionId":execution,"ownerThreadId":test.session_configured.thread_id.to_string(),
            "formatVersion":2,"lifecycleVersion":0,"notificationDelivered":true,
            "source":"","input":null,"status":"succeeded","phase":null,"steps":{},"error":null,"createdAt":1,"updatedAt":2},
    }))?;
    let path = root.join(format!("{run}.json"));
    std::fs::write(&path, &original)?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let historical = service.get(&run).map_err(anyhow::Error::msg)?;
    assert_eq!(historical.result, Some(Value::Null));
    assert!(!historical.lifecycle.enabled && historical.lifecycle.events.is_empty());
    assert!(
        presented(&test).await?.is_empty(),
        "opening historical runs must not synthesize lifecycle history"
    );
    let first = test
        .codex
        .display_workflow_result(&run, Some(&execution))
        .await
        .map_err(anyhow::Error::msg)?;
    let second = test
        .codex
        .display_workflow_result(&run, Some(&execution))
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(second, first);
    let WorkflowLifecycleKind::ResultShown { result, .. } = &first.kind else {
        panic!("explicit result must be a stable ResultShown presentation fact")
    };
    assert_eq!(result, &WorkflowResult::Value { value: Value::Null });
    assert_eq!(presented(&test).await?, vec![first]);
    assert_eq!(
        std::fs::read(&path)?,
        original,
        "explicit display must preserve the historical producer record"
    );
    assert!(
        responses::received_responses_requests(&server)
            .await
            .is_empty()
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

struct HeldParentTool {
    reached: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

struct ParentToolGate(Mutex<Option<HeldParentTool>>);

impl codex_extension_api::ToolLifecycleContributor for ParentToolGate {
    fn on_tool_finish<'a>(
        &'a self,
        input: codex_extension_api::ToolFinishInput<'a>,
    ) -> codex_extension_api::ToolLifecycleFuture<'a> {
        Box::pin(async move {
            if input.call_id != "held-parent-tool" {
                return;
            }
            let gate = self.0.lock().expect("parent tool gate").take();
            if let Some(gate) = gate {
                gate.reached
                    .send(())
                    .expect("the actual parent tool completed");
                let _ = gate.release.await;
            }
        })
    }
}

#[derive(Clone, Copy)]
enum ParentCompletion {
    Display,
    Continue,
    ContinueSuperseded,
}

#[test_case(ParentCompletion::Display; "active_parent_display_does_not_change_its_model_context")]
#[test_case(ParentCompletion::Continue; "explicit_continue_runs_once_after_the_captured_parent")]
#[test_case(ParentCompletion::ContinueSuperseded; "a_new_human_turn_supersedes_the_captured_continuation")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_completion_obeys_the_captured_parent_and_explicit_mode(
    mode: ParentCompletion,
) -> Result<()> {
    use codex_core::TurnInputRequest;
    use codex_core::workflows::WorkflowCompletionMode;
    use codex_protocol::protocol::Op;
    use codex_protocol::user_input::UserInput;

    core_test_support::skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let (reached, waiting) = tokio::sync::oneshot::channel();
    let (release, gate) = tokio::sync::oneshot::channel();
    let mut extensions =
        codex_extension_api::ExtensionRegistryBuilder::<codex_core::config::Config>::new();
    extensions.tool_lifecycle_contributor(Arc::new(ParentToolGate(Mutex::new(Some(
        HeldParentTool {
            reached,
            release: gate,
        },
    )))));
    let first = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_function_call("held-parent-tool", "test_sync_tool", "{}"),
            responses::ev_completed("parent-tool"),
        ]),
    )
    .await;
    let test = builder()
        .with_extensions(Arc::new(extensions.build()))
        .with_model_info_override("gpt-5.6-sol", |model| {
            model
                .experimental_supported_tools
                .push("test_sync_tool".into())
        })
        .build_with_auto_env(&server)
        .await?;
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Finish this original human task after its tool is available.".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30), waiting).await??;
    first.single_request();
    let private_result = json!({"private":format!("{}ACTIVE_PARENT_FULL_RESULT","x".repeat(512))});
    responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_assistant_message("msg_worker", &private_result.to_string()),
            responses::ev_completed("worker"),
        ]),
    )
    .await;
    let completion_mode = match mode {
        ParentCompletion::Display => WorkflowCompletionMode::Display,
        ParentCompletion::Continue | ParentCompletion::ContinueSuperseded => {
            WorkflowCompletionMode::Continue
        }
    };
    let run=test.codex.start_workflow_with_completion_mode(super::workflow_v2::script("return await workflow.agent('Produce the separate workflow result.',{label:'separate'});"),json!({}),completion_mode)
        .await.map_err(anyhow::Error::msg)?;
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30),wait_for_event(&test.codex,|event|
        matches!(event,EventMsg::WorkflowLifecycle(fact) if fact.run_id==run.id && matches!(&fact.kind,WorkflowLifecycleKind::Completed {..})))).await?;
    assert_eq!(
        responses::received_responses_requests(&server).await.len(),
        2,
        "completion must neither interrupt nor sample alongside the still-active parent"
    );
    if matches!(mode, ParentCompletion::Continue) {
        responses::mount_sse_once(
            &server,
            responses::sse(vec![
                responses::ev_function_call(
                    "inspect-pending-continuation",
                    "workflow_read",
                    &json!({"run_id":run.id,"section":"metadata"}).to_string(),
                ),
                responses::ev_completed("parent-status"),
            ]),
        )
        .await;
    }
    responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_assistant_message("msg_parent_final", "The human task is complete."),
            responses::ev_completed("parent-final"),
        ]),
    )
    .await;
    if matches!(mode, ParentCompletion::Continue) {
        responses::mount_sse_once(
            &server,
            responses::sse(vec![
                responses::ev_assistant_message(
                    "msg_continued",
                    "The explicit continuation is complete.",
                ),
                responses::ev_completed("continued"),
            ]),
        )
        .await;
    }
    if matches!(mode, ParentCompletion::ContinueSuperseded) {
        test.codex.submit(Op::Interrupt).await?;
        tokio::time::timeout(
            Duration::from_secs(/*secs*/ 30),
            wait_for_event(&test.codex, |event| {
                matches!(event, EventMsg::TurnAborted(_))
            }),
        )
        .await?;
        test.codex
            .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
                text: "A newer independent human task replaces the captured parent.".into(),
                text_elements: Vec::new(),
            }]))
            .await?;
    } else {
        release
            .send(())
            .expect("let the original parent finish normally");
    }
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        }),
    )
    .await?;
    let expected = if matches!(mode, ParentCompletion::Continue) {
        5
    } else {
        3
    };
    let requests = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
        loop {
            let requests = responses::received_responses_requests(&server).await;
            if requests.len() >= expected {
                return requests;
            }
            ticks.tick().await;
        }
    })
    .await?;
    assert_eq!(requests.len(), expected);
    for request in &requests {
        for item in request.input() {
            let prefix = match item["type"].as_str() {
                Some("message") => Some("msg_"),
                Some("agent_message") => Some("amsg_"),
                _ => None,
            };
            if let Some(prefix) = prefix
                && let Some(id) = item["id"].as_str()
            {
                assert!(
                    id.starts_with(prefix) && id.len() <= 64,
                    "the actual outbound {} ID must use {prefix} and stay provider-sized: {id}",
                    item["type"]
                );
            }
        }
    }
    assert!(!requests[2].body_contains_text("ACTIVE_PARENT_FULL_RESULT"));
    if matches!(mode, ParentCompletion::Continue) {
        let status = requests[3]
            .function_call_output("inspect-pending-continuation")
            .to_string();
        assert!(
            status.contains(&run.id) && status.contains("succeeded"),
            "a parent workflow_read call must finish while its own continuation is pending"
        );
        assert!(!requests[3].body_contains_text("ACTIVE_PARENT_FULL_RESULT"));
        assert!(requests[4].body_contains_text(&run.id));
        assert!(requests[4].body_contains_text("<workflow_status>"));
        assert!(!requests[4].body_contains_text("ACTIVE_PARENT_FULL_RESULT"));
        let fragments = requests[4]
            .inputs_of_type("message")
            .into_iter()
            .filter(|item| item.to_string().contains("<workflow_status>"))
            .collect::<Vec<_>>();
        assert_eq!(
            fragments.len(),
            1,
            "the explicit continuation must carry exactly one canonical typed completion fragment"
        );
        let fragment_id = fragments[0]["id"]
            .as_str()
            .context("the typed continuation must retain its stable Message identity")?;
        assert!(fragment_id.starts_with("msg_") && fragment_id.len() <= 64);
        assert_eq!(
            requests[4].inputs_of_type("agent_message"),
            Vec::<Value>::new(),
            "an explicit continuation is a bounded typed response item, not generic worker mail"
        );
        tokio::time::timeout(
            Duration::from_secs(/*secs*/ 30),
            wait_for_event(&test.codex, |event| {
                matches!(event, EventMsg::TurnComplete(_))
            }),
        )
        .await?;
    } else {
        assert!(
            !requests[2].body_contains_text(&run.id),
            "display and superseded continuations must not inject even a non-triggering completion fragment"
        );
    }
    assert_eq!(
        responses::received_responses_requests(&server).await.len(),
        expected
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_repairs_publish_one_final_result_with_stable_durable_identity_without_waking_the_model()
-> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let (release, gate) = tokio::sync::oneshot::channel();
    let (provider, _) = start_streaming_sse_server(vec![
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_assistant_message("invalid", r#"{"ok":"wrong"}"#),
                responses::ev_completed_with_tokens("invalid", /*total_tokens*/ 3),
            ]),
        }],
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("repair-held")]),
            },
            StreamingSseChunk {
                gate: Some(gate),
                body: responses::sse(vec![
                    responses::ev_function_call("validated-tool", "test_sync_tool", "{}"),
                    responses::ev_completed_with_tokens("repair-held", /*total_tokens*/ 5),
                ]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_assistant_message(
                    "valid",
                    r#"{"ok":true,"secret":"REPAIR_RESULT_PRIVATE"}"#,
                ),
                responses::ev_completed_with_tokens("valid", /*total_tokens*/ 7),
            ]),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_assistant_message("human", "The requested workflow is complete."),
                responses::ev_completed("human"),
            ]),
        }],
    ])
    .await;
    let bootstrap = responses::start_mock_server().await;
    let base_url = format!("{}/v1", provider.uri());
    let test = builder()
        .with_model_info_override("gpt-5.6-sol", |model| {
            model
                .experimental_supported_tools
                .push("test_sync_tool".into())
        })
        .with_config(move |config| config.model_provider.base_url = Some(base_url))
        .build_with_auto_env(&bootstrap)
        .await?;
    test.codex.ensure_rollout_materialized().await;
    let run = test
        .codex
        .start_workflow(
            super::workflow_v2::script(
                r#"
await workflow.agent("Return the validated Boolean object.", {label:"validated",schema:{
  type:"object",properties:{ok:{type:"boolean"}},required:["ok"]
}});
return null;
"#,
            ),
            json!({}),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        provider.wait_for_request_count(/*count*/ 2),
    )
    .await?;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::WorkflowLifecycle(fact) if fact.run_id == run.id
            && matches!(&fact.kind, WorkflowLifecycleKind::Started { .. }))
        }),
    )
    .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let during = service.get(&run.id).map_err(anyhow::Error::msg)?;
    assert_eq!(during.status, WorkflowStatus::Running);
    let repairing = during
        .steps
        .values()
        .next()
        .expect("the actual schema step");
    assert_eq!(repairing.attempts.len(), 2);
    assert!(repairing.attempts[0].schema_error.is_some());
    let facts = presented(&test).await?;
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(fact.kind, WorkflowLifecycleKind::Completed { .. }))
            .count(),
        0,
        "a rejected structured response must not complete the human-facing workflow task"
    );
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(fact.kind, WorkflowLifecycleKind::StepCompleted { .. }))
            .count(),
        0,
        "intermediate schema repair attempts must not produce a friendly task-completed item"
    );
    assert_eq!(provider.requests().await.len(), 2);
    release
        .send(())
        .expect("finish the real corrective native child");
    let completed = tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::WorkflowLifecycle(fact) if fact.run_id == run.id
            && matches!(&fact.kind, WorkflowLifecycleKind::Completed { .. }))
        }),
    )
    .await?;
    let EventMsg::WorkflowLifecycle(completed) = completed else {
        unreachable!()
    };
    assert_eq!(
        completed.owner_thread_id,
        test.session_configured.thread_id.to_string()
    );
    assert_eq!(completed.execution_id, run.execution_id);
    let WorkflowLifecycleKind::Completed { status, result, .. } = &completed.kind else {
        unreachable!()
    };
    assert_eq!(
        (*status, result),
        (
            WorkflowLifecycleStatus::Succeeded,
            &WorkflowResult::Value { value: Value::Null }
        )
    );
    let facts = presented(&test).await?;
    assert_eq!(
        facts.len(),
        3,
        "one start, one validated task result, and one final workflow result"
    );
    assert_eq!(
        facts
            .iter()
            .map(|fact| fact.id.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    assert_eq!(
        facts.iter().map(|fact| fact.sequence).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(
        facts
            .iter()
            .all(|fact| fact.run_id == run.id && fact.execution_id == run.execution_id)
    );
    let steps = facts
        .iter()
        .filter_map(|fact| match &fact.kind {
            WorkflowLifecycleKind::StepCompleted {
                step_id,
                attempt_id,
                child_thread_id,
                owned_turn_ids,
                status,
                result,
                ..
            } => Some((
                step_id,
                attempt_id,
                child_thread_id,
                owned_turn_ids,
                *status,
                result,
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].1, &repairing.attempts[1].id);
    assert_eq!(
        Some(steps[0].2.as_str()),
        repairing.attempts[1].child_thread_id.as_deref()
    );
    assert!(steps.iter().all(|step| !step.3.is_empty()));
    assert_eq!(
        steps[0].5,
        &WorkflowResult::Value {
            value: json!({"ok":true,"secret":"REPAIR_RESULT_PRIVATE"})
        }
    );
    assert_ne!(
        repairing.attempts[0].child_thread_id,
        repairing.attempts[1].child_thread_id
    );
    assert_eq!(
        provider.requests().await.len(),
        3,
        "display-only lifecycle delivery must not wake another model request"
    );
    let worker_after_tool: Value = serde_json::from_slice(&provider.requests().await[2])?;
    assert!(
        worker_after_tool["input"]
            .as_array()
            .expect("actual corrective child input")
            .iter()
            .any(|item| item["type"] == "function_call_output"
                && item["call_id"] == "validated-tool"),
        "the validated child must really execute and consume its normal tool result"
    );
    test.submit_text_turn("A normal human continuation after the workflow.")
        .await?;
    let requests = provider.requests().await;
    assert_eq!(requests.len(), 4);
    let human: Value = serde_json::from_slice(&requests[3])?;
    assert!(
        !human["input"].to_string().contains("REPAIR_RESULT_PRIVATE"),
        "full lifecycle values are human history, never injected model input"
    );
    let resumed = builder().restart_with_auto_env(&bootstrap, &test).await?;
    resumed
        .codex
        .workflows()
        .await
        .map_err(anyhow::Error::msg)?;
    resumed
        .codex
        .workflows()
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        presented(&resumed).await?,
        facts,
        "replaying a drained outbox must not duplicate or rewrite durable lifecycle identities"
    );
    assert_eq!(provider.requests().await.len(), 4);
    assert!(
        responses::received_responses_requests(&bootstrap)
            .await
            .is_empty()
    );
    resumed.codex.shutdown_and_wait().await?;
    provider.shutdown().await;
    Ok(())
}
