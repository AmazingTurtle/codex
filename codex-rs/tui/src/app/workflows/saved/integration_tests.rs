//! Saved commands retain real UI routing, host persistence, VM execution and file watches.
use super::*;
use crate::workflows::WorkflowEvent;
use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::ServerNotification;
use pretty_assertions::assert_eq;
use serde_json::json;

fn source(name: &str) -> String {
    format!(
        "workflow.meta({{name:{name:?},description:'Run the saved review',phases:[]}}); return workflow.args;"
    )
}

async fn handle_until(
    app: &mut App,
    server: &mut AppServerSession,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    accepts: impl Fn(&WorkflowEvent) -> bool,
) -> Result<WorkflowEvent> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(AppEvent::Workflow(event)) = events.recv().await {
                if accepts(&event) {
                    return event;
                }
                app.handle_workflow_event(server, event).await;
            }
        }
    })
    .await
    .map_err(Into::into)
}

#[tokio::test]
async fn named_saved_command_executes_args_and_external_edits_refresh_legacy_source_and_catalog()
-> Result<()> {
    let project = tempfile::tempdir()?;
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    app.config.cwd = AbsolutePathBuf::from_absolute_path(project.path())?;
    let directory = project.path().join(".codex/workflows");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join("source-file.js"), source("saved-review"))?;
    let legacy = "await workflow.agent('legacy', {message:'Do not execute this source'});";
    std::fs::write(directory.join("legacy.js"), legacy)?;
    let target = crate::AppServerTarget::Embedded;
    let state_db = crate::init_state_db_for_app_server_target(&app.config, &target).await?;
    let provider = std::sync::Arc::new(
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
        std::sync::Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
        Default::default(),
        move |mut args| {
            args.code_mode_session_provider = Some(provider);
            codex_app_server_client::InProcessAppServerClient::start(args)
        },
    )
    .await?;
    let mut server = AppServerSession::new(
        crate::backend_id::BackendId::new(),
        crate::AppServerClient::InProcess(client),
        crate::app_server_session::ThreadParamsMode::Embedded,
    )
    .with_local_codex_home(&app.config.codex_home);
    let started = server.start_thread(&app.config).await?;
    let root = started.session.thread_id;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    while events.try_recv().is_ok() {}
    while ops.try_recv().is_ok() {}
    app.discover_workflows_after_attach(&server);
    let loaded = handle_until(&mut app, &mut server, &mut events, |event| {
        matches!(event, WorkflowEvent::Loaded { .. })
    })
    .await?;
    app.handle_workflow_event(&mut server, loaded).await;
    if app.workflows.lock().unwrap().catalog_loading {
        let catalog = handle_until(&mut app, &mut server, &mut events, |event| {
            matches!(event, WorkflowEvent::SavedCatalogLoaded { .. })
        })
        .await?;
        app.handle_workflow_event(&mut server, catalog).await;
    }
    assert!(
        app.workflows
            .lock()
            .unwrap()
            .saved
            .iter()
            .find(|row| row.id == "project:legacy")
            .unwrap()
            .read_only
    );
    assert!(
        app.workflows.lock().unwrap().saved_sources.is_empty(),
        "discovery never reads source into the UI cache"
    );
    app.chat_widget.insert_str("/saved-review {\"n\":3}");
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    let launch = handle_until(&mut app, &mut server, &mut events, |event| {
        matches!(event, WorkflowEvent::StartSaved { .. })
    })
    .await?;
    assert!(
        matches!(&launch, WorkflowEvent::StartSaved { owner, caller, id, input, .. } if *owner == root && *caller == root && id == "project:source-file" && input == &json!({"n":3}))
    );
    assert!(
        ops.try_recv().is_err(),
        "a native saved command does not submit an agent turn"
    );
    app.handle_workflow_event(&mut server, launch).await;
    let finished = handle_until(&mut app, &mut server, &mut events, |event| {
        matches!(event, WorkflowEvent::OperationFinished { .. })
    })
    .await?;
    assert!(
        matches!(
            &finished,
            WorkflowEvent::OperationFinished { result: Ok(()), .. }
        ),
        "{finished:?}"
    );
    app.handle_workflow_event(&mut server, finished).await;
    let completed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = server
                .next_event()
                .await
                .expect("running app-server event stream");
            if let AppServerEvent::ServerNotification(notification) = &event.event
                && let ServerNotification::WorkflowUpdated(update) = notification.as_ref()
                && update.workflow.status == codex_app_server_protocol::WorkflowStatus::Succeeded
            {
                break update.workflow.clone();
            }
            app.handle_app_server_event(&server, event.event).await;
        }
    })
    .await?;
    assert_eq!(
        (completed.owner_thread_id, completed.input, completed.result),
        (root.to_string(), json!({"n":3}), Some(json!({"n":3})))
    );
    app.handle_workflow_event(&mut server, WorkflowEvent::Open)
        .await;
    app.chat_widget.handle_key_event(KeyCode::Char('l').into());
    assert_eq!(
        app.workflows.lock().unwrap().center.saved_id.as_deref(),
        Some("project:legacy"),
        "opening Saved must retain the visible default row's stable identity for source invalidation"
    );
    app.chat_widget.handle_key_event(KeyCode::Char('3').into());
    let read = handle_until(&mut app, &mut server, &mut events, |event| {
        matches!(event, WorkflowEvent::ReadSavedSource { .. })
    })
    .await?;
    assert!(matches!(&read, WorkflowEvent::ReadSavedSource { id, .. } if id == "project:legacy"));
    app.handle_workflow_event(&mut server, read).await;
    let read = handle_until(&mut app, &mut server, &mut events, |event| {
        matches!(event, WorkflowEvent::SavedSourceLoaded { .. })
    })
    .await?;
    app.handle_workflow_event(&mut server, read).await;
    assert_eq!(
        app.workflows.lock().unwrap().saved_sources["project:legacy"]
            .source
            .as_deref(),
        Some(legacy)
    );
    if app.workflows.lock().unwrap().loading {
        let loaded = handle_until(&mut app, &mut server, &mut events, |event| {
            matches!(event, WorkflowEvent::Loaded { .. })
        })
        .await?;
        app.handle_workflow_event(&mut server, loaded).await;
    }
    let updated = source("review-v2");
    std::fs::write(directory.join("legacy.js"), &updated)?;
    let change = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = server.next_event().await.expect("watch event stream");
            if matches!(&event.event, AppServerEvent::ServerNotification(notification) if matches!(notification.as_ref(), ServerNotification::FsChanged(change) if change.watch_id.starts_with("workflow-library-"))) { break event.event; }
            app.handle_app_server_event(&server, event.event).await;
        }
    }).await?;
    app.handle_app_server_event(&server, change).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(AppEvent::Workflow(event)) = events.recv().await { app.handle_workflow_event(&mut server, event).await; }
            let state = app.workflows.lock().unwrap();
            let metadata_updated = state.saved.iter().find(|row| row.id == "project:legacy").and_then(|row| row.metadata.as_ref()).is_some_and(|metadata| metadata.name == "review-v2");
            let source_updated = state.saved_sources.get("project:legacy").and_then(|source| source.source.as_deref()) == Some(updated.as_str());
            let catalog_updated = !state.catalog_loading && state.command_catalog.iter().any(|command| command.name == "review-v2");
            if metadata_updated && source_updated && catalog_updated { break; }
        }
    }).await.map_err(|error| {
        let state = app.workflows.lock().unwrap();
        let metadata = state.saved.iter().find(|row| row.id == "project:legacy").and_then(|row| row.metadata.as_ref());
        let source = state.saved_sources.get("project:legacy");
        color_eyre::eyre::eyre!(
            "Saved external-edit refresh failed: {error}; selected={:?}, metadata={:?}, source_updated={}, source_loading={:?}, source_error={:?}, catalog_loading={}, catalog_updated={}, list_error={:?}",
            state.center.saved_id, metadata.map(|metadata| metadata.name.as_str()),
            source.and_then(|source| source.source.as_deref()) == Some(updated.as_str()),
            source.map(|source| source.loading), source.and_then(|source| source.error.as_deref()),
            state.catalog_loading, state.command_catalog.iter().any(|command| command.name == "review-v2"), state.load_error,
        )
    })?;
    assert_eq!(
        app.workflows.lock().unwrap().saved_sources["project:legacy"]
            .source
            .as_deref(),
        Some(updated.as_str())
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn saved_launch_keeps_the_originating_child_and_rejects_stale_root_dispatch() -> Result<()> {
    use codex_app_server_protocol::WorkflowStartParams;
    use tokio::net::TcpListener;

    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let root = ThreadId::new();
    let child = ThreadId::new();
    let later_displayed_child = ThreadId::new();
    app.enqueue_primary_thread_session(
        test_thread_session(root, app.config.cwd.to_path_buf()),
        Vec::new(),
    )
    .await?;
    app.active_thread_id = Some(later_displayed_child);
    app.workflows.lock().unwrap().owner = Some(root);
    while ops.try_recv().is_ok() {}
    while events.try_recv().is_ok() {}
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = crate::resolve_remote_addr(&format!("ws://{}", listener.local_addr()?))?;
    let host = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        super::disconnect::serve_reconnect_requests(tokio_tungstenite::accept_async(stream).await?, |request| {
            assert_eq!(request.method, "workflow/start");
            let params: WorkflowStartParams = serde_json::from_value(request.params.unwrap()).unwrap();
            assert_eq!((params.thread_id, params.saved_workflow_id, params.input), (child.to_string(), Some("project:review".into()), json!({"n":3})));
            std::future::ready(Some(json!({"error":{"code":-32602,"message":"workflow starts are unavailable from a leaf child"}})))
        }).await
    });
    let mut server = AppServerSession::new(
        crate::backend_id::BackendId::new(),
        crate::connect_remote_app_server(endpoint).await?,
        crate::app_server_session::ThreadParamsMode::Remote,
    );
    let request_id = Uuid::new_v4();
    app.handle_workflow_event(
        &mut server,
        WorkflowEvent::StartSaved {
            owner: root,
            caller: child,
            id: "project:review".into(),
            input: json!({"n":3}),
            request_id,
        },
    )
    .await;
    let finished = handle_until(&mut app, &mut server, &mut events, |event| {
        matches!(event, WorkflowEvent::OperationFinished { .. })
    })
    .await?;
    assert!(
        matches!(&finished, WorkflowEvent::OperationFinished { owner, request_id: actual, result: Err(error) } if *owner == root && *actual == request_id && error.contains("leaf child"))
    );
    app.handle_workflow_event(&mut server, finished).await;
    assert_eq!(
        (
            app.workflows.lock().unwrap().owner,
            app.current_displayed_thread_id()
        ),
        (Some(root), Some(later_displayed_child))
    );
    assert!(!app.workflows.lock().unwrap().operation_pending);
    assert!(ops.try_recv().is_err());
    let later_root = ThreadId::new();
    {
        let mut state = app.workflows.lock().unwrap();
        state.owner = Some(later_root);
        state.operation_id = None;
    }
    app.handle_workflow_event(
        &mut server,
        WorkflowEvent::StartSaved {
            owner: root,
            caller: child,
            id: "project:review".into(),
            input: json!({}),
            request_id: Uuid::new_v4(),
        },
    )
    .await;
    {
        let state = app.workflows.lock().unwrap();
        assert_eq!(
            (state.owner, state.operation_id, state.operation_pending),
            (Some(later_root), None, false),
            "a stale start cannot acquire a new root's operation state"
        );
    }
    server.shutdown().await?;
    host.await??;
    Ok(())
}

#[tokio::test]
async fn saved_source_cache_is_bounded_and_oversized_files_report_the_host_error() -> Result<()> {
    let project = tempfile::tempdir()?;
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    app.config.cwd = AbsolutePathBuf::from_absolute_path(project.path())?;
    let directory = project.path().join(".codex/workflows");
    std::fs::create_dir_all(&directory)?;
    for index in 0..9 {
        std::fs::write(
            directory.join(format!("source-{index}.js")),
            source(&format!("source-{index}")),
        )?;
    }
    std::fs::write(directory.join("large.js"), "x".repeat(24 * 1024 + 1))?;
    let mut server = start_config_write_test_app_server(&app).await?;
    let started = server.start_thread(&app.config).await?;
    let root = started.session.thread_id;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    app.workflows.lock().unwrap().owner = Some(root);
    for index in 0..9 {
        let id = format!("project:source-{index}");
        app.read_saved_workflow_source(&server, id.clone(), /*refresh*/ false);
        let loaded = handle_until(&mut app, &mut server, &mut events, |event| matches!(event, WorkflowEvent::SavedSourceLoaded { id: loaded, .. } if loaded == &id)).await?;
        app.handle_workflow_event(&mut server, loaded).await;
        assert_eq!(
            app.workflows.lock().unwrap().saved_sources[&id]
                .source
                .as_deref(),
            Some(source(&format!("source-{index}")).as_str())
        );
    }
    assert_eq!(app.workflows.lock().unwrap().saved_sources.len(), 8);
    app.finish_saved_workflow_source(
        "project:source-8".into(),
        Uuid::new_v4(),
        Ok("stale source".into()),
    );
    assert_eq!(
        app.workflows.lock().unwrap().saved_sources["project:source-8"]
            .source
            .as_deref(),
        Some(source("source-8").as_str())
    );
    app.read_saved_workflow_source(&server, "project:large".into(), /*refresh*/ false);
    let loaded = handle_until(&mut app, &mut server, &mut events, |event| matches!(event, WorkflowEvent::SavedSourceLoaded { id, .. } if id == "project:large")).await?;
    app.handle_workflow_event(&mut server, loaded).await;
    {
        let state = app.workflows.lock().unwrap();
        assert!(state.saved_sources["project:large"].source.is_none());
        assert!(
            state.saved_sources["project:large"]
                .error
                .as_ref()
                .unwrap()
                .contains("source limit")
        );
    }
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn command_catalog_spans_saved_pages_and_keeps_names_after_the_visible_page_changes()
-> Result<()> {
    let project = tempfile::tempdir()?;
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    app.config.cwd = AbsolutePathBuf::from_absolute_path(project.path())?;
    let directory = project.path().join(".codex/workflows");
    std::fs::create_dir_all(&directory)?;
    for index in 0..51 {
        let name = format!("workflow-{index:03}");
        std::fs::write(directory.join(format!("{name}.js")), source(&name))?;
    }
    let mut server = start_config_write_test_app_server(&app).await?;
    let started = server.start_thread(&app.config).await?;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    while events.try_recv().is_ok() {}
    app.discover_workflows_after_attach(&server);
    let catalog = handle_until(&mut app, &mut server, &mut events, |event| {
        matches!(event, WorkflowEvent::SavedCatalogLoaded { .. })
    })
    .await?;
    app.handle_workflow_event(&mut server, catalog).await;
    if app.workflows.lock().unwrap().loading {
        let loaded = handle_until(&mut app, &mut server, &mut events, |event| {
            matches!(event, WorkflowEvent::Loaded { .. })
        })
        .await?;
        app.handle_workflow_event(&mut server, loaded).await;
    }
    {
        let state = app.workflows.lock().unwrap();
        assert_eq!(
            (
                state.saved.len(),
                state.command_catalog.len(),
                state.catalog_overflow
            ),
            (50, 51, false)
        );
    }
    app.handle_workflow_event(&mut server, WorkflowEvent::NextPage { saved: true })
        .await;
    let loaded = handle_until(&mut app, &mut server, &mut events, |event| {
        matches!(event, WorkflowEvent::Loaded { .. })
    })
    .await?;
    app.handle_workflow_event(&mut server, loaded).await;
    {
        let state = app.workflows.lock().unwrap();
        assert_eq!(
            (
                state.saved.len(),
                state.saved[0].id.as_str(),
                state.command_catalog.len()
            ),
            (1, "project:workflow-050", 51)
        );
    }
    app.sync_workflow_presentation();
    app.chat_widget.insert_str("/workflow-000");
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    let launch = handle_until(&mut app, &mut server, &mut events, |event| {
        matches!(event, WorkflowEvent::StartSaved { .. })
    })
    .await?;
    assert!(
        matches!(launch, WorkflowEvent::StartSaved { id, input, .. } if id == "project:workflow-000" && input == json!({}))
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn incomplete_catalog_disables_unproven_aliases_with_visible_scoped_fallback() -> Result<()> {
    use codex_app_server_protocol::WorkflowSavedListParams;
    use tokio::net::TcpListener;

    for (total, description_bytes) in [(2049, 20), (50, 100_000)] {
        let (mut app, mut events, _ops) = make_test_app_with_channels().await;
        let root = ThreadId::new();
        app.enqueue_primary_thread_session(
            test_thread_session(root, app.config.cwd.to_path_buf()),
            Vec::new(),
        )
        .await?;
        {
            let mut state = app.workflows.lock().unwrap();
            state.owner = Some(root);
            state.attached_thread = Some(root);
        }
        while events.try_recv().is_ok() {}
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = crate::resolve_remote_addr(&format!("ws://{}", listener.local_addr()?))?;
        // This strict external server boundary also exercises a nonconforming large
        // description: the UI cache bound must hold independently of producer validation.
        let host = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            super::disconnect::serve_reconnect_requests(tokio_tungstenite::accept_async(stream).await?, |request| {
                assert_eq!(request.method, "workflow/saved/list");
                let params: WorkflowSavedListParams = serde_json::from_value(request.params.unwrap()).unwrap();
                assert_eq!((params.thread_id.as_str(), params.limit), (root.to_string().as_str(), Some(50)));
                let start = params.cursor.map(|cursor| cursor.parse::<usize>().unwrap()).unwrap_or(0);
                let end = (start + 50).min(total);
                let data = (start..end).map(|index| json!({"id":format!("project:workflow-{index}"),"name":format!("workflow-{index}"),"scope":"project","metadata":{"name":format!("workflow-{index}"),"description":"x".repeat(description_bytes),"phases":[]},"readOnly":false,"readOnlyReason":null,"formatVersion":2})).collect::<Vec<_>>();
                std::future::ready(Some(json!({"result":{"data":data,"nextCursor":(end < total).then(|| end.to_string()),"watchRoots":[]}})))
            }).await
        });
        let mut server = AppServerSession::new(
            crate::backend_id::BackendId::new(),
            crate::connect_remote_app_server(endpoint).await?,
            crate::app_server_session::ThreadParamsMode::Remote,
        );
        app.refresh_saved_workflow_catalog(&server);
        let loaded = handle_until(&mut app, &mut server, &mut events, |event| {
            matches!(event, WorkflowEvent::SavedCatalogLoaded { .. })
        })
        .await?;
        app.handle_workflow_event(&mut server, loaded).await;
        {
            let state = app.workflows.lock().unwrap();
            assert!(state.catalog_overflow);
            assert!(state.command_catalog.len() <= 2048);
            assert!(
                state
                    .command_catalog
                    .iter()
                    .map(|entry| entry.name.len() + entry.id.len() + entry.description.len())
                    .sum::<usize>()
                    <= 4 * 1024 * 1024
            );
        }
        let count = app.workflows.lock().unwrap().command_catalog.len();
        app.handle_workflow_event(
            &mut server,
            WorkflowEvent::SavedCatalogLoaded {
                owner: root,
                request_id: Uuid::new_v4(),
                result: Ok(crate::workflows::saved_commands::SavedCatalog {
                    candidates: Vec::new(),
                    overflow: false,
                }),
            },
        )
        .await;
        {
            let state = app.workflows.lock().unwrap();
            assert_eq!(
                (state.catalog_overflow, state.command_catalog.len()),
                (true, count),
                "a superseded result cannot re-enable an incomplete namespace"
            );
        }
        app.sync_workflow_presentation();
        app.chat_widget.insert_str("/workflow-0");
        app.chat_widget.handle_key_event(KeyCode::Enter.into());
        assert!(
            !std::iter::from_fn(|| events.try_recv().ok())
                .any(|event| matches!(event, AppEvent::Workflow(WorkflowEvent::StartSaved { .. })))
        );
        app.chat_widget
            .show_bottom_pane_view(Box::new(crate::workflows::WorkflowCenter::new(
                app.workflows.clone(),
                app.app_event_tx.clone(),
            )));
        app.chat_widget.handle_key_event(KeyCode::Char('l').into());
        let fallback = render_bottom_popup(&app.chat_widget, 100);
        assert!(
            fallback.contains("catalog limit reached") && fallback.contains("/workflows"),
            "{fallback}"
        );
        server.shutdown().await?;
        host.await??;
    }
    Ok(())
}
