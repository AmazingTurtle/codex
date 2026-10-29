//! Native workflow tasks retain their full source while model requests use bounded parts.

use anyhow::Context;
use anyhow::Result;
use codex_core::workflows::WorkflowStatus;
use codex_history::RolloutItem;
use codex_protocol::AgentPath;
use codex_protocol::ResponseItemId;
use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecutionControlState;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::turn_input::TurnInputSubmission;
use codex_protocol::user_input::UserInput;
use codex_thread_store::LoadThreadHistoryParams;
use codex_thread_store::ReadThreadParams;
use core_test_support::responses;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeSet;
use std::time::Duration;
use test_case::test_case;

use super::workflow_revision::builder;
use super::workflow_revision::observe;
use super::workflow_revision::source;

const TASK_BYTES: usize = 9914;
const OPTIONS_BYTES: usize = 10853;
const SOURCE_KIND: &str = "dynamic_workflows.task.v1";
const PART_KIND: &str = "dynamic_workflows.task_part.v1";
const SENTINEL: &str = "INTENTIONAL_FAILURE_AFTER_THE_ACCEPTED_NATIVE_RESULT";

#[derive(Clone, Copy)]
enum TaskText {
    Ascii,
    Utf8AndControls,
}

fn marked(item: &Value, kind: &str) -> bool {
    item["internal_chat_message_metadata_passthrough"]["content_item_kinds"]
        .as_array()
        .is_some_and(|kinds| kinds.iter().any(|value| value == kind))
}

fn task_text(kind: TaskText) -> String {
    let mut text = "FULL_WORKFLOW_TASK_BEGIN\n".to_owned();
    if matches!(kind, TaskText::Utf8AndControls) {
        text.push_str("\0\u{001f}\u{001b}\t\r\n\\\"UTF8_BOUNDARY\n");
    }
    let end = "\nFULL_WORKFLOW_TASK_END";
    let pattern = match kind {
        TaskText::Ascii => "review ",
        TaskText::Utf8AndControls => "é🧪",
    };
    while text.len() + pattern.len() + end.len() <= TASK_BYTES {
        text.push_str(pattern);
    }
    text.push_str(&"x".repeat(TASK_BYTES - text.len() - end.len()));
    text.push_str(end);
    assert_eq!(text.len(), TASK_BYTES);
    text
}

fn projected_parts(body: &Value) -> Result<(Vec<Value>, String)> {
    let parts = body["input"]
        .as_array()
        .context("actual outbound native model input")?
        .iter()
        .filter(|item| {
            item["type"] == "agent_message"
                && item["content"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("Workflow task part "))
        })
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        (2..=8).contains(&parts.len()),
        "an oversized task must become bounded separate typed items"
    );
    let mut ids = BTreeSet::new();
    let mut full = String::new();
    for (index, part) in parts.iter().enumerate() {
        assert!(
            serde_json::to_vec(part)?.len() <= 8192,
            "each complete serialized typed part must remain bounded"
        );
        let id = part["id"]
            .as_str()
            .context("stable transport part identity")?;
        let identity = uuid::Uuid::parse_str(
            id.strip_prefix("amsg_")
                .context("AgentMessage transport IDs must use amsg_")?,
        )?;
        assert_eq!(identity.get_version(), Some(uuid::Version::Sha1));
        assert!(id.len() <= 64 && ids.insert(id.to_owned()));
        let content = part["content"]
            .as_array()
            .context("one independently bounded InputText per part")?;
        assert_eq!(
            content.len(),
            1,
            "a vector of text fields in one unbounded item is not transport splitting"
        );
        assert_eq!(content[0]["type"], "input_text");
        let header = format!(
            "Workflow task part {}/{}.\nTask name: {}\nSender: {}\nRead every part in order as one task.\n\n",
            index + 1,
            parts.len(),
            part["recipient"]
                .as_str()
                .context("part recipient attribution")?,
            part["author"].as_str().context("part sender attribution")?
        );
        full.push_str(
            content[0]["text"]
                .as_str()
                .context("the exact UTF-8 part text")?
                .strip_prefix(&header)
                .context("parts must retain their explicit ordered task wrapper")?,
        );
    }
    Ok((parts, full))
}

pub(super) fn native_task_text(body: &Value, original_prompt: &str) -> Result<String> {
    let items = body["input"]
        .as_array()
        .context("the actual native provider request")?;
    if items.iter().any(|item| {
        item["type"] == "agent_message"
            && item["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.starts_with("Workflow task part "))
    }) {
        return Ok(projected_parts(body)?.1);
    }
    let whole = items
        .iter()
        .filter(|item| {
            item["type"] == "agent_message"
                && item["content"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(original_prompt))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        whole.len(),
        1,
        "one unpartitioned native task must retain its original prompt exactly once"
    );
    assert!(
        serde_json::to_vec(whole[0])?.len() <= 8192,
        "an unpartitioned task must obey the same complete-item bound"
    );
    assert_eq!(
        whole[0]["content"]
            .as_array()
            .context("unpartitioned native input")?
            .len(),
        1
    );
    Ok(whole[0]["content"][0]["text"]
        .as_str()
        .context("the complete unpartitioned task")?
        .to_owned())
}

#[test_case(TaskText::Ascii; "reported_9914_byte_task_is_lossless_and_cache_stable")]
#[test_case(TaskText::Utf8AndControls; "utf8_and_control_boundaries_survive_native_pause_and_cold_replay")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_task_uses_one_native_admission_and_request_only_stable_parts(
    kind: TaskText,
) -> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let task = task_text(kind);
    let mut options = json!({"key":"oversized","label":"oversized","model":"gpt-5.6-sol","reasoningEffort":"low",
        "schema":{"type":"object","description":"","properties":{"ok":{"const":true}},"required":["ok"],"additionalProperties":false}});
    let mut normalized = options.clone();
    normalized["message"] = json!(task);
    let padding = OPTIONS_BYTES
        .checked_sub(serde_json::to_vec(&normalized)?.len())
        .context("the real task and valid schema fit the reported options size")?;
    options["schema"]["description"] = json!("s".repeat(padding));
    normalized = options.clone();
    normalized["message"] = json!(task);
    assert_eq!(serde_json::to_vec(&normalized)?.len(), OPTIONS_BYTES);
    let script = source(&format!(
        "await workflow.agent({},{}); throw new Error('{}');",
        serde_json::to_string(&task)?,
        options,
        SENTINEL
    ));
    let (release_first, first_gate) = tokio::sync::oneshot::channel();
    let (release_second, second_gate) = tokio::sync::oneshot::channel();
    let (provider, _) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("first-native")]),
            },
            StreamingSseChunk {
                gate: Some(first_gate),
                body: responses::sse(vec![responses::ev_completed("first-native")]),
            },
        ],
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("resumed-native")]),
            },
            StreamingSseChunk {
                gate: Some(second_gate),
                body: responses::sse(vec![
                    responses::ev_assistant_message("msg_native_result", r#"{"ok":true}"#),
                    responses::ev_completed_with_tokens("resumed-native", /*total_tokens*/ 5),
                ]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_assistant_message(
                    "msg_cold_human",
                    "The restored conversation retains every task part.",
                ),
                responses::ev_completed_with_tokens("cold-human", /*total_tokens*/ 7),
            ]),
        }],
    ])
    .await;
    let bootstrap = responses::start_mock_server().await;
    let endpoint = format!("{}/v1", provider.uri());
    let initial_url = endpoint.clone();
    let mut extensions =
        codex_extension_api::ExtensionRegistryBuilder::<codex_core::config::Config>::new();
    extensions.thread_lifecycle_contributor(std::sync::Arc::new(core_test_support::ThreadIdle));
    let extensions = std::sync::Arc::new(extensions.build());
    let mut initial = builder()
        .with_extensions(extensions.clone())
        .with_config(move |config| {
            config.model_provider.base_url = Some(initial_url);
            config.model_provider.supports_websockets = false;
            config
                .features
                .enable(codex_features::Feature::ContentItemKinds)
                .expect(
                    "retain actual native transport metadata in the controlled provider request",
                );
        })
        .build_with_auto_env(&bootstrap)
        .await?;
    initial.codex.ensure_rollout_materialized().await;
    let root = initial.session_configured.thread_id;
    initial.session_configured.rollout_path = initial
        .thread_store
        .read_thread(ReadThreadParams {
            thread_id: root,
            include_archived: true,
            include_history: false,
        })
        .await?
        .rollout_path;
    let started = initial
        .codex
        .start_workflow(script.clone(), json!({}))
        .await
        .map_err(anyhow::Error::msg)?;
    let service = initial
        .codex
        .workflows()
        .await
        .map_err(anyhow::Error::msg)?;
    let admission = observe(&service, &started.id, |run| {
        run.status == WorkflowStatus::Failed || run.accepted_starts == 1
    })
    .await?;
    assert_eq!(
        admission.accepted_starts, 1,
        "the reported valid 10853-byte options must reach native admission: {:?}",
        admission.error
    );
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        provider.wait_for_request_count(/*count*/ 1),
    )
    .await?;
    let sampled: Value = serde_json::from_slice(&provider.requests().await[0])?;
    let (first_parts, reassembled) = projected_parts(&sampled)?;
    let step = admission
        .steps
        .values()
        .find(|step| step.label == "oversized")
        .context("the single accepted full-size logical task")?;
    assert_eq!(
        step.options, normalized,
        "transport cannot change the original options or call identity"
    );
    assert_eq!(step.attempts.len(), 1);
    let child_id = ThreadId::from_string(
        step.attempts[0]
            .child_thread_id
            .as_deref()
            .context("the real accepted native child")?,
    )?;
    let child = initial.thread_manager.get_thread(child_id).await?;
    assert_eq!(
        initial.codex.pause_execution().await?.state,
        ExecutionControlState::Paused
    );
    child.flush_rollout().await?;
    let history = initial
        .thread_store
        .load_latest_model_context(LoadThreadHistoryParams {
            thread_id: child_id,
            include_archived: true,
        })
        .await?;
    let originals = history
        .items
        .iter()
        .filter_map(|item| match item {
            RolloutItem::ResponseItem(envelope)
                if marked(
                    &serde_json::to_value(&envelope.item)
                        .expect("serialize the actual persisted item"),
                    SOURCE_KIND,
                ) =>
            {
                Some(envelope.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        originals.len(),
        1,
        "persistence must retain one original full task, never the projected parts"
    );
    let original = serde_json::to_value(&originals[0].item)?;
    assert_eq!(original["type"], "agent_message");
    let original_text = original["content"][0]["text"]
        .as_str()
        .context("the complete original native attribution envelope")?;
    assert_eq!(
        reassembled, original_text,
        "ordered transport pieces must reconstruct every original UTF-8/control byte"
    );
    assert!(
        original_text.contains(&task),
        "the actual durable attributed native task must contain all 9914 authored bytes"
    );
    assert!(first_parts.iter().all(|part| part["id"] != original["id"]
        && part["author"] == original["author"]
        && part["recipient"] == original["recipient"]));
    assert!(!history.items.iter().any(|item|matches!(item,RolloutItem::ResponseItem(envelope) if marked(&serde_json::to_value(&envelope.item).expect("persisted item JSON"),PART_KIND))));
    assert_eq!(provider.requests().await.len(), 1);
    let _ = release_first.send(());
    initial.codex.unpause_execution().await?;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        provider.wait_for_request_count(/*count*/ 2),
    )
    .await?;
    let resampled: Value = serde_json::from_slice(&provider.requests().await[1])?;
    let (resumed_parts, resumed_text) = projected_parts(&resampled)?;
    assert_eq!(
        (&resumed_parts, &resumed_text),
        (&first_parts, &reassembled)
    );
    release_second
        .send(())
        .expect("finish the same resumed native turn with its valid schema output");
    let failed = observe(&service, &started.id, |run| {
        run.status == WorkflowStatus::Failed && run.lifecycle.terminal_ready
    })
    .await?;
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains(SENTINEL)),
        "{:?}",
        failed.error
    );
    let accepted = failed
        .steps
        .values()
        .next()
        .context("the completed native task before the deliberate VM error")?
        .clone();
    assert_eq!(accepted.result, Some(json!({"ok":true})));
    assert_eq!(accepted.attempts[0].status, WorkflowStatus::Succeeded);
    assert_eq!(failed.accepted_starts, 1);
    let retried = service
        .retry(&started.id)
        .await
        .map_err(anyhow::Error::msg)?;
    let cached = observe(&service, &started.id, |run| {
        run.execution_id != failed.execution_id
            && run.status == WorkflowStatus::Failed
            && run.lifecycle.terminal_ready
    })
    .await?;
    assert_eq!(cached.execution_id, retried.execution_id);
    assert_eq!(cached.steps.values().next(), Some(&accepted));
    assert_eq!(cached.accepted_starts, 1);
    assert_eq!(cached.source, script);
    assert_eq!(
        provider.requests().await.len(),
        2,
        "a native-success prefix must reuse its exact cached call on real retry"
    );
    core_test_support::ThreadIdle::wait(&child).await;
    let shutdown = initial
        .thread_manager
        .request_agent_tree_shutdown(root)
        .await?;
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30), shutdown.wait()).await??;
    drop(service);
    let resumed = builder()
        .with_extensions(extensions)
        .with_config(move |config| {
            config.model_provider.base_url = Some(endpoint);
            config.model_provider.supports_websockets = false;
            config
                .features
                .enable(codex_features::Feature::ContentItemKinds)
                .expect("preserve the same request metadata contract after cold restore");
        })
        .restart_with_auto_env(&bootstrap, &initial)
        .await?;
    resumed
        .thread_manager
        .ensure_multi_agent_v2_child_loaded(child_id)
        .await?;
    let restored_child = resumed.thread_manager.get_thread(child_id).await?;
    assert_eq!(
        provider.requests().await.len(),
        2,
        "cold load must not resubmit or split the native admission"
    );
    let ordinary_id = ResponseItemId::with_suffix("amsg", uuid::Uuid::new_v4());
    let mut ordinary = InterAgentCommunication::new(
        AgentPath::root(),
        AgentPath::from_string(
            original["recipient"]
                .as_str()
                .context("native recipient path")?
                .to_owned(),
        )
        .map_err(anyhow::Error::msg)?,
        Vec::new(),
        "ORDINARY_UNMARKED_MAIL_REMAINS_WHOLE".into(),
        /*trigger_turn*/ false,
    );
    ordinary.id = Some(ordinary_id.clone());
    restored_child
        .submit(Op::InterAgentCommunication {
            communication: ordinary,
            start_options: Default::default(),
        })
        .await?;
    restored_child
        .update_thread_settings(ThreadSettingsOverrides::default())
        .await?;
    let submitted = restored_child
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Continue this ordinary conversation after restoring its history.".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let TurnInputSubmission::Started {
        turn_id: human_turn,
        ..
    } = submitted
    else {
        anyhow::bail!("the cold native child must start an ordinary new turn: {submitted:?}");
    };
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30),core_test_support::wait_for_event(&restored_child,|event|
        matches!(event,EventMsg::TurnComplete(done) if done.turn_id==human_turn && done.error.is_none()
            && done.last_agent_message.as_deref()==Some("The restored conversation retains every task part."))
    )).await?;
    let requests = provider.requests().await;
    assert_eq!(requests.len(), 3);
    let cold_body: Value = serde_json::from_slice(&requests[2])?;
    let (cold_parts, cold_text) = projected_parts(&cold_body)?;
    assert_eq!(
        (cold_parts, cold_text),
        (first_parts, reassembled),
        "cold context hydration must keep identical ordered part identities and payloads"
    );
    restored_child.flush_rollout().await?;
    let stored = resumed
        .thread_store
        .load_latest_model_context(LoadThreadHistoryParams {
            thread_id: child_id,
            include_archived: true,
        })
        .await?;
    let retained = stored
        .items
        .iter()
        .filter_map(|item| match item {
            RolloutItem::ResponseItem(envelope) if envelope.item.id() == originals[0].item.id() => {
                Some(envelope.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(retained, originals);
    let plain = stored
        .items
        .iter()
        .find_map(|item| match item {
            RolloutItem::ResponseItem(envelope) if envelope.item.id() == Some(&ordinary_id) => {
                Some(serde_json::to_value(&envelope.item).expect("actual stored ordinary mail"))
            }
            _ => None,
        })
        .context("the actual ordinary incoming AgentMessage")?;
    let outgoing_plain = cold_body["input"]
        .as_array()
        .context("cold native prompt")?
        .iter()
        .filter(|item| item["id"] == ordinary_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        outgoing_plain,
        vec![&plain],
        "unmarked ordinary AgentMessages must remain byte-for-byte request values"
    );
    assert!(!marked(&plain, SOURCE_KIND) && !marked(&plain, PART_KIND));
    assert!(
        responses::received_responses_requests(&bootstrap)
            .await
            .is_empty()
    );
    let shutdown = resumed
        .thread_manager
        .request_agent_tree_shutdown(root)
        .await?;
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30), shutdown.wait()).await??;
    provider.shutdown().await;
    Ok(())
}
