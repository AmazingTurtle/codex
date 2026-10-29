use super::backend_banner_fallback_tests::fallback_response;
use super::*;
use crate::chatwidget::UserMessage;
use crate::chatwidget::tests::helpers::set_fast_mode_test_catalog_for_models;
use codex_app_server_protocol::FsWatchParams;
use codex_app_server_protocol::ThreadExecutionControlReadParams;
use codex_app_server_protocol::ThreadSettingsUpdateParams;
use codex_app_server_protocol::WorkflowListParams;
use codex_app_server_protocol::WorkflowListScope;
use codex_app_server_protocol::WorkflowSavedListParams;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn backend_banner_fallback_applies_before_initial_and_queued_startup_prompts() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut config = app.config.clone();
    config.model = Some("gpt-5.5".into());
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let init = app.chatwidget_init_for_forked_or_resumed_thread(
        &mut tui,
        config.clone(),
        Some(UserMessage::from("initial prompt")),
    );
    app.replace_chat_widget(ChatWidget::new_with_app_event(init));
    set_chatgpt_auth(&mut app.chat_widget);
    set_fast_mode_test_catalog_for_models(&mut app.chat_widget, "gpt-5.5", "gpt-5.6-terra");
    app.model_catalog = app.chat_widget.model_catalog();
    app.pending_startup_thread_start = true;
    app.chat_widget.set_model("gpt-5.5");
    app.chat_widget
        .set_queue_submissions_until_session_configured(/*queue*/ true);
    app.chat_widget
        .apply_external_edit("queued follow-up".into());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let (mut server, requests, proxy) = session_lifecycle_requests::start_recording_app_server(
        &config, /*blocked_thread_list*/ None, /*failed_thread_name*/ None,
    )
    .await?;
    let generation = app.rate_limit_hard_stop_generation;
    app.handle_event(
        &mut tui,
        &mut server,
        AppEvent::RateLimitsLoaded {
            request_id: 1,
            origin: RateLimitRefreshOrigin::StartupPrefetch {
                reset_hint_request_id: 0,
            },
            hard_stop_generation: generation,
            result: Ok(fallback_response()),
        },
    )
    .await?;
    assert!(requests.lock().unwrap().is_empty());
    assert!(
        !render_bottom_popup(&app.chat_widget, /*width*/ 72)
            .contains("Selected model usage exhausted")
    );
    let started = server.start_thread(&config).await?;
    let thread_id = started.session.thread_id;
    requests.lock().unwrap().clear();
    while events.try_recv().is_ok() {}
    app.handle_startup_thread_started(&mut server, Ok(started))
        .await?;
    assert_eq!(app.active_thread_id, Some(thread_id));
    let mut workflow_loaded = false;
    let mut catalog_loaded = false;
    let mut submissions = Vec::new();
    let mut pending =
        std::iter::from_fn(|| events.try_recv().ok()).collect::<std::collections::VecDeque<_>>();
    // Discovery and the native command catalog run independently. Drain already queued
    // submissions and await both results before asserting their complete request set.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = if let Some(event) = pending.pop_front() {
                event
            } else if workflow_loaded && catalog_loaded {
                break;
            } else {
                events
                    .recv()
                    .await
                    .expect("startup event channel remains open")
            };
            match event {
                AppEvent::Workflow(crate::workflows::WorkflowEvent::Loaded {
                    owner,
                    result,
                    ..
                }) => {
                    assert_eq!(
                        (
                            owner,
                            result.expect("startup workflow discovery succeeds").root
                        ),
                        (thread_id, thread_id)
                    );
                    workflow_loaded = true;
                }
                AppEvent::Workflow(crate::workflows::WorkflowEvent::SavedCatalogLoaded {
                    owner,
                    result,
                    ..
                }) => {
                    assert_eq!(owner, thread_id);
                    let catalog =
                        result.expect("startup native command catalog discovery succeeds");
                    assert!(
                        !catalog.overflow,
                        "the fixture catalog fits its bounded discovery"
                    );
                    catalog_loaded = true;
                }
                AppEvent::CodexOp(AppCommand::UserTurn { model, items, .. }) => {
                    submissions.push((model, items))
                }
                _ => {}
            }
        }
    })
    .await?;
    assert_eq!(
        submissions,
        vec![(
            "gpt-5.6-terra".into(),
            vec![UserInput::Text {
                text: "initial prompt".into(),
                text_elements: Vec::new(),
            }]
        )]
    );
    assert_eq!(
        app.chat_widget.queued_user_message_texts(),
        vec!["queued follow-up"]
    );
    let recorded = requests.lock().unwrap().clone();
    let mut methods = recorded
        .iter()
        .map(|request| request.method.clone())
        .collect::<Vec<_>>();
    methods.sort();
    assert_eq!(
        methods,
        vec![
            "fs/watch",
            "fs/watch",
            "thread/executionControl/read",
            "thread/settings/update",
            "workflow/list",
            "workflow/saved/list",
            "workflow/saved/list",
        ]
    );
    let params = |method: &str| {
        recorded
            .iter()
            .filter(|request| request.method == method)
            .map(|request| {
                request
                    .params
                    .clone()
                    .expect("startup requests carry typed parameters")
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        serde_json::from_value::<ThreadExecutionControlReadParams>(
            params("thread/executionControl/read").remove(0)
        )?,
        ThreadExecutionControlReadParams {
            thread_id: thread_id.to_string()
        }
    );
    assert_eq!(
        serde_json::from_value::<WorkflowListParams>(params("workflow/list").remove(0))?,
        WorkflowListParams {
            thread_id: thread_id.to_string(),
            scope: Some(WorkflowListScope::Current),
            cursor: None,
            limit: Some(50)
        }
    );
    let saved = params("workflow/saved/list")
        .into_iter()
        .map(serde_json::from_value::<WorkflowSavedListParams>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let expected_saved = WorkflowSavedListParams {
        thread_id: thread_id.to_string(),
        cursor: None,
        limit: Some(50),
    };
    assert_eq!(saved, vec![expected_saved.clone(), expected_saved]);
    let watches = params("fs/watch")
        .into_iter()
        .map(serde_json::from_value::<FsWatchParams>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert!(
        watches
            .iter()
            .all(|watch| watch.watch_id.starts_with("workflow-library-"))
    );
    assert_ne!(
        watches[0].watch_id, watches[1].watch_id,
        "the two libraries have distinct connection-owned watches"
    );
    let mut watched_paths = watches
        .into_iter()
        .map(|watch| watch.path)
        .collect::<Vec<_>>();
    watched_paths.sort();
    let mut expected_paths = vec![
        config.cwd.join(".codex").join("workflows"),
        config.codex_home.join("workflows"),
    ];
    expected_paths.sort();
    assert_eq!(watched_paths, expected_paths);
    let mode = app.chat_widget.effective_collaboration_mode();
    assert_eq!(
        serde_json::from_value::<ThreadSettingsUpdateParams>(
            params("thread/settings/update").remove(0)
        )?,
        ThreadSettingsUpdateParams {
            thread_id: thread_id.to_string(),
            model: Some("gpt-5.6-terra".into()),
            effort: mode.settings.reasoning_effort.clone(),
            collaboration_mode: Some(mode),
            service_tier: Some(Some("default".into())),
            ..Default::default()
        }
    );
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}
