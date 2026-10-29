//! Provider limits apply to fresh notices and to legacy receipts resumed from history or pause.

use anyhow::Context;
use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_core::workflows::WorkflowRun;
use codex_features::Feature;
use codex_history::RolloutItem;
use codex_protocol::AgentPath;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecutionControlState;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::turn_input::TurnStartOptions;
use codex_protocol::user_input::UserInput;
use codex_thread_store::AppendThreadItemsParams;
use codex_thread_store::FindPersistedInputIdsParams;
use codex_thread_store::PersistedInputId;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::submit_thread_settings;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeSet;
use std::time::Duration;
use test_case::test_case;
use uuid::Uuid;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

fn builder(responses_lite: bool) -> TestCodexBuilder {
    test_codex()
        .with_model("gpt-5.6-sol")
        .with_model_info_override("gpt-5.6-sol", move |model| {
            model.use_responses_lite = responses_lite
        })
        .with_config(|config| {
            config
                .features
                .disable(Feature::WakeOnAgentMessages)
                .expect("queue notices until human continuation");
            for feature in [Feature::CodeModeHost, Feature::DynamicWorkflows] {
                config
                    .features
                    .enable(feature)
                    .expect("enable workflow runtime");
            }
        })
}

async fn mount_strict_provider(server: &MockServer) {
    let answer = responses::sse(vec![
        responses::ev_assistant_message("msg_continued", "continued"),
        responses::ev_completed("continued"),
    ]);
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(move |request: &wiremock::Request| {
            let bytes = if request
                .headers
                .get("content-encoding")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("zstd"))
            {
                zstd::stream::decode_all(std::io::Cursor::new(&request.body))
                    .expect("decode provider request")
            } else {
                request.body.clone()
            };
            let body: Value = serde_json::from_slice(&bytes).expect("provider request JSON");
            let invalid_id = body["input"]
                .as_array()
                .expect("request input")
                .iter()
                .enumerate()
                .find_map(|(index, item)| {
                    let id = item["id"].as_str()?;
                    let problem = if id.chars().count() > 64 {
                        "exceeds the provider limit of 64 characters"
                    } else if item["type"] == "agent_message" && !id.starts_with("amsg_") {
                        "must begin with amsg_ for an agent_message"
                    } else if item["type"] == "message" && !id.starts_with("msg_") {
                        "must begin with msg_ for a message"
                    } else {
                        return None;
                    };
                    Some((index, problem))
                });
            if let Some((index, problem)) = invalid_id {
                return ResponseTemplate::new(400).set_body_json(json!({"error": {
                    "message": format!("input[{index}].id {problem}"),
                    "type": "invalid_request_error", "param": format!("input[{index}].id")
                }}));
            }
            responses::sse_response(answer.clone())
        })
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
}

async fn continue_successfully(test: &TestCodex) -> Result<()> {
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Continue after the workflow report.".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let event = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let EventMsg::TurnComplete(complete) = event else {
        unreachable!()
    };
    assert!(
        complete.error.is_none(),
        "strict provider rejected the request: {:?}",
        complete.error
    );
    assert_eq!(complete.last_agent_message.as_deref(), Some("continued"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn historical_typed_message_uses_a_request_only_msg_alias_after_real_resume() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let identity = Uuid::new_v4();
    let historical_id = ResponseItemId::with_suffix("amsg", identity);
    let alias = ResponseItemId::with_suffix("msg", identity);
    let valid_id = ResponseItemId::with_suffix("msg", Uuid::new_v4());
    let first = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_assistant_message(&historical_id, "HISTORICAL_TYPED_MESSAGE_CONTENT"),
            responses::ev_assistant_message(&valid_id, "VALID_TYPED_MESSAGE_CONTENT"),
            responses::ev_completed("first-normal-turn"),
        ]),
    )
    .await;
    let mut extensions =
        codex_extension_api::ExtensionRegistryBuilder::<codex_core::config::Config>::new();
    extensions.thread_lifecycle_contributor(std::sync::Arc::new(core_test_support::ThreadIdle));
    let initial = builder(/*responses_lite*/ false)
        .with_extensions(std::sync::Arc::new(extensions.build()))
        .build_with_auto_env(&server)
        .await?;
    initial
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Record the original assistant messages.".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let completed = wait_for_event(&initial.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert!(matches!(completed,EventMsg::TurnComplete(done) if done.error.is_none()));
    core_test_support::ThreadIdle::wait(&initial.codex).await;
    first.single_request();
    initial.codex.flush_rollout().await?;
    let root = initial.session_configured.thread_id;
    let original = initial
        .thread_store
        .load_latest_model_context(codex_thread_store::LoadThreadHistoryParams {
            thread_id: root,
            include_archived: true,
        })
        .await?;
    let original_rows=original.items.iter().filter_map(|item|match item {
        RolloutItem::ResponseItem(envelope) if matches!(&envelope.item,ResponseItem::Message {id:Some(id),..} if id==&historical_id || id==&valid_id)=>Some(envelope.clone()),
        _=>None,
    }).collect::<Vec<_>>();
    assert_eq!(
        original_rows.len(),
        2,
        "both actual provider-produced typed Messages must be durably retained"
    );
    let historical = original_rows
        .iter()
        .find(|row| row.item.id() == Some(&historical_id))
        .context("the typed historical Message with its old amsg identity")?;
    mount_strict_provider(&server).await;
    let resumed = builder(/*responses_lite*/ false)
        .restart_with_auto_env(&server, &initial)
        .await?;
    assert_eq!(resumed.session_configured.thread_id, root);
    assert_eq!(
        responses::received_responses_requests(&server).await.len(),
        1,
        "resuming the persisted conversation must not create inference"
    );
    continue_successfully(&resumed).await?;
    let requests = responses::received_responses_requests(&server).await;
    assert_eq!(requests.len(), 2);
    let messages = requests[1].inputs_of_type("message");
    let migrated = messages
        .iter()
        .filter(|item| {
            item.to_string()
                .contains("HISTORICAL_TYPED_MESSAGE_CONTENT")
        })
        .collect::<Vec<_>>();
    assert_eq!(migrated.len(), 1);
    let mut expected = serde_json::to_value(&historical.item)?;
    expected["id"] = json!(alias);
    assert_eq!(
        migrated[0], &expected,
        "only the outbound typed Message identity is aliased"
    );
    assert_eq!(
        messages
            .iter()
            .filter(|item| item["id"] == valid_id.as_str())
            .count(),
        1,
        "valid provider Message identities remain unchanged"
    );
    assert!(messages.iter().all(|item| {
        item["id"]
            .as_str()
            .is_none_or(|id| id.starts_with("msg_") && id.len() <= 64)
    }));
    resumed.codex.flush_rollout().await?;
    let stored = resumed
        .thread_store
        .load_latest_model_context(codex_thread_store::LoadThreadHistoryParams {
            thread_id: root,
            include_archived: true,
        })
        .await?;
    let retained=stored.items.iter().filter_map(|item|match item {
        RolloutItem::ResponseItem(envelope) if matches!(&envelope.item,ResponseItem::Message {id:Some(id),..} if id==&historical_id || id==&valid_id)=>Some(envelope.clone()),
        _=>None,
    }).collect::<Vec<_>>();
    assert_eq!(
        retained, original_rows,
        "request migration must preserve both original raw rows exactly once"
    );
    assert!(!stored.items.iter().any(|item|matches!(item,RolloutItem::ResponseItem(envelope) if envelope.item.id()==Some(&alias))),
        "the outbound alias must never be appended as a rewritten historical row");
    resumed.codex.shutdown_and_wait().await?;
    Ok(())
}

#[test_case(false; "responses")]
#[test_case(true; "responses_lite")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_workflow_lifecycle_stays_out_of_provider_input(responses_lite: bool) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    mount_strict_provider(&server).await;
    let test = builder(responses_lite).build_with_auto_env(&server).await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let run = service
        .start(super::workflow_v2::script("return {done:true};"), json!({}))
        .await
        .map_err(anyhow::Error::msg)?;
    let delivered = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), wait_for_event(&test.codex, |event|
        matches!(event, EventMsg::WorkflowLifecycle(fact) if fact.run_id == run.id
            && matches!(&fact.kind, codex_protocol::workflow::WorkflowLifecycleKind::Completed { .. })))).await?;
    let EventMsg::WorkflowLifecycle(delivered) = delivered else {
        unreachable!()
    };
    assert!(delivered.id.starts_with("wfe_") && delivered.id.len() <= 64);
    let codex_protocol::workflow::WorkflowLifecycleKind::Completed { result, .. } = delivered.kind
    else {
        unreachable!()
    };
    assert_eq!(
        result,
        codex_protocol::workflow::WorkflowResult::Value {
            value: json!({"done":true})
        }
    );
    submit_thread_settings(&test.codex, ThreadSettingsOverrides::default()).await?;
    assert!(
        responses::received_responses_requests(&server)
            .await
            .is_empty(),
        "API-only launch must not manufacture a model turn"
    );
    continue_successfully(&test).await?;
    let requests = responses::received_responses_requests(&server).await;
    assert_eq!(requests.len(), 1);
    let ids = requests[0].body_json()["input"]
        .as_array()
        .expect("request input")
        .iter()
        .filter(|item| item["type"] == "agent_message")
        .filter_map(|item| item["id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        Vec::<String>::new(),
        "new display results must not create agent mail in model context"
    );
    assert!(
        !serde_json::to_string(&requests[0].input())?.contains(&run.id),
        "the human presentation event must not enter provider input"
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[derive(Clone, Copy)]
enum LegacyReceipt {
    Persisted,
    PausedPending,
}

#[derive(Clone, Copy)]
enum LegacyNoticeId {
    Original,
    Compact,
}

#[test_case(false, LegacyReceipt::Persisted, LegacyNoticeId::Original; "responses_original_persisted")]
#[test_case(true, LegacyReceipt::Persisted, LegacyNoticeId::Original; "responses_lite_original_persisted")]
#[test_case(false, LegacyReceipt::PausedPending, LegacyNoticeId::Original; "responses_original_paused_pending")]
#[test_case(true, LegacyReceipt::PausedPending, LegacyNoticeId::Original; "responses_lite_original_paused_pending")]
#[test_case(false, LegacyReceipt::Persisted, LegacyNoticeId::Compact; "responses_compact_persisted")]
#[test_case(true, LegacyReceipt::Persisted, LegacyNoticeId::Compact; "responses_lite_compact_persisted")]
#[test_case(false, LegacyReceipt::PausedPending, LegacyNoticeId::Compact; "responses_compact_paused_pending")]
#[test_case(true, LegacyReceipt::PausedPending, LegacyNoticeId::Compact; "responses_lite_compact_paused_pending")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_legacy_workflow_notice_is_aliased_without_redelivery_or_history_rewrite(
    responses_lite: bool,
    receipt: LegacyReceipt,
    notice_id: LegacyNoticeId,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    mount_strict_provider(&server).await;
    let initial = builder(responses_lite).build_with_auto_env(&server).await?;
    let run_id = "0e8de8a1-6597-4aa6-b0a2-60eca8980acf";
    let execution_id = "282444a6-6b36-4899-9a26-c12bde2f0801";
    let original_id = ResponseItemId::with_suffix("workflow", format!("{run_id}-{execution_id}"));
    let identity = Uuid::new_v5(&Uuid::NAMESPACE_OID, original_id.as_bytes());
    let legacy_id = match notice_id {
        LegacyNoticeId::Original => original_id,
        LegacyNoticeId::Compact => ResponseItemId::with_suffix("workflow", identity),
    };
    let alias = ResponseItemId::with_suffix("amsg", identity);
    let root = initial.session_configured.thread_id;
    // A current-format run can still have a historical notice ID accepted before
    // its delivery acknowledgement. Message compatibility must not execute V1 runs.
    let mut expected: WorkflowRun = serde_json::from_value(json!({
        "id":run_id, "formatVersion":2,
        "metadata":{"name":"core_integration","description":"Exercise the production workflow service and native child lifecycle.","phases":[]},
        "executionId":execution_id, "notificationDelivered":false,
        "ownerThreadId":root.to_string(), "source":super::workflow_v2::script("throw new Error('completed source must never replay');"),
        "input":{}, "status":"succeeded", "phase":null, "steps":{},
        "metrics":{"usage":{"inputTokens":0,"outputTokens":0,"cachedInputTokens":0,"totalTokens":0},
            "usageComplete":true,"activeDurationMs":null,"wallDurationMs":0},
        "result":{"retained":true}, "error":null, "createdAt":1, "updatedAt":2
    }))?;
    let directory = initial
        .config
        .codex_home
        .join("workflow-runs")
        .join(root.to_string());
    let records = directory.join(run_id).join("records");
    std::fs::create_dir_all(&records)?;
    let definition = Uuid::new_v4().to_string();
    let result = Uuid::new_v4().to_string();
    std::fs::write(
        records.join(format!("{definition}.json")),
        serde_json::to_vec(&json!({
            "source":expected.source,"input":expected.input,"nested_workflows":{},
        }))?,
    )?;
    std::fs::write(
        records.join(format!("{result}.json")),
        serde_json::to_vec(&expected.result)?,
    )?;
    let mut header = expected.clone();
    header.source.clear();
    header.input = Value::Null;
    header.result = None;
    std::fs::write(
        directory.join(format!("{run_id}.json")),
        serde_json::to_vec(&json!({
            "storageVersion":2,"revision":Uuid::new_v4().to_string(),"previousRecords":[],
            "header":header,"definition":definition,"steps":{},"result":result,"usage":{},
        }))?,
    )?;
    let mut communication = InterAgentCommunication::new(
        AgentPath::root(),
        AgentPath::root(),
        Vec::new(),
        "<workflow_status>Historical workflow completed; its result is retained.</workflow_status>"
            .into(),
        /*trigger_turn*/ false,
    );
    communication.id = Some(legacy_id.clone());
    initial.codex.ensure_rollout_materialized().await;
    match receipt {
        LegacyReceipt::Persisted => {
            initial
                .thread_store
                .append_items(AppendThreadItemsParams {
                    thread_id: root,
                    items: vec![RolloutItem::ResponseItem(
                        communication.to_model_input_item().into(),
                    )],
                })
                .await?;
            initial.codex.flush_rollout().await?;
        }
        LegacyReceipt::PausedPending => {
            initial
                .codex
                .submit(Op::InterAgentCommunication {
                    communication,
                    start_options: TurnStartOptions::default(),
                })
                .await?;
            // Submission processing is serial; this existing settings barrier proves
            // the real legacy report is queued before pause snapshots it durably.
            submit_thread_settings(&initial.codex, ThreadSettingsOverrides::default()).await?;
            assert!(initial.codex.can_pause_execution().await?);
            assert_eq!(
                initial.codex.pause_execution().await?.state,
                ExecutionControlState::Paused
            );
        }
    }
    let resumed = builder(responses_lite)
        .restart_with_auto_env(&server, &initial)
        .await?;
    let service = resumed
        .codex
        .workflows()
        .await
        .map_err(anyhow::Error::msg)?;
    resumed
        .codex
        .workflows()
        .await
        .map_err(anyhow::Error::msg)?;
    expected.notification_delivered = true;
    assert_eq!(service.get(run_id).map_err(anyhow::Error::msg)?, expected);
    assert!(
        responses::received_responses_requests(&server)
            .await
            .is_empty(),
        "receipt recovery must neither rerun the source nor wake a new model turn"
    );
    continue_successfully(&resumed).await?;
    let requests = responses::received_responses_requests(&server).await;
    assert_eq!(requests.len(), 1);
    let ids = requests[0].body_json()["input"]
        .as_array()
        .expect("request input")
        .iter()
        .filter(|item| item["type"] == "agent_message")
        .filter_map(|item| item["id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![alias.to_string()]);
    resumed.codex.flush_rollout().await?;
    let persisted = resumed
        .thread_store
        .find_persisted_input_ids(FindPersistedInputIdsParams {
            thread_id: root,
            ids: vec![
                PersistedInputId::ResponseItem(legacy_id.clone()),
                PersistedInputId::ResponseItem(alias),
            ],
        })
        .await?;
    assert_eq!(
        persisted,
        BTreeSet::from([PersistedInputId::ResponseItem(legacy_id)])
    );
    assert_eq!(service.get(run_id).map_err(anyhow::Error::msg)?, expected);
    resumed.codex.shutdown_and_wait().await?;
    Ok(())
}
