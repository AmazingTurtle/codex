//! Profile-backed roles must route model requests using the child's configuration.

#![allow(clippy::unwrap_used)]

use anyhow::Result;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_models_manager::bundled_models_response;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::sse_completed;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use test_case::test_case;

fn assert_worker_settings(
    request: &ResponsesRequest,
    model: &str,
    effort: &str,
    summary: &serde_json::Value,
) {
    let body = request.body_json();
    assert_eq!(
        (
            body["model"].clone(),
            body["reasoning"]["effort"].clone(),
            request.header("authorization")
        ),
        (
            json!(model),
            json!(effort),
            Some("Bearer spark-test-token".to_string())
        )
    );
    assert_eq!(&body["reasoning"]["summary"], summary);
    assert!(request.inputs_of_type("agent_message").is_empty());
    assert!(
        request
            .instructions_text()
            .contains("Spark system instructions.")
    );
}

fn tool_response(
    call_id: &str,
    namespace: &str,
    tool: &str,
    arguments: &serde_json::Value,
) -> String {
    sse(vec![
        ev_response_created(call_id),
        ev_function_call_with_namespace(call_id, namespace, tool, &arguments.to_string()),
        ev_completed(call_id),
    ])
}

#[test_case(false, false, false, "catalog", false; "v1 profile defaults")]
#[test_case(false, true, false, "catalog", false; "v1 explicit model")]
#[test_case(false, true, false, "catalog", true; "v1 closed profile agent reopens")]
#[test_case(true, false, false, "catalog", true; "v2 profile defaults")]
#[test_case(true, true, false, "catalog", true; "v2 explicit model")]
#[test_case(true, true, true, "catalog", true; "v2 role restrictions override profile and explicit model")]
#[test_case(true, false, false, "no-catalog", true; "v2 default model without catalog")]
#[test_case(false, true, false, "no-catalog", false; "v1 explicit default model without catalog")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_profiles_route_to_the_selected_provider(
    v2: bool,
    explicit_model: bool,
    locked_role: bool,
    profile_fixture: &str,
    resume_profile: bool,
) -> Result<()> {
    let parent_server = core_test_support::responses::start_mock_server().await;
    let child_server = core_test_support::responses::start_mock_server().await;
    let namespace = if v2 {
        "profile_agents"
    } else {
        "multi_agent_v1"
    };
    let has_catalog = profile_fixture == "catalog";
    let override_model = if has_catalog {
        "glm-test-override"
    } else {
        "glm-test-default"
    };
    let mut arguments = json!({"agent_type": "spark", "message": "Say hello."});
    if v2 {
        arguments["task_name"] = json!("spark_worker");
        arguments["fork_turns"] = json!("none");
    }
    if explicit_model {
        arguments["model"] = json!(override_model);
        arguments["reasoning_effort"] = json!("high");
    }
    let parent_responses = mount_sse_sequence(
        &parent_server,
        vec![
            tool_response("spawn-spark", namespace, "spawn_agent", &arguments),
            sse_completed("parent-done"),
        ],
    )
    .await;
    mount_sse_once(&parent_server, sse_completed("notification-done")).await;
    let child_response = mount_sse_once(&child_server, sse_completed("child-done")).await;
    let child_url = child_server.uri();
    let test = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_model_info_override("gpt-5.5", |model| {
            model.supports_search_tool = false;
            model.tool_mode = Some(codex_protocol::openai_models::ToolMode::Direct);
            model.model_messages = None;
        })
        .with_pre_build_hook(move |home| {
            std::fs::write(home.join("config.toml"), "[agents.spark]\ndescription = \"Local worker\"\nprofile = \"spark\"\n").unwrap();
            if locked_role {
                std::fs::write(home.join("config.toml"), "[agents.spark]\ndescription = \"Local worker\"\nprofile = \"spark\"\nconfig_file = \"./spark-role.toml\"\n").unwrap();
                std::fs::write(home.join("spark-role.toml"), "model = \"glm-test-default\"\nmodel_reasoning_effort = \"low\"\n").unwrap();
            }
            let mut model = bundled_models_response().unwrap().models.into_iter()
                .find(|model| model.slug == "gpt-5.5").unwrap();
            model.slug = "glm-test-default".to_string();
            model.default_reasoning_level = Some(ReasoningEffort::Medium);
            model.service_tiers.clear();
            model.model_messages = None;
            model.multi_agent_version = None;
            model.use_responses_lite = false;
            model.supports_reasoning_summary_parameter = true;
            let mut alternate = model.clone();
            alternate.slug = "glm-test-override".to_string();
            std::fs::write(home.join("spark-models.json"), serde_json::to_string(&ModelsResponse {models: vec![model, alternate]}).unwrap()).unwrap();
            std::fs::write(home.join("spark-instructions.md"), "Spark system instructions.").unwrap();
            let catalog_setting = if has_catalog { "model_catalog_json = \"./spark-models.json\"" } else { "" };
            std::fs::write(home.join("spark.config.toml"), format!(r#"
model = "glm-test-default"
model_provider = "spark"
model_reasoning_effort = "medium"
{catalog_setting}
model_instructions_file = "./spark-instructions.md"
developer_instructions = "Use the Spark profile instructions."
approval_policy = "never"
sandbox_mode = "danger-full-access"
[features]
shell_tool = false
[features.multi_agent_v2]
enabled = false
[model_providers.spark]
name = "Spark"
base_url = "{child_url}/v1"
wire_api = "responses"
experimental_bearer_token = "spark-test-token"
requires_openai_auth = false
"#)).unwrap();
        })
        .with_config(move |config| {
            config.model_reasoning_summary = Some(ReasoningSummary::Detailed);
            config.features.enable(Feature::Collab).unwrap();
            if v2 { config.features.enable(Feature::MultiAgentV2).unwrap(); }
            else { config.features.disable(Feature::MultiAgentV2).unwrap(); }
            config.multi_agent_v2.hide_spawn_agent_metadata = false;
        })
        .build_with_auto_env(&parent_server).await?;
    let mut created = test.thread_manager.subscribe_thread_created();
    test.submit_turn_with_approval_and_permission_profile(
        "Start the Spark worker.",
        AskForApproval::OnRequest,
        test.config.permissions.permission_profile().clone(),
    )
    .await?;
    let child_id =
        tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 10), created.recv())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "No child created: {}",
                    parent_responses.requests()[1].function_call_output("spawn-spark")
                )
            })??;
    let child = test.thread_manager.get_thread(child_id).await?;
    wait_for_event(&child, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    let request = child_response.single_request();
    assert!(request.body_contains_text("Say hello."));
    let expected_model = if explicit_model && !locked_role {
        override_model
    } else {
        "glm-test-default"
    };
    let expected_effort = if locked_role {
        "low"
    } else if explicit_model {
        "high"
    } else {
        "medium"
    };
    let captured_summary =
        parent_responses.requests()[0].body_json()["reasoning"]["summary"].clone();
    assert_worker_settings(&request, expected_model, expected_effort, &captured_summary);
    assert!(
        request
            .message_input_texts("developer")
            .iter()
            .any(|text| text.contains("Use the Spark profile instructions."))
    );
    assert!(request.tool_by_name("functions", "exec_command").is_none());
    assert!(
        request.body_json()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| {
                !matches!(
                    tool["name"].as_str(),
                    Some("spawn_agent" | "profile_agents")
                )
            })
    );
    let parent_snapshot = test.codex.config_snapshot().await;
    let child_snapshot = child.config_snapshot().await;
    assert_eq!(
        (
            child_snapshot.model_provider_id.as_str(),
            child_snapshot.approval_policy,
            &child_snapshot.permission_profile
        ),
        (
            "spark",
            parent_snapshot.approval_policy,
            &parent_snapshot.permission_profile
        )
    );
    let requests = parent_responses.requests();
    if v2 {
        assert!(
            requests[0]
                .tool_by_name("collaboration", "spawn_agent")
                .is_none()
        );
    }
    assert!(
        requests
            .iter()
            .all(|request| request.body_json()["model"] == json!("gpt-5.5"))
    );
    let spawn = requests[0].tool_by_name(namespace, "spawn_agent").unwrap();
    assert_ne!(
        spawn["parameters"]["properties"]["message"]["encrypted"],
        json!(true)
    );
    if v2 {
        for tool in ["send_message", "followup_task"] {
            let spec = requests[0].tool_by_name(namespace, tool).unwrap();
            assert_ne!(
                spec["parameters"]["properties"]["message"]["encrypted"],
                json!(true)
            );
        }
    }
    assert!(
        spawn["parameters"]["properties"]["agent_type"]["description"]
            .as_str()
            .unwrap()
            .contains("glm-test-default")
    );
    child.shutdown_and_wait().await?;
    test.codex.shutdown_and_wait().await?;
    if resume_profile {
        let resume_server = core_test_support::responses::start_mock_server().await;
        let (method, args) = if v2 {
            (
                "followup_task",
                json!({"target": "spark_worker", "message": "Say hello again."}),
            )
        } else {
            ("resume_agent", json!({"id": child_id.to_string()}))
        };
        let mut responses = vec![tool_response("resume-spark", namespace, method, &args)];
        if !v2 {
            responses.push(tool_response(
                "send-spark",
                namespace,
                "send_input",
                &json!({"target": child_id.to_string(), "message": "Say hello again."}),
            ));
        }
        responses.push(sse_completed("resume-done"));
        let resume_requests = mount_sse_sequence(&resume_server, responses).await;
        mount_sse_once(&resume_server, sse_completed("resume-notification")).await;
        let resumed_response = mount_sse_once(&child_server, sse_completed("resumed-child")).await;
        let resumed = test_codex()
            .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
            .with_model_info_override("gpt-5.5", |model| {
                model.supports_search_tool = false;
                model.model_messages = None;
            })
            .with_config(move |config| {
                config.features.enable(Feature::Collab).unwrap();
                if v2 {
                    config.features.enable(Feature::MultiAgentV2).unwrap();
                } else {
                    config.features.disable(Feature::MultiAgentV2).unwrap();
                }
            })
            .resume(
                &resume_server,
                test.home.clone(),
                test.session_configured.rollout_path.clone().unwrap(),
            )
            .await?;
        resumed.submit_turn("Continue the Spark worker.").await?;
        let reloaded = resumed.thread_manager.get_thread(child_id).await?;
        wait_for_event(&reloaded, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;
        let request = resumed_response.single_request();
        assert!(request.body_contains_text("Say hello again."));
        let resumed_summary =
            resume_requests.requests()[0].body_json()["reasoning"]["summary"].clone();
        assert_worker_settings(
            &request,
            &child_snapshot.model,
            expected_effort,
            &resumed_summary,
        );
        assert_eq!(
            reloaded.config_snapshot().await.reasoning_effort,
            child_snapshot.reasoning_effort
        );
        reloaded.shutdown_and_wait().await?;
        resumed.codex.shutdown_and_wait().await?;
    }
    Ok(())
}

#[test_case("missing"; "missing profile")]
#[test_case("malformed"; "malformed profile")]
#[test_case("oversized"; "oversized model instructions")]
#[test_case("all"; "provider switch rejects full history")]
#[test_case("1"; "provider switch rejects partial history")]
#[test_case("unknown-model"; "catalog fallback rejects unconfigured model")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_profiles_fail_before_spawning_when_unavailable(failure: &str) -> Result<()> {
    let failure = failure.to_string();
    let expected_error = if failure == "unknown-model" {
        "Unknown model `profile-only-model`"
    } else {
        "agent profile `spark`"
    };
    let fork_turns = match failure.as_str() {
        "all" | "1" => failure.as_str(),
        _ => "none",
    };
    let server = core_test_support::responses::start_mock_server().await;
    let response = mount_sse_sequence(&server, vec![
        tool_response("invalid-profile", "profile_agents", "spawn_agent", &json!({"agent_type": "spark", "message": "Hello.", "model": "profile-only-model", "task_name": "worker", "fork_turns": fork_turns})),
        sse_completed("done"),
    ]).await;
    let test = test_codex()
        .with_pre_build_hook(move |home| {
            std::fs::write(
                home.join("config.toml"),
                "[agents.spark]\ndescription = \"Local worker\"\nprofile = \"spark\"\n",
            )
            .unwrap();
            if failure == "malformed" {
                std::fs::write(home.join("spark.config.toml"), "model = [").unwrap();
            } else if failure == "oversized" {
                std::fs::write(home.join("spark.config.toml"), "model_instructions_file = \"./oversized.md\"\n").unwrap();
                std::fs::write(home.join("oversized.md"), "instruction ".repeat(12_000)).unwrap();
            } else if matches!(failure.as_str(), "all" | "1" | "unknown-model") {
                std::fs::write(home.join("spark.config.toml"), "model = \"glm-test-default\"\nmodel_provider = \"spark\"\n[model_providers.spark]\nname = \"Spark\"\nbase_url = \"http://127.0.0.1:9/v1\"\nwire_api = \"responses\"\n").unwrap();
            }
        })
        .with_config(|config| {
            config.features.enable(Feature::MultiAgentV2).unwrap();
        })
        .build_with_auto_env(&server)
        .await?;
    let mut created = test.thread_manager.subscribe_thread_created();
    test.submit_turn("Try the unavailable worker.").await?;
    let requests = response.requests();
    let error = requests[1].function_call_output("invalid-profile")["output"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(error.contains(expected_error), "unexpected error: {error}");
    assert!(matches!(
        created.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
