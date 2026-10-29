//! Real workflow lifecycle coverage. Only the external model SSE boundary is simulated;
//! V8, admission, child controllers, profiles, journals and thread stores are production code.

use anyhow::Context;
use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_core::config::AgentRoleConfig;
use codex_core::workflows::DynamicWorkflowService;
use codex_core::workflows::WorkflowRun;
use codex_core::workflows::WorkflowStatus;
use codex_core::workflows::WorkflowStep;
use codex_features::DynamicWorkflowsMode;
use codex_features::Feature;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecutionControlState;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::ResponsesRequest;
use core_test_support::skip_if_no_network;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use test_case::test_case;
use tokio::sync::broadcast;
use tokio::sync::oneshot;
use wiremock::matchers::header;

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
                    .expect("enable workflow lifecycle feature");
            }
            config.multi_agent_v2.non_code_mode_only = false;
            config.multi_agent_v2.tool_namespace = Some("lifecycle_agents".into());
            config.multi_agent_v2.max_concurrent_threads_per_session = 4;
            config.dynamic_workflows_mode = DynamicWorkflowsMode::Auto;
        })
}

fn answer(id: &str, text: &str) -> String {
    responses::sse(vec![
        responses::ev_response_created(id),
        responses::ev_assistant_message(id, text),
        responses::ev_completed(id),
    ])
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30), future)
        .await
        .expect("workflow lifecycle timed out")
}

fn brief(run: &WorkflowRun) -> String {
    let error = run
        .error
        .as_deref()
        .map(|error| error.chars().take(/*n*/ 512).collect::<String>());
    let steps = run
        .steps
        .values()
        .take(/*n*/ 8)
        .map(|step| {
            (
                step.id.chars().take(/*n*/ 128).collect::<String>(),
                step.attempts.last().map(|attempt| {
                    (
                        attempt.status,
                        attempt
                            .child_thread_id
                            .as_deref()
                            .map(|id| id.chars().take(/*n*/ 128).collect::<String>()),
                    )
                }),
            )
        })
        .collect::<Vec<_>>();
    format!(
        "status={:?}, phase={:?}, error={error:?}, steps={steps:?}",
        run.status, run.phase
    )
}

async fn lifecycle_timeline(test: &TestCodex, thread_id: ThreadId) -> Result<Vec<Value>> {
    let history = test
        .thread_store
        .load_latest_model_context(codex_thread_store::LoadThreadHistoryParams {
            thread_id,
            include_archived: true,
        })
        .await?;
    let mut timeline = history.items.iter().rev().filter_map(|item| match item {
        codex_history::RolloutItem::EventMsg(EventMsg::TurnStarted(event)) => Some(json!({"kind":"started","turnId":event.turn_id})),
        codex_history::RolloutItem::EventMsg(EventMsg::TurnComplete(event)) => Some(json!({"kind":"completed","turnId":event.turn_id,"hasError":event.error.is_some()})),
        codex_history::RolloutItem::EventMsg(EventMsg::TurnAborted(event)) => Some(json!({"kind":"aborted","turnId":event.turn_id,"reason":event.reason})),
        codex_history::RolloutItem::TokenUsageRecord(record) => Some(json!({"kind":"usage","turnId":record.turn_id,"ownTotalTokens":record.thread_token_usage.total_tokens})),
        codex_history::RolloutItem::Compacted(_) => Some(json!({"kind":"compacted"})),
        _ => None,
    }).take(/*n*/ 16).collect::<Vec<_>>();
    timeline.reverse();
    Ok(timeline)
}

async fn observe(
    service: &DynamicWorkflowService,
    events: &mut broadcast::Receiver<WorkflowRun>,
    id: &str,
    expected: impl Fn(&WorkflowRun) -> bool,
) -> Result<WorkflowRun> {
    let observation = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        loop {
            let run = service.get(id).map_err(anyhow::Error::msg)?;
            if expected(&run) { return Ok(run); }
            // Delivery acknowledgement follows execution completion asynchronously.
            let awaiting_delivery = run.status == WorkflowStatus::Succeeded
                && (!run.notification_delivered || !run.lifecycle.terminal_ready
                    || run.lifecycle.delivered.len()!=run.lifecycle.events.len());
            if matches!(run.status, WorkflowStatus::Succeeded | WorkflowStatus::Failed | WorkflowStatus::Stopped | WorkflowStatus::Interrupted)
                && !awaiting_delivery
            {
                anyhow::bail!("workflow {id} reached a terminal state before the expected lifecycle condition: {}", brief(&run));
            }
            match events.recv().await {
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }).await;
    match observation {
        Ok(result) => result,
        Err(_) => {
            let current = service
                .get(id)
                .map(|run| brief(&run))
                .unwrap_or_else(|error| error.chars().take(/*n*/ 512).collect());
            anyhow::bail!("workflow {id} lifecycle observation timed out: {current}");
        }
    }
}

async fn requested(
    server: &wiremock::MockServer,
    matches: impl Fn(&ResponsesRequest) -> bool,
) -> ResponsesRequest {
    bounded(async {
        let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
        loop {
            if let Some(request) = responses::received_responses_requests(server)
                .await
                .into_iter()
                .find(&matches)
            {
                return request;
            }
            ticks.tick().await;
        }
    })
    .await
}

fn from_thread(request: &ResponsesRequest, thread: ThreadId) -> bool {
    request.header("thread-id").as_deref() == Some(thread.to_string().as_str())
}

fn body(request: &wiremock::Request) -> Value {
    let bytes = if request
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("zstd"))
        }) {
        zstd::stream::decode_all(std::io::Cursor::new(&request.body))
            .expect("decode captured request")
    } else {
        request.body.clone()
    };
    serde_json::from_slice(&bytes).expect("captured model request JSON")
}

fn contains(request: &wiremock::Request, text: &str) -> bool {
    body(request)["input"].to_string().contains(text)
}

fn declares_tool(tools: &[Value], name: &str) -> bool {
    tools.iter().any(|tool| {
        tool.get("name").and_then(Value::as_str) == Some(name)
            || tool
                .get("tools")
                .and_then(Value::as_array)
                .is_some_and(|nested| declares_tool(nested, name))
    })
}

fn step<'a>(run: &'a WorkflowRun, label: &str) -> &'a WorkflowStep {
    run.steps
        .values()
        .find(|step| step.label == label)
        .expect("the workflow retains its authored step label")
}

fn child_id(run: &WorkflowRun, label: &str) -> Result<ThreadId> {
    ThreadId::from_string(
        step(run, label)
            .attempts
            .last()
            .and_then(|attempt| attempt.child_thread_id.as_deref())
            .context("accepted child id")?,
    )
    .map_err(Into::into)
}

fn spawned_path(request: &ResponsesRequest, call_id: &str) -> Result<AgentPath> {
    let (output, _) = request
        .function_call_output_content_and_success(call_id)
        .context("native spawn must return its accepted task identity")?;
    let output = output.context("native spawn output text")?;
    let result: Value = serde_json::from_str(&output)
        .with_context(|| format!("native spawn did not return accepted task JSON: {output}"))?;
    let path = result["task_name"]
        .as_str()
        .context("native spawn must return an accepted task_name")?;
    AgentPath::from_string(path.to_owned()).map_err(anyhow::Error::msg)
}

struct HeldToolCompletion {
    reached: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

struct AccountingToolGates(
    std::sync::Mutex<std::collections::BTreeMap<String, HeldToolCompletion>>,
);

impl codex_extension_api::ToolLifecycleContributor for AccountingToolGates {
    fn on_tool_finish<'a>(
        &'a self,
        input: codex_extension_api::ToolFinishInput<'a>,
    ) -> codex_extension_api::ToolLifecycleFuture<'a> {
        Box::pin(async move {
            let gate = self
                .0
                .lock()
                .expect("accounting tool gates")
                .remove(input.call_id);
            if let Some(gate) = gate {
                gate.reached
                    .send(())
                    .expect("observe a real held tool completion");
                gate.release
                    .await
                    .expect("release that registered tool completion");
            }
        })
    }
}

#[derive(Clone, Copy)]
enum AccountingFinish {
    Live,
    ColdRestart,
    ColdLoaded,
}

#[test_case(AccountingFinish::Live; "live_owned_descendant_completion")]
#[test_case(AccountingFinish::ColdRestart; "cold_unloaded_recovery_keeps_only_proven_usage")]
#[test_case(AccountingFinish::ColdLoaded; "cold_loaded_completed_child_keeps_only_proven_usage")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_worker_followup_usage_is_excluded_while_its_owned_descendant_finishes(
    finish: AccountingFinish,
) -> Result<()> {
    use codex_history::RolloutItem;
    use codex_thread_store::LoadThreadHistoryParams;

    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let (descendant_reached, descendant_waiting) = oneshot::channel();
    let (release_descendant, descendant_gate) = oneshot::channel();
    let (human_reached, human_waiting) = oneshot::channel();
    let (release_human, human_gate) = oneshot::channel();
    let mut extensions =
        codex_extension_api::ExtensionRegistryBuilder::<codex_core::config::Config>::new();
    extensions.thread_lifecycle_contributor(Arc::new(core_test_support::ThreadIdle));
    extensions.tool_lifecycle_contributor(Arc::new(AccountingToolGates(std::sync::Mutex::new(
        std::collections::BTreeMap::from([
            (
                "owned-accounting-tool".into(),
                HeldToolCompletion {
                    reached: descendant_reached,
                    release: descendant_gate,
                },
            ),
            (
                "later-human-tool".into(),
                HeldToolCompletion {
                    reached: human_reached,
                    release: human_gate,
                },
            ),
        ]),
    ))));
    let test = builder().with_extensions(Arc::new(extensions.build()))
        .with_model_info_override("gpt-5.6-sol", |model| model.experimental_supported_tools.push("test_sync_tool".into()))
        .with_pre_build_hook(|home| {
            std::fs::write(home.join("accounting-coordinator.config.toml"), "model_reasoning_effort = 'low'\n[features]\nwake_on_agent_messages = false\n[features.multi_agent_v2]\nenabled = true\nnon_code_mode_only = false\ntool_namespace = 'accounting_agents'\n")
                .expect("write the native coordinator's actual profile");
        }).with_config(|config| {
            config.features.disable(Feature::WakeOnAgentMessages).expect("descendant reports do not manufacture extra inference");
            config.agent_roles.insert("accounting-coordinator".into(), AgentRoleConfig {
                profile:Some("accounting-coordinator".parse().expect("coordinator profile")), description:Some("Coordinate an owned native descendant".into()),
                config_file:None, nickname_candidates:None,
            });
        }).build_with_auto_env(&server).await?;
    let root = test.session_configured.thread_id.to_string();
    let parent = root.clone();
    responses::mount_sse_once_match(&server, move |request: &wiremock::Request|
        body(request)["client_metadata"]["x-codex-parent-thread-id"] == json!(parent), responses::sse(vec![
            responses::ev_function_call_with_namespace("spawn-owned", "accounting_agents", "spawn_agent", &json!({
                "task_name":"owned_descendant", "message":"Owned accounting descendant must finish its normal tool.", "fork_turns":"none",
            }).to_string()), responses::ev_completed_with_tokens("coordinator-spawn", /*total_tokens*/ 3),
        ])).await;
    let parent = root.clone();
    responses::mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            body(request)["client_metadata"]["x-codex-parent-thread-id"] == json!(parent)
        },
        responses::sse(vec![
            responses::ev_assistant_message("original-result", r#"{"owned":true}"#),
            responses::ev_completed_with_tokens("original-result", /*total_tokens*/ 5),
        ]),
    )
    .await;
    let parent = root.clone();
    responses::mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            body(request)["client_metadata"]["x-codex-parent-thread-id"] != json!(parent)
                && contains(
                    request,
                    "Owned accounting descendant must finish its normal tool.",
                )
        },
        responses::sse(vec![
            responses::ev_function_call("owned-accounting-tool", "test_sync_tool", "{}"),
            responses::ev_completed_with_tokens("descendant-tool", /*total_tokens*/ 7),
        ]),
    )
    .await;
    test.codex.ensure_rollout_materialized().await;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut created = test.thread_manager.subscribe_thread_created();
    let run = service.start(super::workflow_v2::script("return await workflow.agent('Create the owned accounting descendant and return JSON.',{label:'coordinator',agentType:'accounting-coordinator'});"), json!({})).await.map_err(anyhow::Error::msg)?;
    let spawn_request = requested(&server, |request| {
        request
            .inputs_of_type("function_call_output")
            .iter()
            .any(|item| item["call_id"] == "spawn-owned")
    })
    .await;
    let accepted_path = spawned_path(&spawn_request, "spawn-owned")?;
    let registered_coordinator = bounded(created.recv()).await?;
    let registered_descendant = bounded(created.recv()).await?;
    let registered = test
        .thread_manager
        .get_thread(registered_descendant)
        .await?;
    assert_eq!(
        registered
            .config_snapshot()
            .await
            .session_source
            .get_agent_path(),
        Some(accepted_path)
    );
    match tokio::time::timeout(Duration::from_secs(/*secs*/ 30), descendant_waiting).await {
        Ok(reached) => reached?,
        Err(error) => {
            let current = service
                .get(&run.id)
                .map(|run| brief(&run))
                .unwrap_or_else(|error| error);
            let requests = responses::received_responses_requests(&server).await;
            let diagnostics = requests
                .iter()
                .take(/*n*/ 8)
                .map(|request| {
                    let body = request.body_json();
                    let mut tools = vec![(String::new(), body["tools"].clone())];
                    tools.extend(
                        request
                            .inputs_of_type("additional_tools")
                            .into_iter()
                            .map(|item| (String::new(), item["tools"].clone())),
                    );
                    let mut advertised = Vec::new();
                    while let Some((namespace, value)) = tools.pop() {
                        if let Some(items) = value.as_array() {
                            tools.extend(
                                items.iter().cloned().map(|item| (namespace.clone(), item)),
                            );
                        } else {
                            let name = value["name"].as_str().unwrap_or_default();
                            if matches!(name, "spawn_agent" | "test_sync_tool") {
                                advertised.push(format!("{namespace}.{name}"));
                            }
                            if let Some(nested) = value.get("tools") {
                                tools.push((name.to_owned(), nested.clone()));
                            }
                        }
                    }
                    let spawn_outputs = request
                        .inputs_of_type("function_call_output")
                        .into_iter()
                        .filter(|item| item["call_id"] == "spawn-owned")
                        .map(|item| {
                            item["output"]
                                .to_string()
                                .chars()
                                .take(/*n*/ 512)
                                .collect::<String>()
                        })
                        .collect::<Vec<_>>();
                    (
                        request.header("thread-id"),
                        body["client_metadata"]["x-codex-parent-thread-id"].clone(),
                        body["model"].clone(),
                        advertised,
                        spawn_outputs,
                    )
                })
                .collect::<Vec<_>>();
            return Err(anyhow::Error::new(error).context(format!(
                "owned descendant never reached its registered tool completion; {current}; requests(thread,parent,model,advertised,spawn_output)={diagnostics:?}"
            )));
        }
    }
    let waiting = super::workflow_v2::observe(&service, &run.id, |run| {
        run.steps
            .values()
            .any(|step| step.label == "coordinator" && step.result == Some(json!({"owned":true})))
    })
    .await?;
    assert_eq!(
        waiting.status,
        WorkflowStatus::Running,
        "workflow finalization must wait for the owned descendant"
    );
    let coordinator_id = child_id(&waiting, "coordinator")?;
    assert_eq!(coordinator_id, registered_coordinator);
    let coordinator = test.thread_manager.get_thread(coordinator_id).await?;
    bounded(wait_for_event(&coordinator, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    }))
    .await;
    core_test_support::ThreadIdle::wait(&coordinator).await;
    let descendant_request = requested(&server, |request| {
        request.body_json()["client_metadata"]["x-codex-parent-thread-id"] == json!(coordinator_id)
            && request
                .body_contains_text("Owned accounting descendant must finish its normal tool.")
    })
    .await;
    let descendant_id = ThreadId::from_string(
        &descendant_request
            .header("thread-id")
            .context("owned descendant thread")?,
    )?;
    assert_eq!(descendant_id, registered_descendant);
    let descendant = test.thread_manager.get_thread(descendant_id).await?;
    assert_eq!(descendant.agent_status().await, AgentStatus::Running);
    responses::mount_sse_once_match(
        &server,
        header("thread-id", coordinator_id.to_string()),
        responses::sse(vec![
            responses::ev_function_call("later-human-tool", "test_sync_tool", "{}"),
            responses::ev_completed_with_tokens("later-human-tool", /*total_tokens*/ 23),
        ]),
    )
    .await;
    let human_completion = responses::mount_sse_once_match(
        &server,
        header("thread-id", coordinator_id.to_string()),
        responses::sse(vec![
            responses::ev_assistant_message(
                "human-followup-result",
                "The independent follow-up is complete.",
            ),
            responses::ev_completed_with_tokens("human-followup-result", /*total_tokens*/ 29),
        ]),
    )
    .await;
    coordinator.start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
        text:"Perform this independent human follow-up after the original workflow worker completed.".into(), text_elements:Vec::new(),
    }])).await?;
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30), human_waiting)
        .await
        .context("independent human follow-up never reached its registered tool completion")??;
    assert_eq!(coordinator.agent_status().await, AgentStatus::Running);
    assert_eq!(
        service.get(&run.id).map_err(anyhow::Error::msg)?.status,
        WorkflowStatus::Running
    );
    if matches!(
        finish,
        AccountingFinish::ColdRestart | AccountingFinish::ColdLoaded
    ) {
        release_human
            .send(())
            .expect("finish the unrelated human turn before the cold restart");
        bounded(wait_for_event(&coordinator, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        }))
        .await;
        core_test_support::ThreadIdle::wait(&coordinator).await;
        human_completion.single_request();
        coordinator.flush_rollout().await?;
        let history = test
            .thread_store
            .load_latest_model_context(LoadThreadHistoryParams {
                thread_id: coordinator_id,
                include_archived: true,
            })
            .await?;
        let own = history
            .items
            .iter()
            .rev()
            .find_map(|item| match item {
                RolloutItem::TokenUsageRecord(record) if record.thread_id == coordinator_id => {
                    Some(&record.thread_token_usage)
                }
                _ => None,
            })
            .context("the later independent human turn must really be durable")?;
        assert_eq!(own.total_tokens, 60);
        let proven = super::workflow_v2::observe(&service, &run.id, |run| {
            run.metrics
                .usage
                .as_ref()
                .is_some_and(|usage| usage.total_tokens == 15)
        })
        .await?;
        assert_eq!(proven.status, WorkflowStatus::Running);
        let original_attempts = step(&proven, "coordinator").attempts.clone();
        let shutdown = test
            .thread_manager
            .request_agent_tree_shutdown(test.session_configured.thread_id)
            .await?;
        bounded(shutdown.wait()).await?;
        drop(service);
        let resumed = builder()
            .with_model_info_override("gpt-5.6-sol", |model| {
                model
                    .experimental_supported_tools
                    .push("test_sync_tool".into())
            })
            .with_config(|config| {
                config
                    .features
                    .disable(Feature::WakeOnAgentMessages)
                    .expect("recovery reports do not wake model turns");
                config.agent_roles.insert(
                    "accounting-coordinator".into(),
                    AgentRoleConfig {
                        profile: Some(
                            "accounting-coordinator"
                                .parse()
                                .expect("coordinator profile"),
                        ),
                        description: Some("Coordinate an owned native descendant".into()),
                        config_file: None,
                        nickname_candidates: None,
                    },
                );
            })
            .restart_with_auto_env(&server, &test)
            .await?;
        assert_eq!(
            resumed.thread_manager.list_thread_ids().await,
            vec![resumed.session_configured.thread_id],
            "cold startup must leave both contributors unloaded until an explicit child resume"
        );
        let mut expected_loaded =
            std::collections::HashSet::from([resumed.session_configured.thread_id]);
        if matches!(finish, AccountingFinish::ColdLoaded) {
            bounded(
                resumed
                    .thread_manager
                    .ensure_multi_agent_v2_child_loaded(coordinator_id),
            )
            .await?;
            let loaded = resumed.thread_manager.get_thread(coordinator_id).await?;
            assert_eq!(
                loaded.agent_status().await,
                AgentStatus::PendingInit,
                "a genuinely resumed completed child starts idle with PendingInit, without a new turn"
            );
            expected_loaded.insert(coordinator_id);
        }
        let service = resumed
            .codex
            .workflows()
            .await
            .map_err(anyhow::Error::msg)?;
        let interrupted = service.get(&run.id).map_err(anyhow::Error::msg)?;
        assert_eq!(interrupted.status, WorkflowStatus::Interrupted);
        assert_eq!(interrupted.metrics.model_turns, Some(3));
        assert_eq!(
            interrupted
                .metrics
                .usage
                .as_ref()
                .map(|usage| usage.total_tokens),
            Some(15)
        );
        service.recover(&run.id).await.map_err(anyhow::Error::msg)?;
        let recovered = super::workflow_v2::observe(&service, &run.id, |run| {
            run.status == WorkflowStatus::Succeeded
        })
        .await?;
        assert_eq!(
            recovered.result,
            Some(json!({"owned":true})),
            "the cached authoritative worker result must survive its later unrelated result"
        );
        assert_eq!(recovered.accepted_starts, 1);
        assert_eq!(
            recovered.metrics.model_turns,
            Some(3),
            "cold recovery preserves the proven response highwater and excludes later human responses"
        );
        assert_eq!(
            recovered
                .metrics
                .usage
                .as_ref()
                .map(|usage| usage.total_tokens),
            Some(15),
            "cold recovery may retain proven original usage (8+7), never the unrelated latest cumulative usage (60+7)"
        );
        assert!(
            !recovered.metrics.usage_complete,
            "unproven cold contributor ownership must remain explicitly incomplete"
        );
        assert!(
            recovered
                .metrics
                .usage_incomplete_reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty())
        );
        // Shutdown learns why the still-owned descendant's usage is partial.
        // Cached recovery preserves every original attempt fact and known
        // counter while carrying that newly established incomplete reason.
        let mut expected_attempts = original_attempts;
        for attempt in &mut expected_attempts {
            assert!(!attempt.metrics.usage_complete);
            attempt.metrics.usage_incomplete_reason =
                recovered.metrics.usage_incomplete_reason.clone();
        }
        assert_eq!(step(&recovered, "coordinator").attempts, expected_attempts);
        assert_eq!(
            resumed
                .thread_manager
                .list_thread_ids()
                .await
                .into_iter()
                .collect::<std::collections::HashSet<_>>(),
            expected_loaded
        );
        assert_eq!(
            responses::received_responses_requests(&server).await.len(),
            5,
            "settling known cached work must not sample the later unrelated human turn"
        );
        let _ = release_descendant.send(());
        resumed.codex.shutdown_and_wait().await?;
        return Ok(());
    }
    let descendant_completion = responses::mount_sse_once_match(
        &server,
        header("thread-id", descendant_id.to_string()),
        responses::sse(vec![
            responses::ev_assistant_message("descendant-result", r#"{"descendant":true}"#),
            responses::ev_completed_with_tokens("descendant-result", /*total_tokens*/ 11),
        ]),
    )
    .await;
    release_descendant
        .send(())
        .expect("finish only the descendant owned by the original workflow");
    let completed = super::workflow_v2::observe(&service, &run.id, |run| {
        run.status == WorkflowStatus::Succeeded
    })
    .await?;
    assert_eq!(
        coordinator.agent_status().await,
        AgentStatus::Running,
        "workflow completion must not wait for the later independent human turn"
    );
    assert_eq!(completed.result, Some(json!({"owned":true})));
    assert_eq!(completed.accepted_starts, 1);
    assert_eq!(step(&completed, "coordinator").attempts.len(), 1);
    assert_eq!(
        completed
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(26),
        "only original coordinator usage (3+5) and owned descendant usage (7+11) belong to this workflow"
    );
    assert!(completed.metrics.usage_complete);
    assert_eq!(
        completed.metrics.model_turns,
        Some(4),
        "both owned threads count each actual response once"
    );
    let frozen_attempt_duration = step(&completed, "coordinator").attempts[0]
        .metrics
        .active_duration_ms;
    assert!(
        frozen_attempt_duration.is_some(),
        "native accepted work must have a gate-clock observation"
    );
    let descendant_sample = descendant_completion.single_request();
    assert_eq!(
        descendant_sample.header("thread-id"),
        Some(descendant_id.to_string())
    );
    release_human
        .send(())
        .expect("complete the independent human follow-up after the workflow is already final");
    bounded(wait_for_event(&coordinator, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    }))
    .await;
    core_test_support::ThreadIdle::wait(&coordinator).await;
    // ResponseMock records candidates before its later header matcher rejects
    // them. The descendant's intervening POST is not a human model sample.
    let captured = human_completion.requests();
    let human_sample_count = captured
        .iter()
        .filter(|request| {
            from_thread(request, coordinator_id)
                && request
                    .inputs_of_type("function_call_output")
                    .iter()
                    .any(|item| item["call_id"] == "later-human-tool")
        })
        .count();
    assert_eq!(
        human_sample_count, 1,
        "exactly one actual coordinator response must consume the independent human tool result"
    );
    coordinator.flush_rollout().await?;
    let history = test
        .thread_store
        .load_latest_model_context(LoadThreadHistoryParams {
            thread_id: coordinator_id,
            include_archived: true,
        })
        .await?;
    let own = history
        .items
        .iter()
        .rev()
        .find_map(|item| match item {
            RolloutItem::TokenUsageRecord(record) if record.thread_id == coordinator_id => {
                Some(&record.thread_token_usage)
            }
            _ => None,
        })
        .context("authoritative usage of both coordinator turns")?;
    assert_eq!(
        own.total_tokens, 60,
        "the independent human's 23+29 tokens must actually be recorded on that thread"
    );
    let unchanged = service.get(&run.id).map_err(anyhow::Error::msg)?;
    assert_eq!(unchanged.metrics.model_turns, Some(4));
    assert_eq!(
        step(&unchanged, "coordinator").attempts[0]
            .metrics
            .active_duration_ms,
        frozen_attempt_duration
    );
    assert_eq!(
        service
            .get(&run.id)
            .map_err(anyhow::Error::msg)?
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(26)
    );
    assert_eq!(
        responses::received_responses_requests(&server).await.len(),
        6
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_pause_stalls_new_javascript_and_stop_does_not_require_unpause() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = builder().build_with_auto_env(&server).await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut events = service.subscribe();
    let blocker = service
        .start(
            super::workflow_v2::script("await new Promise(() => {});"),
            Value::Null,
        )
        .await
        .map_err(anyhow::Error::msg)?;
    observe(&service, &mut events, &blocker.id, |run| {
        run.status == WorkflowStatus::Running
    })
    .await?;
    assert!(
        test.codex.can_pause_execution().await?,
        "a running workflow must make its owning conversation pauseable"
    );
    assert_eq!(
        test.codex.pause_execution().await?.state,
        ExecutionControlState::Paused
    );
    let run = service
        .start(
            super::workflow_v2::script_with_phases(
                &["must wait"],
                "workflow.phase('must wait'); return {done:true};",
            ),
            Value::Null,
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let running = observe(&service, &mut events, &run.id, |run| {
        run.status == WorkflowStatus::Running
    })
    .await?;
    assert_eq!((running.phase, running.result), (None, None));
    assert!(
        tokio::time::timeout(
            Duration::from_millis(/*millis*/ 150),
            observe(&service, &mut events, &run.id, |run| run.phase.is_some())
        )
        .await
        .is_err(),
        "paused root must gate pure JavaScript, before its first phase"
    );
    let stopped = bounded(service.stop(&run.id))
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(stopped.status, WorkflowStatus::Stopped);
    assert_eq!(
        test.codex.execution_state().await?.state,
        ExecutionControlState::Paused
    );
    assert!(
        responses::received_responses_requests(&server)
            .await
            .is_empty()
    );
    let stopped_blocker = bounded(service.stop(&blocker.id))
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(stopped_blocker.status, WorkflowStatus::Stopped);
    test.codex.unpause_execution().await?;
    let resumed = service.recover(&run.id).await.map_err(anyhow::Error::msg)?;
    assert_eq!(resumed.status, WorkflowStatus::Pending);
    let completed = observe(&service, &mut events, &run.id, |run| {
        run.status == WorkflowStatus::Succeeded
    })
    .await?;
    assert_eq!(
        (completed.phases, completed.result),
        (vec!["must wait".to_string()], Some(json!({"done":true})))
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_workflow_child_pauses_and_resamples_the_same_child_on_unpause() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let (release, gate) = oneshot::channel();
    let (streaming, _) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("held")]),
            },
            StreamingSseChunk {
                gate: Some(gate),
                body: responses::sse(vec![responses::ev_completed("held")]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: answer("resumed", r#"{"resumed":true}"#),
        }],
    ])
    .await;
    let server = responses::start_mock_server().await;
    let url = format!("{}/v1", streaming.uri());
    let test = builder()
        .with_config(move |config| config.model_provider.base_url = Some(url))
        .build_with_auto_env(&server)
        .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut events = service.subscribe();
    let run = service
        .start(
            super::workflow_v2::script(
                "return await workflow.agent('Pause this real child request.',{label:'sample'});",
            ),
            Value::Null,
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let accepted = observe(&service, &mut events, &run.id, |run| {
        run.steps
            .values()
            .find(|step| step.label == "sample")
            .is_some_and(|step| step.attempts[0].child_thread_id.is_some())
    })
    .await?;
    bounded(streaming.wait_for_request_count(/*count*/ 1)).await;
    let child = test
        .thread_manager
        .get_thread(child_id(&accepted, "sample")?)
        .await?;
    let path = child
        .config_snapshot()
        .await
        .session_source
        .get_agent_path()
        .context("readable native workflow path")?;
    assert_eq!(path.name(), "workflow_r1_sample_s1_a1");
    let paused = bounded(test.codex.pause_execution()).await?;
    assert_eq!(paused.state, ExecutionControlState::Paused);
    assert_eq!(child.execution_state().await?, paused);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(/*millis*/ 150),
            streaming.wait_for_request_count(/*count*/ 2)
        )
        .await
        .is_err(),
        "paused child must not reopen inference"
    );
    assert_eq!(
        service.get(&run.id).map_err(anyhow::Error::msg)?.status,
        WorkflowStatus::Running
    );
    test.codex.unpause_execution().await?;
    let completed = observe(&service, &mut events, &run.id, |run| {
        run.status == WorkflowStatus::Succeeded
    })
    .await?;
    assert_eq!(completed.result, Some(json!({"resumed":true})));
    assert_eq!(step(&completed, "sample").attempts.len(), 1);
    let requests = streaming
        .requests()
        .await
        .into_iter()
        .map(|bytes| serde_json::from_slice::<Value>(&bytes))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0]["client_metadata"]["thread_id"],
        requests[1]["client_metadata"]["thread_id"]
    );
    assert_eq!(
        requests[1]["client_metadata"]["thread_id"],
        json!(child_id(&completed, "sample")?)
    );
    drop(release);
    test.codex.shutdown_and_wait().await?;
    streaming.shutdown().await;
    Ok(())
}

#[test_case(false; "known_child")]
#[test_case(true; "unresolved_child")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_reconciles_accepted_child_and_retries_only_authoritatively_failed_attempt(
    unresolved: bool,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = builder().build_with_auto_env(&server).await?;
    test.codex.ensure_rollout_materialized().await;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| contains(request, "first durable child"),
        answer("first", r#"{"cached":true}"#),
    )
    .await;
    responses::mount_response_once_match(
        &server,
        |request: &wiremock::Request| contains(request, "second durable child"),
        responses::sse_response(answer("never-retrieved", "unused"))
            .set_delay(Duration::from_secs(/*secs*/ 60)),
    )
    .await;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut events = service.subscribe();
    let run = service
        .start(
            super::workflow_v2::script(
                r#"
const first = await workflow.agent("first durable child", {label:"first"});
const second = await workflow.agent("second durable child", {label:"second"});
return [first, second];
"#,
            ),
            Value::Null,
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let state_file = test
        .config
        .codex_home
        .join("workflow-runs")
        .join(test.session_configured.thread_id.to_string())
        .join(format!("{}.json", run.id));
    let accepted = observe(&service, &mut events, &run.id, |run| {
        run.steps
            .values()
            .find(|step| step.label == "second")
            .is_some_and(|step| step.attempts[0].child_thread_id.is_some())
    })
    .await?;
    let prior_child = child_id(&accepted, "second")?;
    let prior_path = test
        .thread_manager
        .get_thread(prior_child)
        .await?
        .config_snapshot()
        .await
        .session_source
        .get_agent_path()
        .context("accepted workflow path")?;
    assert_eq!(prior_path.name(), "workflow_r1_second_s2_a1");
    let observed = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
        loop {
            if responses::received_responses_requests(&server)
                .await
                .iter()
                .any(|request| from_thread(request, prior_child))
            {
                break;
            }
            ticks.tick().await;
        }
    })
    .await;
    if let Err(error) = observed {
        let timeline = lifecycle_timeline(&test, prior_child).await;
        return Err(anyhow::Error::new(error).context(format!("accepted second child {prior_child} never issued its first provider request; persisted timeline={timeline:?}; run={}", brief(&service.get(&run.id).map_err(anyhow::Error::msg)?))));
    }
    let preserved = step(&accepted, "first").clone();
    let second_step_id = step(&accepted, "second").id.clone();
    assert_eq!(accepted.accepted_starts, 2);
    // A new manager in this process must use the same complete persistence
    // handoff as a real server shutdown, including all live child writers.
    let shutdown = test
        .thread_manager
        .request_agent_tree_shutdown(test.session_configured.thread_id)
        .await?;
    match tokio::time::timeout(Duration::from_secs(/*secs*/ 30), shutdown.wait()).await {
        Ok(result) => result?,
        Err(error) => {
            let timeline = lifecycle_timeline(&test, prior_child).await;
            return Err(anyhow::Error::new(error).context(format!("tree shutdown did not join accepted second child {prior_child}; persisted timeline={timeline:?}; run={}", brief(&service.get(&run.id).map_err(anyhow::Error::msg)?))));
        }
    }
    let shutdown_timeline = lifecycle_timeline(&test, prior_child)
        .await
        .context("read the accepted child's durable terminal timeline after tree shutdown")?;
    if unresolved {
        let manifest: Value = serde_json::from_slice(&std::fs::read(&state_file)?)?;
        let record = manifest["steps"][second_step_id.as_str()]["attempts"][0]
            .as_str()
            .context("current-format attempt record")?;
        let path = state_file
            .parent()
            .context("workflow run directory")?
            .join(&run.id)
            .join("records")
            .join(format!("{record}.json"));
        let mut attempt: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        attempt["childThreadId"] = Value::Null;
        std::fs::write(path, serde_json::to_vec(&attempt)?)?;
    }
    drop(events);
    drop(service);
    let mut resume_builder = builder();
    let resumed = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), resume_builder.restart_with_auto_env(&server, &test)).await
        .with_context(|| format!("root auto-env restart did not settle after accepted child {prior_child}; shutdown timeline={shutdown_timeline:?}"))??;
    drop(test);
    let recovered_service = resumed
        .codex
        .workflows()
        .await
        .map_err(anyhow::Error::msg)?;
    let mut recovered_events = recovered_service.subscribe();
    assert_eq!(
        recovered_service
            .get(&run.id)
            .map_err(anyhow::Error::msg)?
            .status,
        WorkflowStatus::Interrupted
    );
    recovered_service
        .recover(&run.id)
        .await
        .map_err(anyhow::Error::msg)?;
    let reconciled = observe(&recovered_service, &mut recovered_events, &run.id, |run| {
        run.status == WorkflowStatus::Failed
    })
    .await
    .with_context(|| {
        format!("accepted child {prior_child} durable shutdown timeline: {shutdown_timeline:?}")
    })?;
    assert_eq!(step(&reconciled, "first"), &preserved);
    assert_eq!(child_id(&reconciled, "second")?, prior_child);
    assert_eq!(step(&reconciled, "second").attempts.len(), 1);
    assert_eq!(
        reconciled.accepted_starts, 2,
        "reconnecting the same accepted child must not spend another start"
    );
    assert_eq!(
        responses::received_responses_requests(&server).await.len(),
        2,
        "recovery of stopped accepted child must not spawn or sample"
    );
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| contains(request, "second durable child"),
        answer("retried", r#"{"retried":true}"#),
    )
    .await;
    recovered_service
        .retry(&run.id)
        .await
        .map_err(anyhow::Error::msg)?;
    let completed = observe(&recovered_service, &mut recovered_events, &run.id, |run| {
        run.status == WorkflowStatus::Succeeded
    })
    .await.with_context(|| format!("retry after accepted child {prior_child} durable shutdown timeline: {shutdown_timeline:?}"))?;
    assert_eq!(step(&completed, "first"), &preserved);
    assert_eq!(step(&completed, "second").attempts.len(), 2);
    assert_eq!(
        completed.accepted_starts, 3,
        "a genuinely new retry child spends exactly one start"
    );
    assert_ne!(child_id(&completed, "second")?, prior_child);
    assert_eq!(
        completed.result,
        Some(json!([{"cached":true},{"retried":true}]))
    );
    let requests = responses::received_responses_requests(&server).await;
    let first_child = child_id(&completed, "first")?;
    let second_child = child_id(&completed, "second")?;
    assert_eq!(
        requests
            .iter()
            .filter(|request| from_thread(request, first_child))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| from_thread(request, second_child))
            .count(),
        1
    );
    assert_eq!(requests.len(), 3);
    resumed.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_before_native_acceptance_does_not_turn_retry_into_unknown_child_recovery()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = builder()
        .with_config(|config| {
            config.multi_agent_v2.max_concurrent_threads_per_session = 2;
            config
                .features
                .disable(Feature::WakeOnAgentMessages)
                .expect("capacity holder reports do not wake the root");
        })
        .build_with_auto_env(&server)
        .await?;
    let root = test.session_configured.thread_id.to_string();
    responses::mount_sse_once_match(&server, header("thread-id", root.as_str()), responses::sse(vec![
        responses::ev_function_call_with_namespace("capacity-holder", "lifecycle_agents", "spawn_agent", &json!({
            "task_name":"capacity_holder", "message":"Keep the only ordinary native slot occupied.", "fork_turns":"none",
        }).to_string()),
        responses::ev_completed("holder-created"),
    ])).await;
    let spawn_completion = responses::mount_sse_once_match(
        &server,
        header("thread-id", root.as_str()),
        answer("root-ready", "The capacity holder is running."),
    )
    .await;
    responses::mount_response_once_match(
        &server,
        |request: &wiremock::Request| {
            contains(request, "Keep the only ordinary native slot occupied.")
        },
        responses::sse_response(answer("held-ordinary", "unused"))
            .set_delay(Duration::from_secs(/*secs*/ 60)),
    )
    .await;
    let mut created = test.thread_manager.subscribe_thread_created();
    test.submit_text_turn("Create the ordinary capacity holder.")
        .await?;
    let accepted_path = spawned_path(&spawn_completion.single_request(), "capacity-holder")?;
    let ordinary_id = bounded(created.recv()).await?;
    let ordinary = test.thread_manager.get_thread(ordinary_id).await?;
    assert_eq!(
        ordinary
            .config_snapshot()
            .await
            .session_source
            .get_agent_path(),
        Some(accepted_path)
    );
    let ordinary_request = requested(&server, |request| {
        request.body_json()["client_metadata"]["x-codex-parent-thread-id"] == json!(root)
            && request.body_contains_text("Keep the only ordinary native slot occupied.")
    })
    .await;
    assert_eq!(
        ordinary_request.header("thread-id"),
        Some(ordinary_id.to_string())
    );
    assert_eq!(ordinary.agent_status().await, AgentStatus::Running);
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut events = service.subscribe();
    let run = service.start(super::workflow_v2::script("return await workflow.agent('Start only after the ordinary capacity holder is released.',{label:'queued'});"), json!({})).await.map_err(anyhow::Error::msg)?;
    let queued = observe(&service, &mut events, &run.id, |run| {
        run.steps.values().any(|step| {
            step.label == "queued"
                && step.attempts.last().is_some_and(|attempt| {
                    attempt.admission == codex_core::workflows::AdmissionStatus::Reserved
                        && attempt.child_thread_id.is_none()
                })
        })
    })
    .await?;
    assert_eq!(queued.accepted_starts, 0);
    assert_eq!(test.thread_manager.list_thread_ids().await.len(), 2);
    let stopped = bounded(service.stop(&run.id))
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        (stopped.status, stopped.accepted_starts),
        (WorkflowStatus::Stopped, 0)
    );
    let refused = &step(&stopped, "queued").attempts[0];
    assert_eq!(
        (refused.admission, refused.child_thread_id.as_deref()),
        (codex_core::workflows::AdmissionStatus::Rejected, None),
        "a cancelled capacity wait is a confirmed non-start, not an unknown accepted child"
    );
    ordinary
        .submit(codex_protocol::protocol::Op::Interrupt)
        .await?;
    bounded(wait_for_event(&ordinary, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    }))
    .await;
    let model = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            contains(
                request,
                "Start only after the ordinary capacity holder is released.",
            )
        },
        answer("queued-retry", r#"{"started":true}"#),
    )
    .await;
    service.retry(&run.id).await.map_err(anyhow::Error::msg)?;
    let completed = observe(&service, &mut events, &run.id, |run| {
        run.status == WorkflowStatus::Succeeded
    })
    .await?;
    assert_eq!(
        (completed.accepted_starts, &completed.result),
        (1, &Some(json!({"started":true})))
    );
    let attempts = &step(&completed, "queued").attempts;
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0], *refused);
    assert_eq!(
        attempts[1].admission,
        codex_core::workflows::AdmissionStatus::Accepted
    );
    assert_ne!(
        attempts[1].child_thread_id.as_deref(),
        Some(ordinary_id.to_string().as_str())
    );
    model.single_request();
    assert_eq!(
        responses::received_responses_requests(&server).await.len(),
        4,
        "stopped preacceptance work must never sample until its one real retry starts"
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_one_paused_run_preserves_sibling_and_similarly_named_ordinary_worker()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = builder().build_with_auto_env(&server).await?;
    let root = test.session_configured.thread_id.to_string();
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut events = service.subscribe();
    let a_root = root.clone();
    responses::mount_response_once_match(
        &server,
        move |request: &wiremock::Request| {
            request
                .headers
                .get("thread-id")
                .is_some_and(|id| id != a_root.as_str())
                && contains(request, "workflow-isolation-A")
        },
        responses::sse_response(answer("held-a", "unused"))
            .set_delay(Duration::from_secs(/*secs*/ 60)),
    )
    .await;
    let b_root = root.clone();
    responses::mount_response_once_match(
        &server,
        move |request: &wiremock::Request| {
            request
                .headers
                .get("thread-id")
                .is_some_and(|id| id != b_root.as_str())
                && contains(request, "workflow-isolation-B")
        },
        responses::sse_response(answer("held-b", "unused"))
            .set_delay(Duration::from_secs(/*secs*/ 60)),
    )
    .await;
    let a = service
        .start(
            super::workflow_v2::script(
                "return await workflow.agent('workflow-isolation-A',{label:'audit'});",
            ),
            Value::Null,
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let accepted_a = observe(&service, &mut events, &a.id, |run| {
        run.steps
            .values()
            .find(|step| step.label == "audit")
            .is_some_and(|step| step.attempts[0].child_thread_id.is_some())
    })
    .await?;
    let a_id = child_id(&accepted_a, "audit")?;
    requested(&server, |request| from_thread(request, a_id)).await;
    let a_child = test.thread_manager.get_thread(a_id).await?;
    let similar_name = format!("workflow_{}_ordinary", a.id.replace('-', ""));
    responses::mount_sse_once_match(&server, header("thread-id",root.as_str()), responses::sse(vec![responses::ev_function_call_with_namespace("ordinary-spawn","lifecycle_agents","spawn_agent",&json!({"task_name":similar_name,"message":"ordinary unaffected worker","fork_turns":"none"}).to_string()), responses::ev_completed("ordinary-spawn")])).await;
    responses::mount_sse_once_match(
        &server,
        header("thread-id", root.as_str()),
        answer("root-done", "Ordinary worker started."),
    )
    .await;
    let ordinary_root = root.clone();
    responses::mount_response_once_match(
        &server,
        move |request: &wiremock::Request| {
            request
                .headers
                .get("thread-id")
                .is_some_and(|id| id != ordinary_root.as_str())
                && contains(request, "ordinary unaffected worker")
        },
        responses::sse_response(answer("held-ordinary", "unused"))
            .set_delay(Duration::from_secs(/*secs*/ 60)),
    )
    .await;
    test.submit_text_turn("Create the ordinary worker next to the workflow.")
        .await?;
    let ordinary_request = requested(&server, |request| {
        request.body_json()["client_metadata"]["x-codex-parent-thread-id"] == json!(root)
            && request.body_contains_text("ordinary unaffected worker")
    })
    .await;
    let ordinary_id = ThreadId::from_string(
        &ordinary_request
            .header("thread-id")
            .context("ordinary thread header")?,
    )?;
    let ordinary = test.thread_manager.get_thread(ordinary_id).await?;
    let b = service
        .start(
            super::workflow_v2::script(
                "return await workflow.agent('workflow-isolation-B',{label:'audit-more'});",
            ),
            Value::Null,
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let accepted_b = observe(&service, &mut events, &b.id, |run| {
        run.steps
            .values()
            .find(|step| step.label == "audit-more")
            .is_some_and(|step| step.attempts[0].child_thread_id.is_some())
    })
    .await?;
    let b_id = child_id(&accepted_b, "audit-more")?;
    requested(&server, |request| from_thread(request, b_id)).await;
    let b_child = test.thread_manager.get_thread(b_id).await?;
    bounded(test.codex.pause_execution()).await?;
    let stopped = bounded(service.stop(&a.id))
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(stopped.status, WorkflowStatus::Stopped);
    assert_eq!(a_child.agent_status().await, AgentStatus::Interrupted);
    assert_eq!(
        service.get(&b.id).map_err(anyhow::Error::msg)?.status,
        WorkflowStatus::Running
    );
    assert_eq!(b_child.agent_status().await, AgentStatus::Running);
    assert_eq!(ordinary.agent_status().await, AgentStatus::Running);
    responses::mount_sse_once_match(
        &server,
        header("thread-id", b_id.to_string()),
        answer("b-completed", r#"{"sibling":true}"#),
    )
    .await;
    responses::mount_sse_once_match(
        &server,
        header("thread-id", ordinary_id.to_string()),
        answer("ordinary-completed", "ordinary survived"),
    )
    .await;
    responses::mount_sse_once_match(
        &server,
        header("thread-id", root.as_str()),
        answer("notification", "The ordinary worker completed."),
    )
    .await;
    test.codex.unpause_execution().await?;
    let completed_b = match observe(&service, &mut events, &b.id, |run| {
        run.status == WorkflowStatus::Succeeded
    })
    .await
    {
        Ok(run) => run,
        Err(error) => {
            let requests = responses::received_responses_requests(&server).await;
            return Err(error.context(format!("actual sibling request counts: A={}, B={}, ordinary={}; child-status={:?}; child-execution={:?}",requests.iter().filter(|request|from_thread(request,a_id)).count(),requests.iter().filter(|request|from_thread(request,b_id)).count(),requests.iter().filter(|request|from_thread(request,ordinary_id)).count(),b_child.agent_status().await,b_child.execution_state().await?)));
        }
    };
    wait_for_event(&ordinary, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(completed_b.result, Some(json!({"sibling":true})));
    assert_eq!(
        service.get(&a.id).map_err(anyhow::Error::msg)?.status,
        WorkflowStatus::Stopped
    );
    let requests = responses::received_responses_requests(&server).await;
    assert_eq!(
        (
            requests
                .iter()
                .filter(|request| from_thread(request, a_id))
                .count(),
            requests
                .iter()
                .filter(|request| from_thread(request, b_id))
                .count(),
            requests
                .iter()
                .filter(|request| from_thread(request, ordinary_id))
                .count()
        ),
        (1, 2, 2),
        "stopped A must never sample after resume, while both surviving children resume exactly once"
    );
    assert_eq!(a_child.agent_status().await, AgentStatus::Interrupted);
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_a_paused_run_settles_its_private_review_without_cancelling_other_runs()
-> Result<()> {
    use codex_core::config::Constrained;
    use codex_protocol::config_types::ApprovalsReviewer;
    use codex_protocol::protocol::AskForApproval;

    skip_if_no_network!(Ok(()));
    core_test_support::skip_if_remote!(
        Ok(()),
        "checks actual local command side effects in each workflow's cwd"
    );
    core_test_support::skip_if_target_windows!(
        Ok(()),
        "uses a POSIX command to identify approved actions"
    );
    let (release_a, gate_a) = oneshot::channel();
    let (release_b, gate_b) = oneshot::channel();
    let (release_unrelated, gate_unrelated) = oneshot::channel();
    let command = |label: &str| {
        responses::sse(vec![
        responses::ev_function_call(&format!("approved-{label}"), "exec_command", &json!({
            "cmd":format!("printf approved > approved-{label}.txt"),
            "sandbox_permissions":"require_escalated", "justification":format!("Approve workflow {label}'s requested action."),
        }).to_string()),
        responses::ev_completed(&format!("command-{label}")),
    ])
    };
    let held = |id: &str, gate| {
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created(id)]),
            },
            StreamingSseChunk {
                gate: Some(gate),
                body: responses::sse(vec![responses::ev_completed(id)]),
            },
        ]
    };
    let (streaming, _) = start_streaming_sse_server(vec![
        vec![StreamingSseChunk {
            gate: None,
            body: command("A"),
        }],
        held("review-A", gate_a),
        vec![StreamingSseChunk {
            gate: None,
            body: command("B"),
        }],
        held("review-B", gate_b),
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("independent")]),
            },
            StreamingSseChunk {
                gate: Some(gate_unrelated),
                body: responses::sse(vec![
                    responses::ev_assistant_message(
                        "independent-result",
                        "Independent root completed.",
                    ),
                    responses::ev_completed("independent"),
                ]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: answer(
                "review-B-resumed",
                r#"{"risk_level":"low","user_authorization":"high","outcome":"allow"}"#,
            ),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: answer("B-completed", r#"{"sibling":true}"#),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: answer("root-drained", "All surviving work completed."),
        }],
    ])
    .await;
    let bootstrap = responses::start_mock_server().await;
    let mut extensions =
        codex_extension_api::ExtensionRegistryBuilder::<codex_core::config::Config>::new();
    extensions.thread_lifecycle_contributor(Arc::new(core_test_support::ThreadIdle));
    let url = format!("{}/v1", streaming.uri());
    let test = builder().with_extensions(Arc::new(extensions.build())).with_config(move |config| {
        config.model_provider.base_url = Some(url);
        config.model_provider.supports_websockets = false;
        config.features.disable(Feature::WakeOnAgentMessages).expect("reports wait for a human turn");
        config.features.enable(Feature::GuardianRecordedItems).expect("observe exact private reviewer aborts through the supported parent event stream");
        config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
        config.approvals_reviewer = ApprovalsReviewer::AutoReview;
    }).build_with_auto_env(&bootstrap).await?;
    let url = format!("{}/v1", streaming.uri());
    let unrelated = builder()
        .with_config(move |config| {
            config.model_provider.base_url = Some(url);
            config.model_provider.supports_websockets = false;
        })
        .build_with_auto_env(&bootstrap)
        .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut events = service.subscribe();
    let a = service
        .start(
            super::workflow_v2::script(
                "return await workflow.agent('Run only workflow A requested action.',{label:'A'});",
            ),
            json!({}),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    bounded(streaming.wait_for_request_count(/*count*/ 2)).await;
    let accepted_a = service.get(&a.id).map_err(anyhow::Error::msg)?;
    let a_id = child_id(&accepted_a, "A")?;
    let a_child = test.thread_manager.get_thread(a_id).await?;
    let b = service
        .start(
            super::workflow_v2::script(
                "return await workflow.agent('Run only workflow B requested action.',{label:'B'});",
            ),
            json!({}),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    bounded(streaming.wait_for_request_count(/*count*/ 4)).await;
    let accepted_b = service.get(&b.id).map_err(anyhow::Error::msg)?;
    let b_id = child_id(&accepted_b, "B")?;
    let b_child = test.thread_manager.get_thread(b_id).await?;
    unrelated
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Finish this independent conversation.".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    bounded(streaming.wait_for_request_count(/*count*/ 5)).await;
    let initial = streaming
        .requests()
        .await
        .into_iter()
        .map(|bytes| serde_json::from_slice::<Value>(&bytes))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(initial[0]["client_metadata"]["thread_id"], json!(a_id));
    assert_eq!(initial[2]["client_metadata"]["thread_id"], json!(b_id));
    let private_a = initial[1]["client_metadata"]["thread_id"]
        .as_str()
        .context("A's real private reviewer request")?
        .to_owned();
    let private_b = initial[3]["client_metadata"]["thread_id"]
        .as_str()
        .context("B's real private reviewer request")?
        .to_owned();
    assert_ne!(private_a, private_b);
    assert_eq!(
        initial[1]["client_metadata"]["x-codex-parent-thread-id"],
        json!(a_id)
    );
    assert_eq!(
        initial[3]["client_metadata"]["x-codex-parent-thread-id"],
        json!(b_id)
    );
    assert!(
        test.thread_manager
            .get_thread(ThreadId::from_string(&private_a)?)
            .await
            .is_err(),
        "Guardian reviewers remain private"
    );
    assert_eq!(
        bounded(test.codex.pause_execution()).await?.state,
        ExecutionControlState::Paused
    );
    let stopped = bounded(service.stop(&a.id))
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(stopped.status, WorkflowStatus::Stopped);
    let private_a_id = ThreadId::from_string(&private_a)?;
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30), wait_for_event(&a_child, |event|
        matches!(event, EventMsg::PrivateSessionEvent(private) if private.thread_id == private_a_id
            && matches!(&private.event, codex_protocol::protocol::PrivateSessionEventPayload::TurnAborted(_)))))
        .await.context("joined stop must forward the exact private A review's abort to its parent")?;
    assert_eq!(a_child.agent_status().await, AgentStatus::Interrupted);
    assert_eq!(
        service.get(&b.id).map_err(anyhow::Error::msg)?.status,
        WorkflowStatus::Running
    );
    assert_eq!(b_child.agent_status().await, AgentStatus::Running);
    assert_eq!(
        b_child.execution_state().await?.state,
        ExecutionControlState::Paused
    );
    assert_eq!(
        unrelated.codex.execution_state().await?.state,
        ExecutionControlState::Running
    );
    release_unrelated
        .send(())
        .expect("independent inference can finish while the other tree stays paused");
    let independent = bounded(wait_for_event(&unrelated.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    }))
    .await;
    let EventMsg::TurnComplete(independent) = independent else {
        unreachable!()
    };
    assert_eq!(
        independent.last_agent_message.as_deref(),
        Some("Independent root completed.")
    );
    assert_eq!(
        streaming.requests().await.len(),
        5,
        "stop must settle A without sampling B's still-paused review"
    );
    assert!(!test.cwd.path().join("approved-A.txt").exists());
    assert!(!test.cwd.path().join("approved-B.txt").exists());
    test.codex.unpause_execution().await?;
    let finished_b = observe(&service, &mut events, &b.id, |run| {
        run.status == WorkflowStatus::Succeeded && run.notification_delivered
    })
    .await?;
    assert_eq!(finished_b.result, Some(json!({"sibling":true})));
    assert_eq!(
        std::fs::read_to_string(test.cwd.path().join("approved-B.txt"))?,
        "approved"
    );
    assert!(
        !test.cwd.path().join("approved-A.txt").exists(),
        "a stopped review must never release its parent's action"
    );
    bounded(async {
        loop {
            if service
                .get(&a.id)
                .map_err(anyhow::Error::msg)?
                .notification_delivered
            {
                return Ok::<_, anyhow::Error>(());
            }
            events.recv().await?;
        }
    })
    .await?;
    core_test_support::submit_thread_settings(&test.codex, ThreadSettingsOverrides::default())
        .await?;
    test.submit_text_turn("Consume the retained reports after the surviving workflow completes.")
        .await?;
    core_test_support::ThreadIdle::wait(&test.codex).await;
    let requests = streaming
        .requests()
        .await
        .into_iter()
        .map(|bytes| serde_json::from_slice::<Value>(&bytes))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(requests.len(), 8);
    assert_eq!(
        requests[5]["client_metadata"]["thread_id"],
        json!(private_b)
    );
    assert_eq!(requests[6]["client_metadata"]["thread_id"], json!(b_id));
    assert_eq!(
        requests[7]["client_metadata"]["thread_id"],
        json!(test.session_configured.thread_id)
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["client_metadata"]["thread_id"] == json!(private_a))
            .count(),
        1
    );
    assert_eq!(a_child.agent_status().await, AgentStatus::Interrupted);
    assert_eq!(
        b_child.agent_status().await,
        AgentStatus::Completed(Some(r#"{"sibling":true}"#.into()))
    );
    let _ = release_a.send(());
    let _ = release_b.send(());
    test.codex.shutdown_and_wait().await?;
    unrelated.codex.shutdown_and_wait().await?;
    streaming.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_source_read_uses_the_restricted_childs_permissions_and_keeps_the_root_catalog()
-> Result<()> {
    use codex_core::workflows::WorkflowScope;
    use codex_protocol::permissions::FileSystemAccessMode;
    use codex_protocol::permissions::FileSystemSandboxEntry;
    use codex_protocol::permissions::NetworkSandboxPolicy;

    skip_if_no_network!(Ok(()));
    core_test_support::skip_if_remote!(
        Ok(()),
        "saved workflow sources and their permission checks are host-local"
    );
    const SOURCE: &str = "// SAVED_SOURCE_PRIVATE_MARKER\nawait workflow.publish({secret:true});";
    let server = responses::start_mock_server().await;
    let test = builder()
        .with_pre_build_hook(|home| {
            std::fs::write(
                home.join("source-reader.config.toml"),
                "model_reasoning_effort = 'low'\n",
            )
            .expect("write a native non-leaf reader profile");
        })
        .with_config(|config| {
            config
                .features
                .disable(Feature::WakeOnAgentMessages)
                .expect("catalog fixture messages do not wake the root");
            config
                .permissions
                .set_permission_profile(PermissionProfile::Disabled)
                .expect("root may read its own saved source");
            config.agent_roles.insert(
                "source-reader".into(),
                AgentRoleConfig {
                    profile: Some("source-reader".parse().expect("reader profile")),
                    description: Some("Read the permitted workflow catalog".into()),
                    config_file: None,
                    nickname_candidates: None,
                },
            );
        })
        .build_with_auto_env(&server)
        .await?;
    let root = test.session_configured.thread_id.to_string();
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let saved_path = service
        .library()
        .save_target("secret", WorkflowScope::User)
        .map_err(anyhow::Error::msg)?;
    // This is an existing historical source, not a request to save a new V1 script.
    std::fs::create_dir_all(saved_path.parent().context("saved source directory")?)?;
    std::fs::write(saved_path.as_path(), SOURCE)?;
    let saved = service
        .library()
        .read("user:secret")
        .map_err(anyhow::Error::msg)?;
    assert!(
        saved.read_only,
        "authorized users may still inspect legacy source without executing it"
    );
    responses::mount_sse_once_match(&server, header("thread-id", root.as_str()), responses::sse(vec![
        responses::ev_function_call_with_namespace("reader-spawn", "lifecycle_agents", "spawn_agent", &json!({
            "task_name":"source_reader", "agent_type":"source-reader", "message":"Wait for the next catalog inspection.", "fork_turns":"none",
        }).to_string()), responses::ev_completed("reader-created"),
    ])).await;
    let spawn_completion = responses::mount_sse_once_match(
        &server,
        header("thread-id", root.as_str()),
        answer("root-ready", "The reader is ready."),
    )
    .await;
    let parent = root.clone();
    responses::mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            request
                .headers
                .get("thread-id")
                .is_some_and(|id| id != parent.as_str())
        },
        answer("reader-ready", "Ready for a permitted catalog inspection."),
    )
    .await;
    let mut created = test.thread_manager.subscribe_thread_created();
    test.submit_text_turn("Create the profile-backed catalog reader.")
        .await?;
    let accepted_path = spawned_path(&spawn_completion.single_request(), "reader-spawn")?;
    let reader_id = bounded(created.recv()).await?;
    let reader = test.thread_manager.get_thread(reader_id).await?;
    assert_eq!(
        reader
            .config_snapshot()
            .await
            .session_source
            .get_agent_path(),
        Some(accepted_path)
    );
    let initial = requested(&server, |request| {
        request.body_json()["client_metadata"]["x-codex-parent-thread-id"] == json!(root)
    })
    .await;
    assert_eq!(initial.header("thread-id"), Some(reader_id.to_string()));
    bounded(wait_for_event(&reader, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    }))
    .await;
    let reader_policy = reader.config().await;
    assert!(
        reader_policy.features.enabled(Feature::MultiAgentV2)
            && !reader_policy.multi_agent_v2.disable_native_tools,
        "the actual reader must already have native control tools"
    );
    let mut file_system = PermissionProfile::read_only().file_system_sandbox_policy();
    file_system.entries.push(FileSystemSandboxEntry::new(
        saved_path.clone().into(),
        FileSystemAccessMode::Deny,
    ));
    reader
        .update_thread_settings(ThreadSettingsOverrides {
            permission_profile: Some(PermissionProfile::from_runtime_permissions(
                &file_system,
                NetworkSandboxPolicy::Restricted,
            )),
            ..Default::default()
        })
        .await?;
    let reader_service = reader.workflows().await.map_err(anyhow::Error::msg)?;
    assert!(
        Arc::ptr_eq(&service, &reader_service),
        "the caller must use the same permissive root catalog service"
    );
    let read_args = json!({"saved_workflow_id":saved.id,"offset":0}).to_string();
    responses::mount_sse_once_match(
        &server,
        header("thread-id", root.as_str()),
        responses::sse(vec![
            responses::ev_function_call("root-source-read", "saved_workflow_read", &read_args),
            responses::ev_completed("root-source-read"),
        ]),
    )
    .await;
    let root_output = responses::mount_sse_once_match(
        &server,
        header("thread-id", root.as_str()),
        answer(
            "root-inspected",
            "The authorized legacy source was inspected.",
        ),
    )
    .await;
    test.submit_text_turn("Inspect the saved legacy source without executing it.")
        .await?;
    let (authorized, _) = root_output
        .single_request()
        .function_call_output_content_and_success("root-source-read")
        .context("authorized source page")?;
    assert!(
        authorized
            .context("authorized source text")?
            .contains("SAVED_SOURCE_PRIVATE_MARKER")
    );
    responses::mount_sse_once_match(
        &server,
        header("thread-id", reader_id.to_string()),
        responses::sse(vec![
            responses::ev_function_call("reader-catalog", "saved_workflow_list", "{}"),
            responses::ev_function_call("denied-source-read", "saved_workflow_read", &read_args),
            responses::ev_completed("reader-inspection"),
        ]),
    )
    .await;
    let reader_output = responses::mount_sse_once_match(
        &server,
        header("thread-id", reader_id.to_string()),
        answer(
            "reader-inspected",
            "The source was denied; its catalog entry remains visible.",
        ),
    )
    .await;
    reader
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "Inspect the shared saved-workflow catalog and its secret entry.".into(),
                text_elements: Vec::new(),
            }])
            .from_agent(),
        )
        .await?;
    bounded(wait_for_event(&reader, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    }))
    .await;
    let request = reader_output.single_request();
    let (catalog, _) = request
        .function_call_output_content_and_success("reader-catalog")
        .context("shared catalog response")?;
    let catalog: Value = serde_json::from_str(&catalog.context("shared catalog JSON")?)?;
    assert_eq!(catalog["workflows"][0]["id"], json!(saved.id));
    assert_eq!(catalog["workflows"][0]["read_only"], json!(true));
    let (denied, _) = request
        .function_call_output_content_and_success("denied-source-read")
        .context("denied source response")?;
    assert!(
        denied
            .context("permission rejection")?
            .contains("filesystem policy does not permit reading")
    );
    assert!(
        !request.body_contains_text("SAVED_SOURCE_PRIVATE_MARKER"),
        "neither source nor its first 384-byte preview may reach the restricted child"
    );
    assert_eq!(
        test.codex.config_snapshot().await.permission_profile,
        PermissionProfile::Disabled
    );
    assert_eq!(std::fs::read_to_string(saved_path.as_path())?, SOURCE);
    assert!(
        service
            .list(/*offset*/ 0, /*limit*/ 50)
            .map_err(anyhow::Error::msg)?
            .is_empty(),
        "inspecting legacy source must never execute it"
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_origin_launch_uses_root_service_but_retains_profile_and_readonly_authority()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = builder().with_pre_build_hook(|home| {
        std::fs::write(home.join("origin.config.toml"),"model_reasoning_effort = 'low'\ndeveloper_instructions = 'Retain the origin profile authority marker.'\n").expect("write named origin profile");
    }).with_config(|config| {
        config.permissions.set_permission_profile(PermissionProfile::Disabled).expect("root full access");
        config.agent_roles.insert("origin".into(),AgentRoleConfig { profile:Some("origin".parse().expect("valid origin profile name")),description:Some("Profile-backed origin worker".into()),config_file:None,nickname_candidates:None });
    }).build_with_auto_env(&server).await?;
    let root = test.session_configured.thread_id.to_string();
    let root_service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut events = root_service.subscribe();
    responses::mount_sse_once_match(&server,header("thread-id",root.as_str()),responses::sse(vec![responses::ev_function_call_with_namespace("origin-spawn","lifecycle_agents","spawn_agent",&json!({"task_name":"origin","agent_type":"origin","message":"Wait for the next authorized task.","fork_turns":"none"}).to_string()),responses::ev_completed("origin-spawn")])).await;
    responses::mount_sse_once_match(
        &server,
        header("thread-id", root.as_str()),
        answer("root-finished", "The origin worker started."),
    )
    .await;
    responses::mount_sse_once_match(
        &server,
        header("thread-id", root.as_str()),
        answer("root-summary", "The nested workflow completed."),
    )
    .await;
    responses::mount_sse_once_match(
        &server,
        header("thread-id", root.as_str()),
        answer("origin-notification", "The origin worker is ready."),
    )
    .await;
    let origin_root = root.clone();
    responses::mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            body(request)["client_metadata"]["x-codex-parent-thread-id"] == json!(origin_root)
        },
        answer("origin-ready", "Ready for the next authorized task."),
    )
    .await;
    let origin_root = root.clone();
    let source = super::workflow_v2::script(
        "return await workflow.agent('nested origin workflow result',{label:'nested'});",
    );
    responses::mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            body(request)["client_metadata"]["x-codex-parent-thread-id"] == json!(origin_root)
        },
        responses::sse(vec![
            responses::ev_function_call(
                "origin-workflow",
                "workflow_start",
                &json!({"source":source,"input":{}}).to_string(),
            ),
            responses::ev_completed("origin-workflow"),
        ]),
    )
    .await;
    let origin_root = root.clone();
    responses::mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            body(request)["client_metadata"]["x-codex-parent-thread-id"] == json!(origin_root)
        },
        answer("origin-finished", "The nested workflow is running."),
    )
    .await;
    let nested_root = root.clone();
    responses::mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            let request_body = body(request);
            request
                .headers
                .get("thread-id")
                .is_some_and(|id| id != nested_root.as_str())
                && request_body["client_metadata"]["x-codex-parent-thread-id"] != json!(nested_root)
                && request_body["input"]
                    .to_string()
                    .contains("nested origin workflow result")
        },
        answer("nested-result", r#"{"origin":"preserved"}"#),
    )
    .await;
    test.submit_text_turn("Start the profile-backed origin worker and its workflow.")
        .await?;
    let origin_request = requested(&server, |request| {
        request.body_json()["client_metadata"]["x-codex-parent-thread-id"] == json!(root)
    })
    .await;
    let origin_id = ThreadId::from_string(
        &origin_request
            .header("thread-id")
            .context("origin header")?,
    )?;
    let origin = test.thread_manager.get_thread(origin_id).await?;
    wait_for_event(&origin, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    // Runtime permissions are inherited from the initiating turn, so narrow the
    // worker through the supported host API before submitting its next task.
    origin
        .update_thread_settings(ThreadSettingsOverrides {
            permission_profile: Some(PermissionProfile::read_only()),
            ..Default::default()
        })
        .await?;
    let origin_config = origin.config_snapshot().await;
    let root_config = test.codex.config_snapshot().await;
    assert_eq!(
        origin_config.permission_profile,
        PermissionProfile::read_only()
    );
    assert_ne!(
        origin_config.permission_profile,
        root_config.permission_profile
    );
    let origin_policy = origin.config().await;
    assert!(
        origin_policy.features.enabled(Feature::MultiAgentV2),
        "the selected origin profile must already permit descendants"
    );
    assert!(
        !origin_policy.multi_agent_v2.disable_native_tools,
        "the selected origin profile must already permit native delegation"
    );
    origin
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "Start the nested origin workflow.".into(),
                text_elements: Vec::new(),
            }])
            .from_agent(),
        )
        .await?;
    let origin_request = requested(&server, |request| {
        from_thread(request, origin_id)
            && request.body_contains_text("Start the nested origin workflow.")
    })
    .await;
    let direct = origin_request.body_json()["tools"]
        .as_array()
        .is_some_and(|tools| declares_tool(tools, "workflow_start"));
    let incremental = origin_request
        .inputs_of_type("additional_tools")
        .iter()
        .any(|input| {
            input["tools"]
                .as_array()
                .is_some_and(|tools| declares_tool(tools, "workflow_start"))
        });
    assert!(
        direct || incremental,
        "origin must already advertise workflow launch through tools or additional_tools; do not promote a leaf role to satisfy this test"
    );
    let request = requested(&server, |request| {
        request.body_json()["client_metadata"]["x-codex-parent-thread-id"] == json!(origin_id)
            && request.body_contains_text("nested origin workflow result")
    })
    .await;
    let listed = root_service
        .list(/*offset*/ 0, /*limit*/ 50)
        .map_err(anyhow::Error::msg)?;
    assert_eq!(listed.len(), 1);
    let run = observe(&root_service, &mut events, &listed[0].id, |run| {
        run.status == WorkflowStatus::Succeeded
            && run.notification_delivered
            && run.lifecycle.terminal_ready
            && run.lifecycle.delivered.len() == run.lifecycle.events.len()
    })
    .await?;
    assert_eq!(run.owner_thread_id, root);
    assert_eq!(run.result, Some(json!({"origin":"preserved"})));
    let origin_service = origin.workflows().await.map_err(anyhow::Error::msg)?;
    assert!(
        Arc::ptr_eq(&root_service, &origin_service),
        "worker must use the owning root service"
    );
    assert_eq!(
        origin_service.get(&run.id).map_err(anyhow::Error::msg)?,
        run
    );
    let nested = test
        .thread_manager
        .get_thread(child_id(&run, "nested")?)
        .await?;
    let nested_config = nested.config_snapshot().await;
    assert_eq!(
        (
            nested_config.permission_profile,
            nested_config.reasoning_effort,
            nested_config.parent_thread_id
        ),
        (
            origin_config.permission_profile.clone(),
            Some(ReasoningEffort::Low),
            Some(origin_id)
        )
    );
    assert!(request.body_contains_text("origin profile authority marker"));
    assert_eq!(request.body_json()["reasoning"]["effort"], json!("low"));
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
