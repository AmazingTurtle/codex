//! Workflow acceptance through real JSON-RPC routing, V8 execution, and disk persistence.

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence;
use codex_app_server_protocol::ConfigBatchWriteParams;
use codex_app_server_protocol::ConfigEdit;
use codex_app_server_protocol::ConfigWriteResponse;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::MergeStrategy;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadExecutionControl;
use codex_app_server_protocol::ThreadExecutionControlReadResponse;
use codex_app_server_protocol::ThreadPauseResponse;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::TurnSteerResponse;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowReadResponse;
use codex_app_server_protocol::WorkflowRecoverResponse;
use codex_app_server_protocol::WorkflowRetryResponse;
use codex_app_server_protocol::WorkflowRun;
use codex_app_server_protocol::WorkflowSaveResponse;
use codex_app_server_protocol::WorkflowSavedDeleteResponse;
use codex_app_server_protocol::WorkflowSavedListResponse;
use codex_app_server_protocol::WorkflowSavedReadResponse;
use codex_app_server_protocol::WorkflowScope;
use codex_app_server_protocol::WorkflowStartResponse;
use codex_app_server_protocol::WorkflowStatus;
use codex_app_server_protocol::WorkflowStopResponse;
use codex_app_server_protocol::WorkflowUpdatedNotification;
use codex_core::config::ConfigBuilder;
use codex_core::test_support::construct_model_info_offline;
use codex_features::Feature;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::ExecutionControlState;
use codex_protocol::protocol::MultiAgentVersion;
use core_test_support::responses;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use pretty_assertions::assert_eq;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(/*secs*/ 30);
const SCRIPT: &str = "workflow.meta({name:'answer',description:'Publish the requested answer',phases:[{title:'publish'}]}); await workflow.phase('publish'); await workflow.log('Publishing answer'); return {answer: workflow.args.value};";

pub(super) async fn rpc<T: DeserializeOwned>(
    app: &mut TestAppServer,
    method: &str,
    params: Value,
) -> Result<T> {
    let request = app.send_request(method, Some(params)).await?;
    timeout(WAIT, app.read_response(request)).await?
}

async fn rejected(app: &mut TestAppServer, method: &str, params: Value) -> Result<JSONRPCError> {
    let request = app.send_request(method, Some(params)).await?;
    let error = timeout(
        WAIT,
        app.read_stream_until_error_message(RequestId::Integer(request)),
    )
    .await??;
    assert_eq!(error.error.code, -32600);
    Ok(error)
}

pub(super) async fn terminal(app: &mut TestAppServer, workflow_id: &str) -> Result<WorkflowRun> {
    timeout(WAIT, async {
        loop {
            let update: WorkflowUpdatedNotification =
                app.read_notification("workflow/updated").await?;
            if update.workflow.id == workflow_id
                && matches!(
                    update.workflow.status,
                    WorkflowStatus::Succeeded
                        | WorkflowStatus::Failed
                        | WorkflowStatus::Stopped
                        | WorkflowStatus::Interrupted
                )
            {
                return Ok(update.workflow);
            }
        }
    })
    .await?
}

pub(super) async fn client(home: &TempDir) -> Result<TestAppServer> {
    TestAppServer::builder()
        .with_codex_home(home.path())
        .without_managed_config()
        .build_initialized()
        .await
}

pub(super) async fn configure_agent_workflows(
    home: &TempDir,
    server: &wiremock::MockServer,
) -> Result<()> {
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(codex_config::LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await?;
    let mut model = construct_model_info_offline("gpt-5.6-sol", &config);
    model.multi_agent_version = Some(MultiAgentVersion::V2);
    model.tool_mode = Some(ToolMode::Direct);
    model.supported_reasoning_levels = vec![codex_protocol::openai_models::ReasoningEffortPreset {
        effort: codex_protocol::openai_models::ReasoningEffort::Low,
        description: "Low reasoning effort for the controlled worker fixture".to_string(),
    }];
    let catalog = home.path().join("workflow-models.json");
    std::fs::write(&catalog, serde_json::to_vec(&json!({"models":[model]}))?)?;
    MockResponsesConfig::new(&server.uri())
        .with_model("gpt-5.6-sol")
        .enable_feature(Feature::Collab)
        .enable_feature(Feature::MultiAgentV2)
        .enable_feature(Feature::CodeModeHost)
        .enable_feature(Feature::DynamicWorkflows)
        .with_root_config(&format!(
            "model_catalog_json = {}",
            serde_json::to_string(&catalog)?
        ))
        .write(home.path())?;
    let config_path = home.path().join("config.toml");
    let mut config: toml::Value = toml::from_str(&std::fs::read_to_string(&config_path)?)?;
    config["features"]["multi_agent_v2"] =
        toml::Value::try_from(json!({"enabled":true,"non_code_mode_only":false}))?;
    std::fs::write(config_path, toml::to_string(&config)?)?;
    Ok(())
}

#[tokio::test]
async fn workflows_publish_save_paginate_and_resume_from_disk() -> Result<()> {
    let home = TempDir::new()?;
    let project = TempDir::new()?;
    let server =
        create_mock_responses_server_sequence(vec![create_final_assistant_message_sse_response(
            "The conversation stays available",
        )?])
        .await;
    configure_agent_workflows(&home, &server).await?;
    let mut app = client(&home).await?;
    let thread_id = app
        .start_thread(ThreadStartParams {
            cwd: Some(project.path().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?
        .thread
        .id;
    let before_start = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
    )?;
    let started: WorkflowStartResponse = rpc(
        &mut app,
        "workflow/start",
        json!({"threadId":thread_id,"source":SCRIPT,"input":{"value":42}}),
    )
    .await?;
    let run = terminal(&mut app, &started.workflow.id).await?;
    let after_terminal = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
    )?;
    assert_eq!(run.status, WorkflowStatus::Succeeded);
    assert_eq!(run.result, Some(json!({"answer":42})));
    assert_eq!(run.phase.as_deref(), Some("publish"));
    assert_eq!(run.phases, vec!["publish".to_string()]);
    assert_eq!(run.format_version, 2);
    assert!(!run.read_only);
    assert_eq!(
        run.metadata.as_ref().expect("declared metadata").name,
        "answer"
    );
    let mut phase_progress = run.phase_progress.clone();
    for phase in &mut phase_progress {
        let completed_at = phase.completed_at.expect("the visited phase completed");
        assert_eq!(phase.updated_at, Some(completed_at));
        assert!((before_start..=after_terminal).contains(&completed_at));
        // Normalize only independently validated timestamps for the full comparison.
        phase.updated_at = Some(0);
        phase.completed_at = Some(0);
    }
    assert_eq!(
        phase_progress,
        vec![codex_app_server_protocol::WorkflowPhaseProgress {
            title: "publish".into(),
            status: codex_app_server_protocol::WorkflowPhaseStatus::Completed,
            steps_discovered: 0,
            steps_completed: 0,
            metrics: codex_app_server_protocol::WorkflowMetrics {
                usage: Some(codex_app_server_protocol::WorkflowTokenUsage::default()),
                usage_complete: true,
                usage_incomplete_reason: None,
                active_duration_ms: None,
                wall_duration_ms: None,
                model_turns: Some(0),
                agents_used: Some(0),
            },
            updated_at: Some(0),
            completed_at: Some(0),
        }]
    );
    assert_eq!(run.accepted_starts, 0);
    assert_eq!(
        run.metrics.usage,
        Some(codex_app_server_protocol::WorkflowTokenUsage::default())
    );
    assert!(run.metrics.usage_complete);
    assert_eq!(
        run.logs
            .iter()
            .map(|entry| entry.message.as_str())
            .collect::<Vec<_>>(),
        vec!["Publishing answer"]
    );
    let read: WorkflowReadResponse = rpc(
        &mut app,
        "workflow/read",
        json!({"threadId":thread_id,"workflowId":run.id}),
    )
    .await?;
    assert_eq!(read.workflow, run);
    let saved: WorkflowSaveResponse = rpc(
        &mut app,
        "workflow/save",
        json!({"threadId":thread_id,"workflowId":run.id,"name":"answer","scope":"user"}),
    )
    .await?;
    assert_eq!(saved.saved_workflow.source, SCRIPT);
    assert_eq!(saved.saved_workflow.scope, WorkflowScope::User);
    assert_eq!(saved.saved_workflow.metadata, run.metadata);
    assert!(!saved.saved_workflow.read_only);
    let library: WorkflowSavedListResponse = rpc(
        &mut app,
        "workflow/saved/list",
        json!({"threadId":thread_id,"limit":1}),
    )
    .await?;
    assert_eq!(library.data.len(), 1);
    assert_eq!(library.data[0].id, saved.saved_workflow.id);
    assert_eq!(library.next_cursor, None);
    assert!(
        library
            .watch_roots
            .iter()
            .any(|root| root.to_string().contains("workflows"))
    );
    let saved_read: WorkflowSavedReadResponse = rpc(
        &mut app,
        "workflow/saved/read",
        json!({"threadId":thread_id,"savedWorkflowId":saved.saved_workflow.id}),
    )
    .await?;
    assert_eq!(saved_read.saved_workflow, saved.saved_workflow);
    let second: WorkflowStartResponse = rpc(
        &mut app,
        "workflow/start",
        json!({"threadId":thread_id,"savedWorkflowId":saved.saved_workflow.id,"input":{"value":7}}),
    )
    .await?;
    let second = terminal(&mut app, &second.workflow.id).await?;
    assert_eq!(second.result, Some(json!({"answer":7})));
    let first_page: WorkflowListResponse = rpc(
        &mut app,
        "workflow/list",
        json!({"threadId":thread_id,"limit":1}),
    )
    .await?;
    assert_eq!(first_page.data.len(), 1);
    let second_page: WorkflowListResponse = rpc(&mut app,"workflow/list",json!({"threadId":thread_id,"limit":1,"cursor":first_page.next_cursor.clone().expect("second page")})).await?;
    assert_eq!(second_page.data.len(), 1);
    assert_eq!(second_page.next_cursor, None);
    assert_ne!(first_page.data[0].id, second_page.data[0].id);
    for row in first_page.data.iter().chain(&second_page.data) {
        let expected = if row.id == run.id {
            &run
        } else {
            assert_eq!(row.id, second.id);
            &second
        };
        assert_eq!(row.phase_count, 1);
        assert_eq!(row.metadata, expected.metadata);
        assert_eq!(row.phase_progress, expected.phase_progress);
        assert_eq!(row.metrics.usage, expected.metrics.usage);
        assert!(row.metrics.usage_complete);
    }
    let _: TurnStartResponse = rpc(
        &mut app,
        "turn/start",
        json!({"threadId":thread_id,"input":[{"type":"text","text":"Hello"}]}),
    )
    .await?;
    let completed: TurnCompletedNotification =
        timeout(WAIT, app.read_notification("turn/completed")).await??;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    drop(app);
    let mut app = client(&home).await?;
    let _: ThreadResumeResponse =
        rpc(&mut app, "thread/resume", json!({"threadId":thread_id})).await?;
    let reread: WorkflowReadResponse = rpc(
        &mut app,
        "workflow/read",
        json!({"threadId":thread_id,"workflowId":run.id}),
    )
    .await?;
    assert_eq!(reread.workflow, run);
    let library_read: WorkflowSavedReadResponse = rpc(
        &mut app,
        "workflow/saved/read",
        json!({"threadId":thread_id,"savedWorkflowId":saved.saved_workflow.id}),
    )
    .await?;
    assert_eq!(library_read.saved_workflow, saved.saved_workflow);
    let _: WorkflowSavedDeleteResponse = rpc(
        &mut app,
        "workflow/saved/delete",
        json!({"threadId":thread_id,"savedWorkflowId":saved.saved_workflow.id}),
    )
    .await?;
    let library: WorkflowSavedListResponse = rpc(
        &mut app,
        "workflow/saved/list",
        json!({"threadId":thread_id}),
    )
    .await?;
    assert_eq!(library.data, Vec::new());
    Ok(())
}

#[tokio::test]
async fn workflows_reject_invalid_requests_and_keep_failed_runs_retryable() -> Result<()> {
    let home = TempDir::new()?;
    let server = create_mock_responses_server_sequence(Vec::new()).await;
    configure_agent_workflows(&home, &server).await?;
    let mut app = client(&home).await?;
    let thread_id = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    for params in [
        json!({"threadId":thread_id,"input":null}),
        json!({"threadId":thread_id,"source":SCRIPT,"savedWorkflowId":"user:example","input":null}),
        json!({"threadId":thread_id,"source":"","input":null}),
    ] {
        let error = rejected(&mut app, "workflow/start", params).await?;
        assert!(!error.error.message.is_empty());
    }
    for params in [
        json!({"threadId":thread_id,"limit":0}),
        json!({"threadId":thread_id,"limit":51}),
        json!({"threadId":thread_id,"cursor":"invalid"}),
    ] {
        let error = rejected(&mut app, "workflow/list", params).await?;
        assert!(!error.error.message.is_empty());
    }
    let empty: WorkflowListResponse =
        rpc(&mut app, "workflow/list", json!({"threadId":thread_id})).await?;
    assert_eq!(
        empty,
        WorkflowListResponse {
            data: Vec::new(),
            next_cursor: None
        }
    );
    let started: WorkflowStartResponse = rpc(
        &mut app,
        "workflow/start",
        json!({"threadId":thread_id,"source":"workflow.meta({name:'failure',description:'Exercise explicit retry',phases:[]}); throw new Error('expected failure');","input":null}),
    )
    .await?;
    let failed = terminal(&mut app, &started.workflow.id).await?;
    assert_eq!(failed.status, WorkflowStatus::Failed);
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains("expected failure"))
    );
    let _: WorkflowRetryResponse = rpc(
        &mut app,
        "workflow/retry",
        json!({"threadId":thread_id,"workflowId":failed.id}),
    )
    .await?;
    let retried = terminal(&mut app, &failed.id).await?;
    assert_eq!(retried.id, failed.id);
    assert_eq!(retried.status, WorkflowStatus::Failed);
    let other_id = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let error = rejected(
        &mut app,
        "workflow/read",
        json!({"threadId":other_id,"workflowId":failed.id}),
    )
    .await?;
    assert!(!error.error.message.is_empty());
    let _: WorkflowStopResponse = rpc(
        &mut app,
        "workflow/stop",
        json!({"threadId":thread_id,"workflowId":failed.id}),
    )
    .await?;
    let stopped: WorkflowReadResponse = rpc(
        &mut app,
        "workflow/read",
        json!({"threadId":thread_id,"workflowId":failed.id}),
    )
    .await?;
    assert_eq!(stopped.workflow.status, WorkflowStatus::Stopped);
    let error = rejected(
        &mut app,
        "workflow/save",
        json!({"threadId":thread_id,"workflowId":failed.id,"name":"../escape","scope":"user"}),
    )
    .await?;
    assert!(!error.error.message.is_empty());
    let unchanged: WorkflowReadResponse = rpc(
        &mut app,
        "workflow/read",
        json!({"threadId":thread_id,"workflowId":failed.id}),
    )
    .await?;
    assert_eq!(unchanged, stopped);
    Ok(())
}

#[tokio::test]
async fn workflows_v2_preserve_omitted_and_explicit_null_arguments() -> Result<()> {
    let home = TempDir::new()?;
    let server = create_mock_responses_server_sequence(Vec::new()).await;
    configure_agent_workflows(&home, &server).await?;
    let mut app = client(&home).await?;
    let thread = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let source = "workflow.meta({name:'arguments',description:'Return the exact supplied arguments',phases:[]}); return workflow.args;";
    for (params, expected) in [
        (json!({"threadId":thread,"source":source}), json!({})),
        (
            json!({"threadId":thread,"source":source,"input":null}),
            Value::Null,
        ),
    ] {
        let started: WorkflowStartResponse = rpc(&mut app, "workflow/start", params).await?;
        let completed = terminal(&mut app, &started.workflow.id).await?;
        assert_eq!(completed.status, WorkflowStatus::Succeeded);
        assert_eq!(completed.input, expected);
        // The nullable API field decodes JSON null as None; persisted engine
        // state separately retains a present null for cached replay.
        let expected_result = if expected.is_null() {
            None
        } else {
            Some(expected.clone())
        };
        assert_eq!(completed.result, expected_result);
        let wire: Value = rpc(
            &mut app,
            "workflow/read",
            json!({"threadId":thread,"workflowId":started.workflow.id}),
        )
        .await?;
        assert!(
            wire["workflow"]
                .as_object()
                .expect("workflow object")
                .contains_key("result")
        );
        assert_eq!(wire["workflow"]["result"], expected);
        let reread: WorkflowReadResponse = serde_json::from_value(wire)?;
        assert_eq!(reread.workflow, completed);
    }
    Ok(())
}

#[tokio::test]
async fn workflows_saved_launch_rejects_disabled_native_capability_before_creating_a_run()
-> Result<()> {
    let home = TempDir::new()?;
    let server = responses::start_mock_server().await;
    configure_agent_workflows(&home, &server).await?;
    let config_path = home.path().join("config.toml");
    let mut config: toml::Value = toml::from_str(&std::fs::read_to_string(&config_path)?)?;
    config
        .get_mut("features")
        .and_then(toml::Value::as_table_mut)
        .expect("fixture feature table")
        .insert(
            "multi_agent_v2".into(),
            toml::Value::try_from(
                json!({"enabled":true,"non_code_mode_only":false,"disable_native_tools":true}),
            )?,
        );
    std::fs::write(config_path, toml::to_string(&config)?)?;
    std::fs::create_dir_all(home.path().join("workflows"))?;
    let source = "workflow.meta({name:'native-required',description:'Saved launches retain native caller policy',phases:[]}); return await workflow.agent('Must never spawn');";
    std::fs::write(home.path().join("workflows/native-required.js"), source)?;
    let mut app = client(&home).await?;
    let root = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let error = rejected(
        &mut app,
        "workflow/start",
        json!({"threadId":root,"savedWorkflowId":"user:native-required"}),
    )
    .await?;
    assert!(error.error.message.contains("native"));
    let list: WorkflowListResponse =
        rpc(&mut app, "workflow/list", json!({"threadId":root})).await?;
    assert!(
        list.data.is_empty(),
        "native policy must reject before persisting a run"
    );
    assert!(
        responses::received_responses_requests(&server)
            .await
            .is_empty()
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("workflows/native-required.js"))?,
        source
    );
    Ok(())
}

#[tokio::test]
async fn workflows_saved_launch_from_isolated_child_uses_the_displayed_owner_catalog() -> Result<()>
{
    core_test_support::skip_if_remote!(Ok(()), "managed worktree fixture paths are host-local");
    let home = TempDir::new()?;
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_assistant_message("isolated-ready", r#""ready""#),
            responses::ev_completed("isolated-ready"),
        ]),
    )
    .await;
    configure_agent_workflows(&home, &server).await?;
    let mut app = client(&home).await?;
    let project = app.auto_env()?.cwd().to_path_buf();
    for arguments in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=Workflow API QA",
            "-c",
            "user.email=workflow-api@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    ] {
        let output = std::process::Command::new("git")
            .current_dir(&project)
            .args(arguments)
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "worktree fixture Git setup failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let owner = app
        .start_thread(ThreadStartParams {
            cwd: Some(project.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?
        .thread
        .id;
    let source = "workflow.meta({name:'isolated-caller',description:'Create a recorded child with a distinct checkout',phases:[]}); return await workflow.agent('Keep the isolated worker available.',{label:'isolated',isolation:'worktree'});";
    let started: WorkflowStartResponse = rpc(
        &mut app,
        "workflow/start",
        json!({"threadId":owner,"source":source}),
    )
    .await?;
    let completed = terminal(&mut app, &started.workflow.id).await?;
    assert_eq!(completed.status, WorkflowStatus::Succeeded);
    let child = completed
        .steps
        .values()
        .find(|step| step.label == "isolated")
        .and_then(|step| step.attempts.first())
        .and_then(|attempt| attempt.child_thread_id.as_ref())
        .expect("accepted isolated child");
    let resumed: ThreadResumeResponse =
        rpc(&mut app, "thread/resume", json!({"threadId":child})).await?;
    assert_eq!(resumed.thread.can_accept_direct_input, Some(true));
    assert_ne!(
        std::fs::canonicalize(resumed.cwd.as_path())?,
        std::fs::canonicalize(&project)?
    );
    let root_library = project.join(".codex/workflows");
    let child_library = resumed.cwd.join(".codex/workflows");
    std::fs::create_dir_all(&root_library)?;
    std::fs::create_dir_all(&child_library)?;
    let selected = "workflow.meta({name:'catalog',description:'The displayed owner catalog entry',phases:[]}); return 'owner catalog';";
    let shadow = "workflow.meta({name:'catalog',description:'A different same-ID file inside the child checkout',phases:[]}); return 'wrong child catalog';";
    std::fs::write(root_library.join("catalog.js"), selected)?;
    std::fs::write(child_library.join("catalog.js"), shadow)?;
    let catalog: WorkflowSavedListResponse =
        rpc(&mut app, "workflow/saved/list", json!({"threadId":child})).await?;
    assert_eq!(catalog.data.len(), 1);
    assert_eq!(
        catalog.data[0]
            .metadata
            .as_ref()
            .expect("catalog metadata")
            .description,
        "The displayed owner catalog entry"
    );
    let launched: WorkflowStartResponse = rpc(
        &mut app,
        "workflow/start",
        json!({"threadId":child,"savedWorkflowId":"project:catalog"}),
    )
    .await?;
    let result = terminal(&mut app, &launched.workflow.id).await?;
    assert_eq!(result.status, WorkflowStatus::Succeeded);
    assert_eq!(result.owner_thread_id, owner);
    assert_eq!(result.source, selected);
    assert_eq!(result.result, Some(json!("owner catalog")));
    assert_eq!(result.accepted_starts, 0);
    assert_eq!(
        std::fs::read_to_string(child_library.join("catalog.js"))?,
        shadow
    );
    assert_eq!(
        mock.single_request().header("thread-id").as_deref(),
        Some(child.as_str())
    );
    Ok(())
}

#[tokio::test]
async fn workflows_legacy_runs_and_saved_sources_remain_read_only_across_resume() -> Result<()> {
    let home = TempDir::new()?;
    let server =
        create_mock_responses_server_sequence(vec![create_final_assistant_message_sse_response(
            "Materialize the conversation",
        )?])
        .await;
    configure_agent_workflows(&home, &server).await?;
    let mut app = client(&home).await?;
    let thread = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let _:TurnStartResponse=rpc(&mut app,"turn/start",json!({"threadId":thread,"input":[{"type":"text","text":"Keep this conversation available after restart."}]})).await?;
    let completion: TurnCompletedNotification =
        timeout(WAIT, app.read_notification("turn/completed")).await??;
    assert_eq!(completion.turn.status, TurnStatus::Completed);
    let directory = home.path().join("workflow-runs").join(&thread);
    std::fs::create_dir_all(&directory)?;
    let old_source = "await workflow.publish({legacy:true});";
    let mut originals = Vec::new();
    for status in ["running", "succeeded"] {
        let id = uuid::Uuid::new_v4().to_string();
        let bytes = serde_json::to_vec(
            &json!({"id":id,"ownerThreadId":thread,"source":old_source,"input":{},"status":status,"phase":null,"phases":["Historical plan","Historical result"],"steps":{},"error":null,"createdAt":1,"updatedAt":1}),
        )?;
        let path = directory.join(format!("{id}.json"));
        std::fs::write(&path, &bytes)?;
        originals.push((id, path, bytes));
    }
    std::fs::create_dir_all(home.path().join("workflows"))?;
    let saved_path = home.path().join("workflows/old.js");
    std::fs::write(&saved_path, old_source)?;
    let library: WorkflowSavedListResponse =
        rpc(&mut app, "workflow/saved/list", json!({"threadId":thread})).await?;
    assert_eq!(library.data.len(), 1);
    assert!(library.data[0].read_only);
    assert_eq!(library.data[0].format_version, 1);
    assert!(library.data[0].metadata.is_none());
    let direct_error = rejected(
        &mut app,
        "workflow/start",
        json!({"threadId":thread,"source":old_source}),
    )
    .await?;
    assert!(direct_error.error.message.contains("read-only"));
    let saved_error = rejected(
        &mut app,
        "workflow/start",
        json!({"threadId":thread,"savedWorkflowId":"user:old"}),
    )
    .await?;
    assert_eq!(
        saved_error.error.message,
        library.data[0]
            .read_only_reason
            .as_deref()
            .expect("published legacy read-only reason")
    );
    drop(app);
    let mut app = client(&home).await?;
    let _: ThreadResumeResponse =
        rpc(&mut app, "thread/resume", json!({"threadId":thread})).await?;
    let list: WorkflowListResponse =
        rpc(&mut app, "workflow/list", json!({"threadId":thread})).await?;
    assert_eq!(list.data.len(), 2);
    assert!(
        list.data
            .iter()
            .all(|run| run.read_only && run.format_version == 1)
    );
    assert_eq!(
        list.data
            .iter()
            .map(|run| run.phase_count)
            .collect::<Vec<_>>(),
        vec![2, 2]
    );
    let all: WorkflowListResponse = rpc(
        &mut app,
        "workflow/list",
        json!({"threadId":thread,"scope":"all"}),
    )
    .await?;
    assert_eq!(all, list);
    for (id, path, bytes) in originals {
        let read: WorkflowReadResponse = rpc(
            &mut app,
            "workflow/read",
            json!({"threadId":thread,"workflowId":id}),
        )
        .await?;
        assert!(read.workflow.read_only);
        assert_eq!(read.workflow.source, old_source);
        assert_eq!(
            read.workflow.phases,
            vec![
                "Historical plan".to_string(),
                "Historical result".to_string()
            ]
        );
        for method in ["workflow/retry", "workflow/recover", "workflow/stop"] {
            let error =
                rejected(&mut app, method, json!({"threadId":thread,"workflowId":id})).await?;
            assert!(error.error.message.contains("read-only"));
        }
        assert_eq!(std::fs::read(path)?, bytes);
    }
    assert_eq!(std::fs::read_to_string(saved_path)?, old_source);
    Ok(())
}

#[tokio::test]
async fn workflows_stop_and_recover_active_script_while_main_chat_remains_usable() -> Result<()> {
    let home = TempDir::new()?;
    let server =
        create_mock_responses_server_sequence(vec![create_final_assistant_message_sse_response(
            "The workflow does not block this response",
        )?])
        .await;
    configure_agent_workflows(&home, &server).await?;
    let mut app = client(&home).await?;
    let thread_id = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let started: WorkflowStartResponse = rpc(
        &mut app,
        "workflow/start",
        json!({"threadId":thread_id,"source":"workflow.meta({name:'held',description:'Remain active until explicitly stopped',phases:[]}); await new Promise(() => {});","input":null}),
    )
    .await?;
    timeout(WAIT, async {
        loop {
            let update: WorkflowUpdatedNotification =
                app.read_notification("workflow/updated").await?;
            if update.workflow.id == started.workflow.id
                && update.workflow.status == WorkflowStatus::Running
            {
                break Result::<()>::Ok(());
            }
        }
    })
    .await??;
    let progress: Value = timeout(WAIT, async {
        loop {
            let progress: Value = app.read_notification("workflow/progress").await?;
            if progress["workflow"]["id"] == json!(started.workflow.id) {
                break Result::<Value>::Ok(progress);
            }
        }
    })
    .await??;
    assert_eq!(progress["threadId"], json!(thread_id));
    assert_eq!(
        progress["workflow"]["executionId"],
        json!(started.workflow.execution_id)
    );
    let compact = progress["workflow"]
        .as_object()
        .expect("compact progress payload");
    for detail in ["source", "input", "steps", "result", "logs"] {
        assert!(
            !compact.contains_key(detail),
            "accounting progress must omit {detail}"
        );
    }
    assert!(compact.contains_key("metrics"));
    let _: TurnStartResponse = rpc(&mut app,"turn/start",json!({"threadId":thread_id,"input":[{"type":"text","text":"Please answer while the workflow is running"}]})).await?;
    let completed: TurnCompletedNotification =
        timeout(WAIT, app.read_notification("turn/completed")).await??;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    let running: WorkflowReadResponse = rpc(
        &mut app,
        "workflow/read",
        json!({"threadId":thread_id,"workflowId":started.workflow.id}),
    )
    .await?;
    assert_eq!(running.workflow.status, WorkflowStatus::Running);
    let stopped: WorkflowStopResponse = rpc(
        &mut app,
        "workflow/stop",
        json!({"threadId":thread_id,"workflowId":started.workflow.id}),
    )
    .await?;
    assert_eq!(stopped.workflow.status, WorkflowStatus::Stopped);
    let _: WorkflowRecoverResponse = rpc(
        &mut app,
        "workflow/recover",
        json!({"threadId":thread_id,"workflowId":started.workflow.id}),
    )
    .await?;
    timeout(WAIT, async {
        loop {
            let update: WorkflowUpdatedNotification =
                app.read_notification("workflow/updated").await?;
            if update.workflow.id == started.workflow.id
                && update.workflow.status == WorkflowStatus::Running
            {
                break Result::<()>::Ok(());
            }
        }
    })
    .await??;
    let stopped_again: WorkflowStopResponse = rpc(
        &mut app,
        "workflow/stop",
        json!({"threadId":thread_id,"workflowId":started.workflow.id}),
    )
    .await?;
    assert_eq!(stopped_again.workflow.status, WorkflowStatus::Stopped);
    Ok(())
}

#[tokio::test]
async fn workflows_project_and_all_filters_read_persisted_runs_without_starting_them() -> Result<()>
{
    let home = TempDir::new()?;
    let first_project = TempDir::new()?;
    let other_project = TempDir::new()?;
    let server = create_mock_responses_server_sequence(Vec::new()).await;
    configure_agent_workflows(&home, &server).await?;
    let mut app = client(&home).await?;
    let mut owners = Vec::new();
    let mut runs = Vec::new();
    for project in [
        first_project.path(),
        first_project.path(),
        other_project.path(),
    ] {
        let id = app
            .start_thread(ThreadStartParams {
                cwd: Some(project.to_string_lossy().into_owned()),
                ..Default::default()
            })
            .await?
            .thread
            .id;
        let started: WorkflowStartResponse = rpc(
            &mut app,
            "workflow/start",
            json!({"threadId":id,"source":SCRIPT,"input":{"value":1}}),
        )
        .await?;
        let completed = terminal(&mut app, &started.workflow.id).await?;
        assert_eq!(completed.status, WorkflowStatus::Succeeded);
        owners.push(id);
        runs.push(completed);
    }
    for (scope, expected) in [
        ("current", vec![runs[0].id.clone()]),
        ("project", vec![runs[0].id.clone(), runs[1].id.clone()]),
        ("all", runs.iter().map(|run| run.id.clone()).collect()),
    ] {
        let listed: WorkflowListResponse = rpc(
            &mut app,
            "workflow/list",
            json!({"threadId":owners[0],"scope":scope}),
        )
        .await?;
        let mut ids = listed
            .data
            .into_iter()
            .map(|run| run.id)
            .collect::<Vec<_>>();
        let mut expected = expected;
        ids.sort();
        expected.sort();
        assert_eq!(ids, expected);
        assert_eq!(listed.next_cursor, None);
    }
    for (owner, expected) in owners.iter().zip(runs) {
        let reread: WorkflowReadResponse = rpc(
            &mut app,
            "workflow/read",
            json!({"threadId":owner,"workflowId":expected.id}),
        )
        .await?;
        assert_eq!(reread.workflow, expected);
    }
    Ok(())
}

#[tokio::test]
async fn workflows_mode_changes_update_loaded_thread_tools_and_policy_on_the_next_turn()
-> Result<()> {
    let home = TempDir::new()?;
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        (0..4)
            .map(|index| {
                responses::sse(vec![
                    responses::ev_response_created(&format!("mode-{index}")),
                    responses::ev_assistant_message(&format!("answer-{index}"), "Ready."),
                    responses::ev_completed(&format!("mode-{index}")),
                ])
            })
            .collect(),
    )
    .await;
    configure_agent_workflows(&home, &server).await?;
    let mut app = client(&home).await?;
    let thread_id = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    for (index, mode, launch_available) in [
        (0, "auto", true),
        (1, "explicit", false),
        (2, "off", false),
        (3, "auto", true),
    ] {
        if index != 0 {
            let request = app
                .send_config_batch_write_request(ConfigBatchWriteParams {
                    edits: vec![ConfigEdit {
                        key_path: "features.dynamic_workflows".to_string(),
                        value: json!({"enabled":mode!="off","mode":mode}),
                        merge_strategy: MergeStrategy::Replace,
                    }],
                    file_path: None,
                    expected_version: None,
                    reload_user_config: true,
                })
                .await?;
            let _: ConfigWriteResponse = timeout(WAIT, app.read_response(request)).await??;
        }
        let _:TurnStartResponse = rpc(&mut app,"turn/start",json!({"threadId":thread_id,"input":[{"type":"text","text":format!("Describe the current workflow policy for turn {index}.")}]})).await?;
        let completed: TurnCompletedNotification =
            timeout(WAIT, app.read_notification("turn/completed")).await??;
        assert_eq!(completed.turn.status, TurnStatus::Completed);
        let requests = mock.requests();
        assert_eq!(requests.len(), index + 1);
        let body = requests[index].body_json();
        let tools = body["tools"].to_string();
        assert_eq!(
            tools.contains("\"name\":\"workflow_start\""),
            launch_available,
            "effective launch tool in mode {mode}"
        );
        assert!(
            tools.contains("\"name\":\"workflow_read\""),
            "existing workflow controls remain exposed in mode {mode}"
        );
        assert!(
            body["input"]
                .to_string()
                .contains(&format!("Mode: {mode}.")),
            "the next request must contain the updated effective policy"
        );
        if mode == "off" {
            let error = rejected(
                &mut app,
                "workflow/start",
                json!({"threadId":thread_id,"source":SCRIPT,"input":{"value":1}}),
            )
            .await?;
            assert!(error.error.message.contains("disabled"));
            let list: WorkflowListResponse =
                rpc(&mut app, "workflow/list", json!({"threadId":thread_id})).await?;
            assert_eq!(list.data, Vec::new());
        }
    }
    Ok(())
}

#[tokio::test]
async fn workflows_model_launch_notifies_thread_subscriber_without_prior_workflow_rpc() -> Result<()>
{
    let home = TempDir::new()?;
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("launch"),
                responses::ev_function_call(
                    "launch-workflow",
                    "workflow_start",
                    &json!({"source":SCRIPT,"input":{"value":42}}).to_string(),
                ),
                responses::ev_completed("launch"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("answer"),
                responses::ev_assistant_message(
                    "launched",
                    "The workflow is running independently.",
                ),
                responses::ev_completed("answer"),
            ]),
        ],
    )
    .await;
    configure_agent_workflows(&home, &server).await?;
    let mut app = client(&home).await?;
    let thread_id = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    // No workflow RPC has been sent: the ordinary thread subscription owns this stream.
    let _: TurnStartResponse = rpc(&mut app,"turn/start",json!({"threadId":thread_id,"input":[{"type":"text","text":"Launch the coordination workflow."}]})).await?;
    let run = timeout(WAIT, async {
        loop {
            let update: WorkflowUpdatedNotification =
                app.read_notification("workflow/updated").await?;
            assert_eq!(update.thread_id, thread_id);
            assert_eq!(update.workflow.owner_thread_id, thread_id);
            if matches!(
                update.workflow.status,
                WorkflowStatus::Succeeded | WorkflowStatus::Failed
            ) {
                break Result::<WorkflowRun>::Ok(update.workflow);
            }
        }
    })
    .await??;
    assert_eq!(run.status, WorkflowStatus::Succeeded);
    assert_eq!(run.result, Some(json!({"answer":42})));
    let completed: TurnCompletedNotification =
        timeout(WAIT, app.read_notification("turn/completed")).await??;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .function_call_output("launch-workflow")
            .to_string()
            .contains(&run.id)
    );
    Ok(())
}

#[tokio::test]
async fn workflows_owned_child_accepts_human_guidance_without_configuration_overrides() -> Result<()>
{
    let home = TempDir::new()?;
    let catalog_server = responses::start_mock_server().await;
    configure_agent_workflows(&home, &catalog_server).await?;
    let (release, gate) = tokio::sync::oneshot::channel();
    let (release_guided, guided_gate) = tokio::sync::oneshot::channel();
    let (streaming, _) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("held-child")]),
            },
            StreamingSseChunk {
                gate: Some(gate),
                body: responses::sse(vec![responses::ev_completed("held-child")]),
            },
        ],
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("guided-child")]),
            },
            StreamingSseChunk {
                gate: Some(guided_gate),
                body: responses::sse(vec![
                    responses::ev_assistant_message("guided-result", r#"{"guided":true}"#),
                    responses::ev_completed("guided-child"),
                ]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_response_created("human-followup"),
                responses::ev_assistant_message("human-followup-result", r#"{"guided":true}"#),
                responses::ev_completed("human-followup"),
            ]),
        }],
    ])
    .await;
    let config_path = home.path().join("config.toml");
    let mut config: toml::Value = toml::from_str(&std::fs::read_to_string(&config_path)?)?;
    config["model_providers"]["mock_provider"]["base_url"] =
        toml::Value::String(format!("{}/v1", streaming.uri()));
    std::fs::write(config_path, toml::to_string(&config)?)?;
    let mut app = client(&home).await?;
    let root = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let started:WorkflowStartResponse = rpc(&mut app,"workflow/start",json!({"threadId":root,"source":"workflow.meta({name:'guided',description:'Retain the worker profile during human guidance',phases:[{title:'Guide'}]}); await workflow.phase('Guide'); return await workflow.agent('Retain this original worker task.',{label:'guided',reasoningEffort:'low'});","input":null})).await?;
    let child_id = timeout(WAIT, async {
        loop {
            let update: WorkflowUpdatedNotification =
                app.read_notification("workflow/updated").await?;
            if let Some(id) = update
                .workflow
                .steps
                .values()
                .find(|step| step.label == "guided")
                .and_then(|step| step.attempts.last())
                .and_then(|attempt| attempt.child_thread_id.clone())
            {
                break Result::<String>::Ok(id);
            }
            if matches!(
                update.workflow.status,
                WorkflowStatus::Failed | WorkflowStatus::Stopped | WorkflowStatus::Interrupted
            ) {
                anyhow::bail!(
                    "workflow failed before accepting the real child: {:?}",
                    update.workflow.error
                );
            }
        }
    })
    .await??;
    timeout(WAIT, streaming.wait_for_request_count(/*count*/ 1)).await?;
    let read: ThreadReadResponse =
        rpc(&mut app, "thread/read", json!({"threadId":child_id})).await?;
    assert_eq!(read.thread.can_accept_direct_input, Some(true));
    let baseline: ThreadResumeResponse =
        rpc(&mut app, "thread/resume", json!({"threadId":child_id})).await?;
    assert_eq!(baseline.thread.can_accept_direct_input, Some(true));
    assert_eq!(
        baseline.reasoning_effort,
        Some(codex_protocol::openai_models::ReasoningEffort::Low)
    );
    let turn_id = baseline
        .thread
        .turns
        .iter()
        .find(|turn| turn.status == TurnStatus::InProgress)
        .expect("real active child turn")
        .id
        .clone();
    let _:TurnSteerResponse = rpc(&mut app,"turn/steer",json!({"threadId":child_id,"expectedTurnId":turn_id,"input":[{"type":"text","text":"HUMAN_GUIDANCE_WHILE_RUNNING"}]})).await?;
    let error = rejected(&mut app,"turn/start",json!({"threadId":child_id,"model":"different-model","input":[{"type":"text","text":"Forbidden profile replacement"}]})).await?;
    assert!(error.error.message.contains("preserve"));
    let error = rejected(&mut app,"turn/steer",json!({"threadId":child_id,"expectedTurnId":turn_id,"additionalContext":{},"input":[{"type":"text","text":"Forbidden context replacement"}]})).await?;
    assert!(error.error.message.contains("preserve"));
    // The accepted steer preempts the first sample. Hold its replacement
    // response so pause observes active inference, before terminal mail exists.
    release
        .send(())
        .expect("release the original child response after steering");
    timeout(WAIT, streaming.wait_for_request_count(/*count*/ 2)).await?;
    let paused: ThreadPauseResponse =
        rpc(&mut app, "thread/pause", json!({"threadId":child_id})).await?;
    assert_eq!(
        paused.execution_control,
        ThreadExecutionControl {
            root_thread_id: root.clone(),
            state: ExecutionControlState::Paused,
            can_pause: false,
        }
    );
    let control: ThreadExecutionControlReadResponse = rpc(
        &mut app,
        "thread/executionControl/read",
        json!({"threadId":root}),
    )
    .await?;
    assert_eq!(control.execution_control, paused.execution_control);
    let _ = release_guided.send(());
    let resumed_turn: TurnStartResponse = rpc(
        &mut app,
        "turn/start",
        json!({"threadId":child_id,"input":[{"type":"text","text":"HUMAN_RESUMES_OWNED_WORK"}]}),
    )
    .await?;
    assert_eq!(resumed_turn.turn.id, turn_id);
    let completed = terminal(&mut app, &started.workflow.id).await?;
    assert_eq!(completed.status, WorkflowStatus::Succeeded);
    assert_eq!(completed.result, Some(json!({"guided":true})));
    let guided_step = completed
        .steps
        .values()
        .find(|step| step.label == "guided")
        .expect("recorded guided step");
    assert_eq!(guided_step.attempts.len(), 1);
    assert_eq!(
        guided_step.attempts[0].child_thread_id.as_deref(),
        Some(child_id.as_str())
    );
    loop {
        let completion: TurnCompletedNotification =
            timeout(WAIT, app.read_notification("turn/completed")).await??;
        if completion.thread_id == child_id && completion.turn.id == resumed_turn.turn.id {
            assert_eq!(completion.turn.status, TurnStatus::Completed);
            break;
        }
    }
    let after: ThreadResumeResponse =
        rpc(&mut app, "thread/resume", json!({"threadId":child_id})).await?;
    assert_eq!(
        (
            after.model,
            after.reasoning_effort,
            after.sandbox,
            after.approval_policy
        ),
        (
            baseline.model,
            baseline.reasoning_effort,
            baseline.sandbox,
            baseline.approval_policy
        )
    );
    let requests = streaming.requests().await;
    assert_eq!(requests.len(), 3);
    let guided: Value = serde_json::from_slice(&requests[1])?;
    let resumed: Value = serde_json::from_slice(&requests[2])?;
    assert!(
        guided["input"]
            .to_string()
            .contains("HUMAN_GUIDANCE_WHILE_RUNNING")
    );
    assert!(
        resumed["input"]
            .to_string()
            .contains("HUMAN_RESUMES_OWNED_WORK")
    );
    assert!(
        resumed["input"]
            .to_string()
            .contains("HUMAN_GUIDANCE_WHILE_RUNNING")
    );
    assert_eq!(resumed["reasoning"]["effort"], json!("low"));
    assert_eq!(guided["client_metadata"]["thread_id"], json!(child_id));
    assert_eq!(resumed["client_metadata"]["thread_id"], json!(child_id));
    drop(app);
    streaming.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn workflows_similarly_named_ordinary_v2_child_remains_read_only() -> Result<()> {
    let home = TempDir::new()?;
    let server = responses::start_mock_server().await;
    configure_agent_workflows(&home, &server).await?;
    let config_path = home.path().join("config.toml");
    let mut config: toml::Value = toml::from_str(&std::fs::read_to_string(&config_path)?)?;
    config.get_mut("features").and_then(toml::Value::as_table_mut).expect("feature table authored by MockResponsesConfig").insert(
        "multi_agent_v2".to_string(),toml::Value::try_from(json!({"enabled":true,"non_code_mode_only":false,"tool_namespace":"workflow_test_agents"}))?,
    );
    std::fs::write(config_path, toml::to_string(&config)?)?;
    let mut app = client(&home).await?;
    let root = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let similar_name = format!(
        "workflow_{}_{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    responses::mount_sse_once_match(&server,wiremock::matchers::header("thread-id",root.as_str()),responses::sse(vec![
        responses::ev_function_call_with_namespace("ordinary-spawn","workflow_test_agents","spawn_agent",&json!({"task_name":similar_name,"message":"Wait for ordinary parent guidance.","fork_turns":"none"}).to_string()),
        responses::ev_completed("ordinary-spawn"),
    ])).await;
    let parent_after_spawn = responses::mount_sse_once_match(
        &server,
        wiremock::matchers::header("thread-id", root.as_str()),
        responses::sse(vec![
            responses::ev_assistant_message(
                "ordinary-root-done",
                "The ordinary worker is running.",
            ),
            responses::ev_completed("ordinary-root-done"),
        ]),
    )
    .await;
    let root_match = root.clone();
    responses::mount_response_once_match(
        &server,
        move |request: &wiremock::Request| {
            request
                .headers
                .get("thread-id")
                .is_some_and(|id| id != root_match.as_str())
        },
        responses::sse_response(responses::sse(vec![responses::ev_completed(
            "held-ordinary",
        )]))
        .set_delay(Duration::from_secs(/*secs*/ 60)),
    )
    .await;
    let _:TurnStartResponse = rpc(&mut app,"turn/start",json!({"threadId":root,"input":[{"type":"text","text":"Create the similarly named ordinary worker."}]})).await?;
    let root_completion: TurnCompletedNotification =
        timeout(WAIT, app.read_notification("turn/completed")).await??;
    assert_eq!(root_completion.thread_id, root);
    let spawn_output = parent_after_spawn
        .single_request()
        .function_call_output_text("ordinary-spawn")
        .expect("ordinary native spawn returns a model-visible result");
    assert!(
        spawn_output.contains(&similar_name),
        "ordinary spawn must accept the valid workflow-like task name before testing direct input: {spawn_output}"
    );
    let child = timeout(WAIT, async {
        let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
        loop {
            if let Some(id) = responses::received_responses_requests(&server)
                .await
                .into_iter()
                .filter_map(|request| request.header("thread-id"))
                .find(|id| id != &root)
            {
                break id;
            }
            ticks.tick().await;
        }
    })
    .await?;
    let read: ThreadReadResponse = rpc(&mut app, "thread/read", json!({"threadId":child})).await?;
    assert_eq!(read.thread.can_accept_direct_input, Some(false));
    for (method, params) in [
        (
            "turn/start",
            json!({"threadId":child,"input":[{"type":"text","text":"Must remain denied"}]}),
        ),
        (
            "turn/steer",
            json!({"threadId":child,"expectedTurnId":"active-child","input":[{"type":"text","text":"Must remain denied"}]}),
        ),
    ] {
        let error = rejected(&mut app, method, params).await?;
        assert!(
            error
                .error
                .message
                .contains("not allowed for multi-agent v2 sub-agents")
        );
    }
    std::fs::create_dir_all(home.path().join("workflows"))?;
    std::fs::write(
        home.path().join("workflows/readonly-caller.js"),
        "workflow.meta({name:'readonly-caller',description:'A saved launch must retain caller authority',phases:[]}); return 'must not run';",
    )?;
    let error = rejected(
        &mut app,
        "workflow/start",
        json!({"threadId":child,"savedWorkflowId":"user:readonly-caller"}),
    )
    .await?;
    assert!(
        error
            .error
            .message
            .contains("not allowed for multi-agent v2 sub-agents")
    );
    let root_runs: WorkflowListResponse =
        rpc(&mut app, "workflow/list", json!({"threadId":root})).await?;
    assert!(
        root_runs.data.is_empty(),
        "a denied child launch must not create a root-owned run"
    );
    let still_read_only: ThreadReadResponse =
        rpc(&mut app, "thread/read", json!({"threadId":child})).await?;
    assert_eq!(still_read_only.thread.can_accept_direct_input, Some(false));
    Ok(())
}
