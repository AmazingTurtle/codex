//! Real VM publication, canonical history paging, and the human TUI share stable identities.

use super::*;
use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadTimelineEntry;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::WorkflowStartParams;
use codex_app_server_protocol::WorkflowStartResponse;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;

async fn actual_server(app: &App) -> Result<AppServerSession> {
    let state_db =
        crate::init_state_db_for_app_server_target(&app.config, &crate::AppServerTarget::Embedded)
            .await?;
    let provider = Arc::new(
        codex_code_mode::ProcessOwnedCodeModeSessionProvider::with_host_program(
            codex_utils_cargo_bin::cargo_bin("codex-code-mode-host")?,
        ),
    );
    let client = crate::start_embedded_app_server_with(
        codex_arg0::Arg0DispatchPaths::default(),
        app.config.clone(),
        Vec::new(),
        codex_config::LoaderOverrides::without_managed_config_for_tests(),
        /*strict_config*/ false,
        codex_config::CloudConfigBundleLoader::default(),
        codex_feedback::CodexFeedback::new(),
        /*log_db*/ None,
        state_db,
        Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
        Default::default(),
        move |mut args| {
            args.code_mode_session_provider = Some(provider);
            codex_app_server_client::InProcessAppServerClient::start(args)
        },
    )
    .await?;
    Ok(AppServerSession::new(
        crate::backend_id::BackendId::new(),
        crate::AppServerClient::InProcess(client),
        crate::app_server_session::ThreadParamsMode::Embedded,
    )
    .with_local_codex_home(&app.config.codex_home))
}

async fn human_turn(server: &mut AppServerSession, thread: ThreadId, text: &str) -> Result<()> {
    let request_id = server.next_request_id();
    server
        .request_handle()
        .request_typed::<TurnStartResponse>(ClientRequest::TurnStart {
            request_id,
            params: serde_json::from_value::<TurnStartParams>(json!({
                "threadId":thread,"input":[{"type":"text","text":text,"textElements":[]}],
            }))?,
        })
        .await?;
    Ok(())
}

async fn through_notification(
    app: &mut App,
    tui: &mut crate::tui::Tui,
    server: &mut AppServerSession,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    expected: impl Fn(&ServerNotification) -> bool,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30),async {
        loop {
            tokio::select! {
                event=server.next_event()=>{
                    let event=event.expect("the actual embedded backend remains connected");
                    let done=matches!(&event.event,AppServerEvent::ServerNotification(notification) if expected(notification));
                    if done && let AppServerEvent::ServerNotification(notification)=&event.event
                        && let ServerNotification::TurnCompleted(finished)=notification.as_ref() {
                        assert_eq!(finished.turn.status,codex_app_server_protocol::TurnStatus::Completed,
                            "the ordinary history turn must reach the controlled provider: {:?}",finished.turn.error);
                        assert_eq!(finished.turn.error,None);
                    }
                    app.handle_backend_event(server,event).await;
                    app.drain_active_thread_events(tui).await?;
                    if done {break;}
                }
                event=events.recv()=>{
                    let control=app.handle_event(tui,server,event.expect("the TUI event channel remains available")).await?;
                    assert!(matches!(control,AppRunControl::Continue),"the history journey must not exit: {control:?}");
                }
            }
        }
        while let Ok(event)=events.try_recv() {app.handle_event(tui,server,event).await?;}
        Ok::<_,color_eyre::Report>(())
    }).await?
}

fn history_text(app: &App) -> String {
    app.transcript_cells
        .iter()
        .flat_map(|cell| cell.display_lines(100))
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn load_older(
    app: &mut App,
    tui: &mut crate::tui::Tui,
    server: &mut AppServerSession,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    root: ThreadId,
) -> Result<usize> {
    let mut pages = 0;
    while server.has_older_history(root) {
        assert!(pages < 8, "bounded older pages must make progress");
        assert!(app.request_older_history_page(server, root));
        tokio::time::timeout(Duration::from_secs(30),async {
            loop {
                let event=events.recv().await.expect("older-page TUI response");
                let loaded=matches!(&event,AppEvent::OlderWorkflowTimelineLoaded {thread_id,..} if *thread_id==root);
                let control=app.handle_event(tui,server,event).await?;
                assert!(matches!(control,AppRunControl::Continue),"history paging must not exit: {control:?}");
                if loaded {break Ok::<_,color_eyre::Report>(());}
            }
        }).await??;
        pages += 1;
    }
    Ok(pages)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_workflow_history_is_rendered_once_between_human_turns_after_cold_paged_resume()
-> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let provider = responses::start_mock_server().await;
    let mut first = vec![responses::ev_response_created("before")];
    for index in 0..110 {
        let text = match index {
            0 => "OLDER_PAGE_START".to_owned(),
            109 => "HISTORY_BEFORE".to_owned(),
            _ => format!("Earlier ordinary conversation item {index}."),
        };
        first.push(responses::ev_assistant_message(
            &format!("ordinary-{index}"),
            &text,
        ));
    }
    first.push(responses::ev_completed("before"));
    let model = responses::mount_sse_sequence(
        &provider,
        vec![
            responses::sse(first),
            responses::sse(vec![
                responses::ev_assistant_message("after", "HISTORY_AFTER"),
                responses::ev_completed("after"),
            ]),
        ],
    )
    .await;
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let home = tempfile::tempdir()?;
    app.config.codex_home = home.path().to_path_buf().abs();
    app.config.cwd = home.path().abs();
    app.config.sqlite = codex_state::SqliteConfig::new_for_testing(home.path().abs());
    app.config.model = Some("gpt-5.6-sol".into());
    app.config.model_provider_id = "mock_provider".into();
    app.config.model_provider = codex_model_provider_info::ModelProviderInfo {
        name: "Controlled workflow history provider".into(),
        base_url: Some(format!("{}/v1", provider.uri())),
        request_max_retries: Some(0),
        stream_max_retries: Some(0),
        ..Default::default()
    };
    let mut info = construct_model_info_offline_for_tests(
        "gpt-5.6-sol",
        &app.config.to_models_manager_config(),
    );
    info.multi_agent_version = Some(codex_protocol::protocol::MultiAgentVersion::V2);
    info.tool_mode = Some(codex_protocol::openai_models::ToolMode::Direct);
    let catalog = home.path().join("workflow-models.json");
    std::fs::write(&catalog, serde_json::to_vec(&json!({"models":[info]}))?)?;
    app_test_support::MockResponsesConfig::new(&provider.uri())
        .with_model("gpt-5.6-sol")
        .enable_feature(codex_features::Feature::Collab)
        .enable_feature(codex_features::Feature::MultiAgentV2)
        .enable_feature(codex_features::Feature::CodeModeHost)
        .enable_feature(codex_features::Feature::DynamicWorkflows)
        .with_root_config(&format!(
            "model_catalog_json = {}",
            serde_json::to_string(&catalog)?
        ))
        .write(home.path())?;
    let config_path = home.path().join("config.toml");
    let mut persisted: toml::Value = toml::from_str(&std::fs::read_to_string(&config_path)?)?;
    let root_config = persisted
        .as_table_mut()
        .expect("a fixture config is a TOML table");
    root_config
        .entry("features")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .expect("fixture features are a TOML table")
        .insert(
            "multi_agent_v2".into(),
            toml::Value::try_from(json!({"enabled":true,"non_code_mode_only":false}))?,
        );
    // Thread navigation reloads this preference from the user's real config.
    root_config
        .entry("tui")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .expect("fixture TUI preferences are a TOML table")
        .insert(
            "terminal_resize_reflow_max_rows".into(),
            toml::Value::Integer(2),
        );
    std::fs::write(config_path, toml::to_string(&persisted)?)?;
    app.config = app
        .rebuild_config_for_cwd(home.path().to_path_buf())
        .await?;
    app.local_settings.transcript_mode = crate::transcript_mode::TranscriptMode::Terminal;
    app.local_settings.tui.alternate_screen = codex_config::types::AltScreenMode::Never;
    app.local_settings.tui.terminal_resize_reflow_max_rows = Some(2);
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut server = actual_server(&app).await?;
    let started = server.start_thread(&app.config).await?;
    let root = started.session.thread_id;
    // Naming is a supported user action that materializes an untouched thread.
    // Resume it before any turn so the real adapter caches Some(empty timeline).
    server
        .thread_set_name(root, "Warm workflow history".into())
        .await?;
    let started = server
        .resume_thread(
            &app.local_settings,
            app.config.clone(),
            root,
            crate::app_server_session::ResumeModelSettings::RestoreFromThread,
        )
        .await?;
    assert_eq!(server.timeline_entries(root), Some(Vec::new()));
    app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        started,
        super::super::session_lifecycle::ThreadAttachPresentation::Fresh,
        /*initial_user_message*/ None,
    )
    .await?;
    while events.try_recv().is_ok() {}
    app.thread_event_channels
        .get(&root)
        .expect("attached warm root")
        .store
        .lock()
        .await
        .capacity = 2;
    human_turn(
        &mut server,
        root,
        "Write the ordinary conversation before the workflow.",
    )
    .await?;
    through_notification(&mut app,&mut tui,&mut server,&mut events,|event|matches!(event,ServerNotification::TurnCompleted(done) if done.thread_id==root.to_string())).await?;
    let request_id = server.next_request_id();
    let started:WorkflowStartResponse=server.request_handle().request_typed(ClientRequest::WorkflowStart {
        request_id,params:WorkflowStartParams {thread_id:root.to_string(),source:Some("workflow.meta({name:'history-review',description:'Publish a human-visible retained report',phases:[]}); return {finding:'WORKFLOW_HISTORY_RESULT',count:3};".into()),saved_workflow_id:None,input:json!({}),completion_mode:None},
    }).await?;
    let run = started.workflow.id;
    through_notification(&mut app,&mut tui,&mut server,&mut events,|event|matches!(event,ServerNotification::WorkflowLifecycleChanged(fact)
        if fact.event.run_id==run && matches!(&fact.event.kind,codex_app_server_protocol::WorkflowLifecycleKind::Completed {..}))).await?;
    assert_eq!(
        model.requests().len(),
        1,
        "default display publication must not make an extra language-model request"
    );
    let live = history_text(&app);
    assert_eq!(live.matches("Started workflow history-review").count(), 1);
    assert_eq!(live.matches("Completed workflow history-review").count(), 1);
    assert!(live.contains("WORKFLOW_HISTORY_RESULT"));
    let before = live
        .find("HISTORY_BEFORE")
        .expect("the actual ordinary answer before the workflow");
    assert!(before < live.find("Started workflow history-review").unwrap());
    human_turn(
        &mut server,
        root,
        "Write the next ordinary human conversation after that report.",
    )
    .await?;
    through_notification(&mut app,&mut tui,&mut server,&mut events,|event|matches!(event,ServerNotification::TurnCompleted(done) if done.thread_id==root.to_string())).await?;
    assert_eq!(model.requests().len(), 2);
    assert!(
        !model.requests()[1].body_contains_text("WORKFLOW_HISTORY_RESULT"),
        "the full rendered report must stay outside the next model's context"
    );
    let live = history_text(&app);
    assert!(
        live.find("Completed workflow history-review").unwrap()
            < live.find("HISTORY_AFTER").unwrap()
    );
    let request_id = server.next_request_id();
    let latest: codex_app_server_protocol::ThreadTimelineListResponse = server
        .request_handle()
        .request_typed(ClientRequest::ThreadTimelineList {
            request_id,
            params: serde_json::from_value(json!({"threadId":root,"limit":100}))?,
        })
        .await?;
    assert_eq!(
        latest.data.len(),
        100,
        "the public timeline must retain distinct recorded items rather than only live cells"
    );
    let cursor = latest
        .next_cursor
        .expect("the actual recorded conversation exceeds one public timeline page");
    let request_id = server.next_request_id();
    let older: codex_app_server_protocol::ThreadTimelineListResponse = server
        .request_handle()
        .request_typed(ClientRequest::ThreadTimelineList {
            request_id,
            params: serde_json::from_value(json!({"threadId":root,"cursor":cursor,"limit":100}))?,
        })
        .await?;
    assert!(latest.data.len() + older.data.len() > 100);
    assert!(older.data.iter().any(|entry|matches!(entry,ThreadTimelineEntry::Item {item,..}
        if matches!(item.as_ref(),codex_app_server_protocol::ThreadItem::AgentMessage {text,..} if text=="OLDER_PAGE_START"))),
        "the oldest original assistant item must be persisted behind the public cursor");
    let other = server.start_thread(&app.config).await?;
    let other_root = other.session.thread_id;
    server
        .thread_set_name(other_root, "Independent navigation target".into())
        .await?;
    let switched = app
        .select_agents_overview_thread(&mut tui, &mut server, other_root)
        .await?;
    assert!(matches!(switched, AppRunControl::Continue));
    assert_eq!(app.current_displayed_thread_id(), Some(other_root));
    let returned = app
        .select_agents_overview_thread(&mut tui, &mut server, root)
        .await?;
    assert!(matches!(returned, AppRunControl::Continue));
    assert_eq!(app.current_displayed_thread_id(), Some(root));
    while let Ok(event) = events.try_recv() {
        app.handle_event(&mut tui, &mut server, event).await?;
    }
    let refreshed = server
        .timeline_entries(root)
        .expect("the warm canonical timeline remains available");
    assert!(refreshed.iter().any(|entry|matches!(entry,ThreadTimelineEntry::Item {item,..}
        if matches!(item.as_ref(),codex_app_server_protocol::ThreadItem::AgentMessage {text,..} if text=="HISTORY_AFTER"))),
        "an initially empty warm cache must refresh after live history instead of hiding new items on switch-back");
    load_older(&mut app, &mut tui, &mut server, &mut events, root).await?;
    let warm = history_text(&app);
    assert_eq!(warm.matches("Completed workflow history-review").count(), 1);
    assert!(
        warm.find("HISTORY_BEFORE").unwrap()
            < warm.find("Completed workflow history-review").unwrap()
    );
    assert!(
        warm.find("Completed workflow history-review").unwrap()
            < warm.find("HISTORY_AFTER").unwrap()
    );
    assert_eq!(
        model.requests().len(),
        2,
        "warm cache refresh and switching must not sample the model"
    );
    server.shutdown().await?;

    let (mut cold, mut cold_events, _ops) = make_test_app_with_channels().await;
    cold.config = app.config.clone();
    cold.local_settings = app.local_settings.clone();
    assert_eq!(
        cold.local_settings.tui.terminal_resize_reflow_max_rows,
        Some(2),
        "real navigation must retain the configured bounded history budget"
    );
    let mut server = actual_server(&cold).await?;
    let resumed = server
        .resume_thread(
            &cold.local_settings,
            cold.config.clone(),
            root,
            crate::app_server_session::ResumeModelSettings::RestoreFromThread,
        )
        .await?;
    cold.replace_chat_widget_with_app_server_thread(
        &mut tui,
        resumed,
        super::super::session_lifecycle::ThreadAttachPresentation::SessionLineage,
        /*initial_user_message*/ None,
    )
    .await?;
    while let Ok(event) = cold_events.try_recv() {
        cold.handle_event(&mut tui, &mut server, event).await?;
    }
    let initial = server
        .timeline_entries(root)
        .expect("the canonical timeline is available after cold resume");
    assert!(
        server.has_older_history(root),
        "more than one hundred actual recorded items must leave bounded older pages"
    );
    let pages = load_older(&mut cold, &mut tui, &mut server, &mut cold_events, root).await?;
    assert!(pages >= 1);
    let entries = server.timeline_entries(root).unwrap();
    assert!(entries.len() > 100 && entries.len() > initial.len());
    let facts = entries
        .iter()
        .filter_map(|entry| match entry {
            ThreadTimelineEntry::Workflow { event, .. } => Some(event),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(facts.len(), 2);
    assert_eq!(
        facts
            .iter()
            .map(|event| &event.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2
    );
    let rendered = history_text(&cold);
    assert!(
        rendered.contains("OLDER_PAGE_START"),
        "the oldest actual assistant item must be reachable through normal older-history UI paging"
    );
    assert_eq!(
        rendered.matches("Started workflow history-review").count(),
        1
    );
    assert_eq!(
        rendered
            .matches("Completed workflow history-review")
            .count(),
        1
    );
    assert!(
        rendered.find("HISTORY_BEFORE").unwrap()
            < rendered.find("Completed workflow history-review").unwrap()
    );
    assert!(
        rendered.find("Completed workflow history-review").unwrap()
            < rendered.find("HISTORY_AFTER").unwrap()
    );
    for event in facts {
        cold.chat_widget
            .on_workflow_lifecycle(event.as_ref().clone());
    }
    while let Ok(event) = cold_events.try_recv() {
        cold.handle_event(&mut tui, &mut server, event).await?;
    }
    assert_eq!(
        history_text(&cold),
        rendered,
        "a repeated live delivery after canonical replay must not duplicate its stable-ID history cells"
    );
    assert_eq!(
        model.requests().len(),
        2,
        "cold resume and older-page reads must not wake inference"
    );
    server.shutdown().await?;
    Ok(())
}
