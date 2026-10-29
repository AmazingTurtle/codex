//! Exercises the model workflow tool through the production V8 service and agent controller.

use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_core::config::Config;
use codex_core::workflows::WorkflowRun;
use codex_core::workflows::WorkflowStatus;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::ToolFinishInput;
use codex_extension_api::ToolLifecycleContributor;
use codex_extension_api::ToolLifecycleFuture;
use codex_features::DynamicWorkflowsMode;
use codex_features::Feature;
use codex_protocol::ThreadId;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::Op;
use codex_protocol::turn_input::TurnInputSubmission;
use codex_protocol::user_input::UserInput;
use codex_skills_extension::HostSkillsLoadInput;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use test_case::test_case;
use tokio::sync::oneshot;
use wiremock::matchers::header;

// The provider asks the first real worker to use a normal tool. Its post-use hook
// holds the result until the parent has finished, making the completion wake causal.
struct WorkerGate(Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>);

impl ToolLifecycleContributor for WorkerGate {
    fn on_tool_finish<'a>(&'a self, input: ToolFinishInput<'a>) -> ToolLifecycleFuture<'a> {
        Box::pin(async move {
            if input.tool_name.name != "test_sync_tool" {
                return;
            }
            let gate = self.0.lock().expect("worker gate lock").take();
            if let Some((reached, release)) = gate {
                reached.send(()).expect("signal reached worker gate");
                release.await.expect("release worker gate");
            }
        })
    }
}

async fn terminal(
    service: &codex_core::workflows::DynamicWorkflowService,
    events: &mut tokio::sync::broadcast::Receiver<WorkflowRun>,
) -> Result<WorkflowRun> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            // Broadcast events are wakeups, not a lossless state history. ACK
            // updates can coalesce; always read the current durable run state.
            if let Some(run) = service
                .list(/*offset*/ 0, /*limit*/ 50)
                .map_err(anyhow::Error::msg)?
                .into_iter()
                .find(|run| {
                    matches!(
                        run.status,
                        WorkflowStatus::Succeeded
                            | WorkflowStatus::Failed
                            | WorkflowStatus::Interrupted
                    )
                })
            {
                return Ok::<_, anyhow::Error>(run);
            }
            match events.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_workflow_queues_real_children_and_keeps_results_out_of_parent_context() -> Result<()>
{
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let (reached, worker_reached) = oneshot::channel();
    let (release, worker_release) = oneshot::channel();
    let mut extensions = ExtensionRegistryBuilder::<Config>::new();
    extensions.tool_lifecycle_contributor(Arc::new(WorkerGate(Mutex::new(Some((
        reached,
        worker_release,
    ))))));
    let test = test_codex()
        .with_model("gpt-5.6-sol")
        .with_extensions(Arc::new(extensions.build()))
        .with_model_info_override("gpt-5.6-sol", |model| {
            model.tool_mode = Some(ToolMode::Direct);
            model
                .experimental_supported_tools
                .push("test_sync_tool".into());
        })
        .with_config(|config| {
            for feature in [
                Feature::Collab,
                Feature::CodeModeHost,
                Feature::DynamicWorkflows,
            ] {
                config
                    .features
                    .enable(feature)
                    .expect("enable workflow test feature");
            }
            config.multi_agent_v2.non_code_mode_only = false;
            config.multi_agent_v2.max_concurrent_threads_per_session = 2;
            config.dynamic_workflows_mode = DynamicWorkflowsMode::Auto;
        })
        .build_with_auto_env(&server)
        .await?;
    let root = test.session_configured.thread_id.to_string();
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut events = service.subscribe();
    let source = super::workflow_v2::script(
        r#"return await workflow.parallel(["first", "second", "third"].map(label => () => workflow.agent("Return a concise JSON result.", {label})));"#,
    );
    responses::mount_sse_once_match(
        &server,
        header("thread-id", root.as_str()),
        responses::sse(vec![
            responses::ev_function_call(
                "launch",
                "workflow_start",
                &json!({"source":source,"input":{}}).to_string(),
            ),
            responses::ev_completed("launch-response"),
        ]),
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
        responses::sse(vec![
            responses::ev_function_call("held-worker", "test_sync_tool", "{}"),
            responses::ev_completed("held-worker"),
        ]),
    )
    .await;
    let parent_completion = responses::mount_sse_once_match(
        &server,
        header("thread-id", root.as_str()),
        responses::sse(vec![
            responses::ev_assistant_message("parent-done", "The workflow is running."),
            responses::ev_completed("parent-done"),
        ]),
    )
    .await;
    let mut child_responses = Vec::new();
    for _ in 0..3 {
        let parent = root.clone();
        child_responses.push(
            responses::mount_sse_once_match(
                &server,
                move |request: &wiremock::Request| {
                    request
                        .headers
                        .get("thread-id")
                        .is_some_and(|id| id != parent.as_str())
                },
                responses::sse(vec![
                    responses::ev_assistant_message(
                        "worker-result",
                        r#"{"workflowOnlySecret":"worker-result"}"#,
                    ),
                    responses::ev_completed("worker-done"),
                ]),
            )
            .await,
        );
    }
    test.submit_text_turn("Coordinate this work as a workflow.")
        .await?;
    let launch_output = parent_completion
        .single_request()
        .function_call_output("launch")
        .to_string();
    let reached = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), worker_reached).await;
    assert!(
        reached.is_ok(),
        "worker gate was not reached; launch output: {launch_output}; run states: {:?}",
        service
            .list(/*offset*/ 0, /*limit*/ 5)
            .map_err(anyhow::Error::msg)?
            .iter()
            .map(|run| (&run.status, &run.error, &run.steps))
            .collect::<Vec<_>>()
    );
    reached??;
    release
        .send(())
        .expect("release real worker after parent completion");
    let run = terminal(&service, &mut events).await?;
    assert_eq!(run.status, WorkflowStatus::Succeeded, "{:?}", run.error);
    assert_eq!(run.owner_thread_id, root);
    assert_eq!(run.steps.len(), 3);
    assert_eq!(
        run.result,
        Some(json!([
            {"workflowOnlySecret":"worker-result"}, {"workflowOnlySecret":"worker-result"}, {"workflowOnlySecret":"worker-result"},
        ]))
    );
    assert!(
        parent_completion
            .single_request()
            .function_call_output("launch")
            .to_string()
            .len()
            < 3200
    );
    for response in child_responses {
        assert!(
            json!(response.single_request().input())
                .to_string()
                .contains("Return a concise JSON result.")
        );
    }
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30),wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::WorkflowLifecycle(fact) if fact.run_id == run.id
            && matches!(&fact.kind,codex_protocol::workflow::WorkflowLifecycleKind::Completed { .. }))
    })).await?;
    assert_eq!(
        responses::received_responses_requests(&server)
            .await
            .iter()
            .filter(|request| request.header("thread-id").as_deref() == Some(root.as_str()))
            .count(),
        2,
        "default display completion must not wake the parent model"
    );
    let expected = super::workflow_v2::observe(&service, &run.id, |run| {
        run.lifecycle.delivered.len() == run.lifecycle.events.len() && run.lifecycle.terminal_ready
    })
    .await?;
    let continued = responses::mount_sse_once_match(
        &server,
        header("thread-id", root.as_str()),
        responses::sse(vec![responses::ev_completed("continued")]),
    )
    .await;
    test.submit_text_turn("Continue our main conversation.")
        .await?;
    assert!(
        !json!(continued.single_request().input())
            .to_string()
            .contains("workflowOnlySecret")
    );
    assert!(
        !json!(continued.single_request().input())
            .to_string()
            .contains("<workflow_status>"),
        "a default display result must not become even non-triggering model mail"
    );
    assert_eq!(
        service.get(&expected.id).map_err(anyhow::Error::msg)?,
        expected
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[derive(Clone, Copy)]
enum WorkflowSkillSelection {
    None,
    Launch,
    Reference,
}

#[test_case(WorkflowSkillSelection::None; "ordinary_human_input_cannot_activate_explicit_mode")]
#[test_case(WorkflowSkillSelection::Launch; "selected_human_launch_skill_activates_explicit_mode")]
#[test_case(WorkflowSkillSelection::Reference; "documentation_mentions_do_not_grant_execution")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_model_launch_requires_current_human_skill_selection(
    selection: WorkflowSkillSelection,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let selected = matches!(selection, WorkflowSkillSelection::Launch);
    let selected_name = match selection {
        WorkflowSkillSelection::None => None,
        WorkflowSkillSelection::Launch => Some("dynamic-workflow"),
        WorkflowSkillSelection::Reference => Some("reference-only"),
    };
    let server = responses::start_mock_server().await;
    let test = test_codex().with_model("gpt-5.6-sol")
        .with_model_info_override("gpt-5.6-sol", |model| model.tool_mode = Some(ToolMode::Direct))
        .with_config(|config| {
            for feature in [Feature::Collab,Feature::MultiAgentV2,Feature::CodeModeHost,Feature::DynamicWorkflows] {
                config.features.enable(feature).expect("enable workflow test feature");
            }
            config.multi_agent_v2.non_code_mode_only = false;
            config.dynamic_workflows_mode = DynamicWorkflowsMode::Explicit;
            codex_skills::install_system_skills(&config.codex_home).expect("install the real workflow skill");
            let directory = config.cwd.join(".agents/skills/reference-only");
            std::fs::create_dir_all(&directory).expect("create test reference directory");
            std::fs::write(directory.join("SKILL.md"), "---\nname: reference-only\ndescription: Explain workflow documentation without launching.\n---\nThe $dynamic-workflow documentation describes workflow_start. Reading it does not authorize a launch.").expect("write private test reference");
        }).build_with_auto_env(&server).await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut events = service.subscribe();
    let first = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_function_call(
                "explicit-launch",
                "workflow_start",
                &json!({"source":super::workflow_v2::script("return {ok:true};")}).to_string(),
            ),
            responses::ev_completed("explicit-response"),
        ]),
    )
    .await;
    let second = responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("parent-finished")]),
    )
    .await;
    responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("optional-workflow-wake")]),
    )
    .await;
    let mut input = vec![UserInput::Text {
        text: "Please perform the requested work.".into(),
        text_elements: Vec::new(),
    }];
    if let Some(name) = selected_name {
        input.push(UserInput::Skill {
            name: name.into(),
            path: if selected {
                codex_skills::system_cache_root_dir(&test.config.codex_home)
                    .join("dynamic-workflow/SKILL.md")
                    .to_path_buf()
            } else {
                test.config
                    .cwd
                    .join(".agents/skills")
                    .join(name)
                    .join("SKILL.md")
                    .to_path_buf()
            },
        });
    }
    let skills = test
        .thread_manager
        .skills_service()
        .snapshot_for_config(
            &HostSkillsLoadInput::new(
                test.config.cwd.clone(),
                Vec::new(),
                test.config.config_layer_stack.clone(),
            ),
            Some(test.fs()),
        )
        .await;
    let selected_skills = codex_skills::collect_explicit_skill_mentions(
        &input,
        skills.outcome(),
        &std::collections::HashMap::new(),
    );
    if let Some(name) = selected_name {
        assert!(
            selected_skills.iter().any(|skill| skill.name == name),
            "the requested skill must actually resolve before testing its execution authority"
        );
    }
    assert_eq!(
        selected_skills
            .iter()
            .any(|skill| skill.name == "dynamic-workflow"),
        selected,
        "the actual selected skill must resolve before activation; discovered workflow skills: {:?}; load errors: {:?}",
        skills
            .outcome()
            .skills
            .iter()
            .filter(|skill| skill.name == "dynamic-workflow")
            .map(|skill| &skill.path_to_skills_md)
            .collect::<Vec<_>>(),
        skills.outcome().errors
    );
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(input))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let request = first.single_request();
    let mut declarations = vec![request.body_json()["tools"].clone()];
    declarations.extend(
        request
            .inputs_of_type("additional_tools")
            .into_iter()
            .map(|item| item["tools"].clone()),
    );
    let mut launch_exposed = false;
    while let Some(layer) = declarations.pop() {
        let Some(tools) = layer.as_array() else {
            continue;
        };
        for tool in tools {
            launch_exposed |= tool["name"].as_str() == Some("workflow_start");
            if let Some(nested) = tool.get("tools") {
                declarations.push(nested.clone());
            }
        }
    }
    assert_eq!(
        launch_exposed,
        selected,
        "resolved workflow skill instruction present: {}; launch output: {}",
        json!(first.single_request().input())
            .to_string()
            .contains("Reading it does not authorize a launch."),
        second
            .single_request()
            .function_call_output("explicit-launch")
    );
    if selected {
        let run = terminal(&service, &mut events).await?;
        assert_eq!(
            (run.status, run.result),
            (WorkflowStatus::Succeeded, Some(json!({"ok":true})))
        );
    } else {
        assert!(service.list(0, 5).map_err(anyhow::Error::msg)?.is_empty());
        assert!(
            second
                .single_request()
                .function_call_output("explicit-launch")
                .to_string()
                .contains("workflow_start")
        );
    }
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_reads_a_published_report_in_bounded_pages_without_losing_json() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = test_codex()
        .with_model("gpt-5.6-sol")
        .with_model_info_override("gpt-5.6-sol", |model| {
            model.tool_mode = Some(ToolMode::Direct)
        })
        .with_config(|config| {
            config
                .features
                .enable(Feature::CodeModeHost)
                .expect("enable real runtime");
            config.multi_agent_v2.non_code_mode_only = false;
        })
        .build_with_auto_env(&server)
        .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut events = service.subscribe();
    let expected = json!({"report":format!("{}Unicode ending: λ", "Report line with escaped newline.\n".repeat(100)),"metadata":{"complete":true}});
    let run = service
        .start(
            super::workflow_v2::script(&format!("return {expected};")),
            json!({}),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let completed = terminal(&service, &mut events).await?;
    assert_eq!(completed.result, Some(expected.clone()));
    let mut offset = 0;
    let mut report = String::new();
    let mut pages = 0;
    loop {
        assert!(
            pages < 32,
            "paging must terminate within the bounded result size"
        );
        let call = format!("read-result-{pages}");
        responses::mount_sse_once(
            &server,
            responses::sse(vec![
                responses::ev_function_call(
                    &call,
                    "workflow_read",
                    &json!({"run_id":run.id,"section":"result","offset":offset}).to_string(),
                ),
                responses::ev_completed(&format!("read-page-{pages}")),
            ]),
        )
        .await;
        let read = responses::mount_sse_once(
            &server,
            responses::sse(vec![responses::ev_completed(&format!("page-read-{pages}"))]),
        )
        .await;
        test.submit_text_turn("Read the next page of the workflow result.")
            .await?;
        let output = read.single_request().function_call_output(&call)["output"].clone();
        let text = match output {
            serde_json::Value::String(text) => text,
            serde_json::Value::Array(items) => items
                .iter()
                .filter_map(|item| item["text"].as_str())
                .collect::<String>(),
            other => anyhow::bail!("unexpected workflow tool output: {other}"),
        };
        assert!(
            text.len() <= 3200,
            "result pages must stay within the parent context budget"
        );
        let page: serde_json::Value = serde_json::from_str(&text)?;
        assert_eq!(page["page"]["offset"], json!(offset));
        report.push_str(
            page["page"]["content"]
                .as_str()
                .expect("published JSON fragment"),
        );
        pages += 1;
        match page["page"]["next_offset"].as_u64() {
            Some(next) => {
                assert!(next > offset as u64);
                offset = usize::try_from(next)?;
            }
            None => break,
        }
    }
    assert!(pages > 2);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&report)?,
        expected
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn escaped_metadata_pages_remain_valid_json_and_preserve_every_step_id() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = test_codex()
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
                    .expect("enable native metadata workers");
            }
            config.multi_agent_v2.non_code_mode_only = false;
        })
        .build_with_auto_env(&server)
        .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut events = service.subscribe();
    let ids = (0..7)
        .map(|index| format!("{}{index}", "\0".repeat(127)))
        .collect::<Vec<_>>();
    let phase = "\u{2}".repeat(128);
    // The provider rejects valid native attempts authoritatively. V2 option
    // validation happens before step creation, so invalid options cannot seed
    // the failed-step metadata whose JSON escaping this scenario exercises.
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path_regex(".*/responses$"))
        .respond_with(wiremock::ResponseTemplate::new(400).set_body_json(json!({
            "error":{"type":"invalid_request_error","code":"invalid_prompt","message":"\u{1}".repeat(128)},
        })))
        .up_to_n_times(ids.len() as u64)
        .expect(ids.len() as u64)
        .mount(&server).await;
    let run = service.start(
        super::workflow_v2::script_with_phases(&[phase.as_str()], "workflow.phase(workflow.args.phase); await workflow.parallel(workflow.args.ids.map(label => () => workflow.agent('metadata test confirmed provider failure', {label}))); return workflow.args.preview;"),
        json!({"ids":ids,"phase":phase,"preview":"\u{3}".repeat(256)}),
    ).await.map_err(anyhow::Error::msg)?;
    let completed = terminal(&service, &mut events).await?;
    assert_eq!(
        completed.status,
        WorkflowStatus::Succeeded,
        "{:?}",
        completed.error
    );
    assert_eq!(completed.steps.len(), ids.len());
    assert_eq!(
        responses::received_responses_requests(&server).await.len(),
        ids.len()
    );
    let mut labels = completed
        .steps
        .values()
        .map(|step| step.label.clone())
        .collect::<Vec<_>>();
    labels.sort();
    assert_eq!(labels, ids);
    let expected_ids = completed.steps.keys().cloned().collect::<Vec<_>>();
    assert!(
        completed
            .steps
            .values()
            .all(
                |step| step.attempts.last().is_some_and(|attempt| attempt.status
                    == WorkflowStatus::Failed
                    && attempt
                        .error
                        .as_ref()
                        .is_some_and(|error| error.contains('\u{1}')))
            )
    );
    let mut offset = 0;
    let mut recovered = Vec::new();
    let mut pages = 0;
    loop {
        assert!(
            pages < ids.len(),
            "each page must advance over at least one exact identifier"
        );
        let call = format!("read-metadata-{pages}");
        responses::mount_sse_once(
            &server,
            responses::sse(vec![
                responses::ev_function_call(
                    &call,
                    "workflow_read",
                    &json!({"run_id":run.id,"offset":offset}).to_string(),
                ),
                responses::ev_completed(&format!("metadata-call-{pages}")),
            ]),
        )
        .await;
        let read = responses::mount_sse_once(
            &server,
            responses::sse(vec![responses::ev_completed(&format!(
                "metadata-read-{pages}"
            ))]),
        )
        .await;
        test.submit_text_turn("Read the next workflow metadata page.")
            .await?;
        let output = read.single_request().function_call_output(&call)["output"].clone();
        let text = match output {
            serde_json::Value::String(text) => text,
            serde_json::Value::Array(items) => items
                .iter()
                .filter_map(|item| item["text"].as_str())
                .collect::<String>(),
            other => anyhow::bail!("unexpected workflow metadata output: {other}"),
        };
        assert!(
            text.len() <= 3200,
            "JSON escaping counts toward the complete output budget"
        );
        let page: serde_json::Value = serde_json::from_str(&text)?;
        let steps = page["steps"].as_array().expect("successful metadata page");
        assert!(!steps.is_empty());
        recovered.extend(steps.iter().map(|step| {
            step["id"]
                .as_str()
                .expect("exact step identifier")
                .to_string()
        }));
        pages += 1;
        match page["next_offset"].as_u64() {
            Some(next) => {
                assert_eq!(
                    usize::try_from(next)?,
                    recovered.len(),
                    "offset must count included entries rather than attempted entries"
                );
                offset = usize::try_from(next)?;
            }
            None => break,
        }
    }
    assert!(
        pages > 1,
        "escaped identifiers must shrink the five-entry metadata page"
    );
    recovered.sort();
    assert_eq!(recovered, expected_ids);
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_then_human_restart_keeps_the_same_workflow_attempt_unsettled() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let (release, gate) = oneshot::channel();
    let (streaming, _) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("original-child")]),
            },
            StreamingSseChunk {
                gate: Some(gate),
                body: responses::sse(vec![responses::ev_completed("original-child")]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_response_created("human-continuation"),
                responses::ev_assistant_message("guided-result", r#"{"guided":true}"#),
                responses::ev_completed("human-continuation"),
            ]),
        }],
    ])
    .await;
    let server = responses::start_mock_server().await;
    let endpoint = format!("{}/v1", streaming.uri());
    let test = test_codex()
        .with_model("gpt-5.6-sol")
        .with_model_info_override("gpt-5.6-sol", |model| {
            model.tool_mode = Some(ToolMode::Direct)
        })
        .with_config(move |config| {
            config
                .features
                .enable(Feature::CodeModeHost)
                .expect("enable real runtime");
            config.multi_agent_v2.non_code_mode_only = false;
            config.model_provider.base_url = Some(endpoint);
        })
        .build_with_auto_env(&server)
        .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut events = service.subscribe();
    let run = service.start(super::workflow_v2::script("return await workflow.agent('Retain the original workflow task.',{label:'guided'});"), json!({})).await.map_err(anyhow::Error::msg)?;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        streaming.wait_for_request_count(/*count*/ 1),
    )
    .await?;
    let child_id = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        loop {
            let run = service.get(&run.id).map_err(anyhow::Error::msg)?;
            if let Some(id) = run
                .steps
                .values()
                .find(|step| step.label == "guided")
                .and_then(|step| step.attempts.last())
                .and_then(|attempt| attempt.child_thread_id.as_ref())
            {
                break Result::<ThreadId>::Ok(ThreadId::from_string(id)?);
            }
            events.recv().await?;
        }
    })
    .await??;
    let child = test.thread_manager.get_thread(child_id).await?;
    let baseline = child.config_snapshot().await;
    // This is the advertised Esc-and-send boundary: an explicit abort is observed
    // before a separate human start. It must not settle a live workflow attempt.
    child.submit(Op::Interrupt).await?;
    wait_for_event(&child, |event| matches!(event, EventMsg::TurnAborted(_))).await;
    child.flush_rollout().await?;
    assert_eq!(child.agent_status().await, AgentStatus::Interrupted);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(/*millis*/ 150),
            terminal(&service, &mut events)
        )
        .await
        .is_err(),
        "a live interrupted worker must await its human continuation rather than fail the workflow"
    );
    let interrupted = service.get(&run.id).map_err(anyhow::Error::msg)?;
    assert_eq!(
        (
            interrupted.status,
            interrupted
                .steps
                .values()
                .find(|step| step.label == "guided")
                .expect("authored guidance step")
                .attempts
                .len()
        ),
        (WorkflowStatus::Running, 1)
    );
    let submission = child
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Human Esc-and-send guidance: include ACTIVE_ESC_GUIDANCE.".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    assert!(matches!(submission, TurnInputSubmission::Started { .. }));
    let completed = terminal(&service, &mut events).await?;
    assert_eq!(
        completed.status,
        WorkflowStatus::Succeeded,
        "{:?}",
        completed.error
    );
    assert_eq!(completed.result, Some(json!({"guided":true})));
    let guided = completed
        .steps
        .values()
        .find(|step| step.label == "guided")
        .expect("authored guidance step");
    assert_eq!(guided.attempts.len(), 1);
    assert_eq!(
        guided.attempts[0].child_thread_id.as_deref(),
        Some(child_id.to_string().as_str())
    );
    let after = child.config_snapshot().await;
    assert_eq!(
        (
            after.model,
            after.reasoning_effort,
            after.permission_profile
        ),
        (
            baseline.model,
            baseline.reasoning_effort,
            baseline.permission_profile
        )
    );
    let requests = streaming
        .requests()
        .await
        .into_iter()
        .map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]["input"]
            .to_string()
            .contains("ACTIVE_ESC_GUIDANCE")
    );
    assert_eq!(
        requests[0]["client_metadata"]["thread_id"],
        requests[1]["client_metadata"]["thread_id"]
    );
    drop(release);
    test.codex.shutdown_and_wait().await?;
    streaming.shutdown().await;
    Ok(())
}
