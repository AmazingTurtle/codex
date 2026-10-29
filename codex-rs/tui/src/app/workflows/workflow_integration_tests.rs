use super::*;
use crate::workflows::WorkflowEvent;
use crate::workflows::WorkflowRow;
use codex_features::DynamicWorkflowsMode;
use pretty_assertions::assert_eq;

use super::persisted_workflow_fixture;

#[tokio::test]
async fn idle_draw_does_not_poll_but_attachment_discovers_existing_workflows() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut server = start_config_write_test_app_server(&app).await?;
    let started = server.start_thread(&app.config).await?;
    let root = started.session.thread_id;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    while events.try_recv().is_ok() {}
    app.sync_workflow_presentation();
    app.sync_workflow_presentation();
    assert!(
        events.try_recv().is_err(),
        "empty idle presentation must not enqueue workflow RPCs"
    );
    // Existing durable work is the prerequisite; this test exercises discovery,
    // while the core/API acceptance suites exercise actual VM execution.
    let workflow =
        persisted_workflow_fixture::create(&app.config.codex_home, root, app.config.cwd.as_path())?;
    app.discover_workflows_after_attach(&server);
    let page = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(AppEvent::Workflow(WorkflowEvent::Loaded { result, .. })) =
                events.recv().await
            {
                break result.expect("attached workflow discovery succeeds");
            }
        }
    })
    .await?;
    assert_eq!(
        page.rows
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        vec![workflow.id.as_str()]
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn current_default_is_local_and_explicit_project_discovers_independent_conversations()
-> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut server = start_config_write_test_app_server(&app).await?;
    let main = server.start_thread(&app.config).await?;
    let root = main.session.thread_id;
    app.enqueue_primary_thread_session(main.session, main.turns)
        .await?;
    let other = server.start_thread(&app.config).await?;
    let other_root = other.session.thread_id;
    let own_run =
        persisted_workflow_fixture::create(&app.config.codex_home, root, app.config.cwd.as_path())?;
    let other_run = persisted_workflow_fixture::create(
        &app.config.codex_home,
        other_root,
        app.config.cwd.as_path(),
    )?;
    while events.try_recv().is_ok() {}
    app.discover_workflows_after_attach(&server);
    let loaded = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(AppEvent::Workflow(event @ WorkflowEvent::Loaded { .. })) =
                events.recv().await
            {
                break event;
            }
        }
    })
    .await?;
    app.handle_workflow_event(&mut server, loaded).await;
    {
        let state = app.workflows.lock().unwrap();
        assert_eq!(state.scope, crate::workflows::WorkflowListScope::Current);
        assert_eq!(
            state
                .rows
                .iter()
                .map(|row| (row.id.clone(), row.owner))
                .collect::<std::collections::BTreeMap<_, _>>(),
            std::collections::BTreeMap::from([(own_run.id.clone(), Some(root))])
        );
    }
    app.handle_workflow_event(
        &mut server,
        WorkflowEvent::SetScope(crate::workflows::WorkflowListScope::Project),
    )
    .await;
    let loaded = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(AppEvent::Workflow(event @ WorkflowEvent::Loaded { .. })) =
                events.recv().await
            {
                break event;
            }
        }
    })
    .await?;
    app.handle_workflow_event(&mut server, loaded).await;
    {
        let state = app.workflows.lock().unwrap();
        assert_eq!(state.scope, crate::workflows::WorkflowListScope::Project);
        assert_eq!(
            state
                .rows
                .iter()
                .map(|row| (row.id.clone(), row.owner))
                .collect::<std::collections::BTreeMap<_, _>>(),
            std::collections::BTreeMap::from([
                (own_run.id, Some(root)),
                (other_run.id, Some(other_root))
            ])
        );
    }
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn scope_picker_requires_a_selection_and_preserves_the_composer_draft() -> Result<()> {
    use crate::workflows::WorkflowListScope;

    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut server = start_config_write_test_app_server(&app).await?;
    let started = server.start_thread(&app.config).await?;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    app.chat_widget
        .insert_str("draft stays in this conversation");
    app.handle_workflow_event(&mut server, WorkflowEvent::Open)
        .await;
    while events.try_recv().is_ok() {}
    app.handle_workflow_event(&mut server, WorkflowEvent::ChooseScope)
        .await;
    insta::assert_snapshot!(
        "workflow_explicit_scope_picker",
        render_bottom_popup(&app.chat_widget, /*width*/ 96)
    );
    assert_eq!(
        app.workflows.lock().unwrap().scope,
        WorkflowListScope::Current
    );
    app.chat_widget.handle_key_event(KeyCode::Esc.into());
    assert_eq!(
        app.workflows.lock().unwrap().scope,
        WorkflowListScope::Current
    );
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::Workflow(WorkflowEvent::SetScope(_))))
    );
    app.handle_workflow_event(&mut server, WorkflowEvent::ChooseScope)
        .await;
    app.chat_widget.handle_key_event(KeyCode::Down.into());
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    let selected = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::Workflow(event @ WorkflowEvent::SetScope(_)) => Some(event),
            _ => None,
        })
        .expect("only selecting a scope submits the change");
    assert!(matches!(
        selected,
        WorkflowEvent::SetScope(WorkflowListScope::Project)
    ));
    app.handle_workflow_event(&mut server, selected).await;
    assert_eq!(
        app.workflows.lock().unwrap().scope,
        WorkflowListScope::Project
    );
    app.handle_workflow_event(&mut server, WorkflowEvent::Open)
        .await;
    assert_eq!(
        app.workflows.lock().unwrap().scope,
        WorkflowListScope::Project
    );
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "draft stays in this conversation"
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn workflow_open_uses_independent_side_but_retains_primary_root_for_worker_views()
-> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut server = start_config_write_test_app_server(&app).await?;
    let main = server.start_thread(&app.config).await?;
    let main_id = main.session.thread_id;
    app.enqueue_primary_thread_session(main.session, main.turns)
        .await?;
    let side = server.start_thread(&app.config).await?;
    let side_id = side.session.thread_id;
    app.side_threads
        .insert(side_id, super::super::side::SideThreadState::new(main_id));
    let worker_id = ThreadId::new();

    for (displayed, expected_root) in [(side_id, side_id), (worker_id, main_id)] {
        // The worker case exercises the existing inspected-agent attachment state;
        // the primary and independent side are real loaded app-server conversations.
        app.active_thread_id = Some(displayed);
        app.sync_workflow_presentation();
        assert_eq!(
            app.workflows.lock().unwrap().attached_thread,
            Some(expected_root)
        );
        app.handle_workflow_event(&mut server, WorkflowEvent::Open)
            .await;
        let loaded = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = events
                    .recv()
                    .await
                    .expect("workflow event sender remains open");
                if matches!(&event, AppEvent::Workflow(WorkflowEvent::Loaded { .. })) {
                    break event;
                }
            }
        })
        .await?;
        let AppEvent::Workflow(WorkflowEvent::Loaded { owner, result, .. }) = &loaded else {
            unreachable!("the event loop selects workflow completion");
        };
        assert_eq!(
            (
                *owner,
                result.as_ref().expect("workflow listing succeeds").root
            ),
            (expected_root, expected_root)
        );
        app.handle_event(&mut tui, &mut server, loaded).await?;
        assert_eq!(app.workflows.lock().unwrap().owner, Some(expected_root));
    }
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn workflow_down_open_and_escape_preserve_composer_and_left_navigation() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut server = start_config_write_test_app_server(&app).await?;
    app.workflows.lock().unwrap().rows.push(WorkflowRow {
        id: "review".into(),
        title: "Review source".into(),
        status: "Running".into(),
        ..Default::default()
    });
    app.chat_widget.set_workflow_state(app.workflows.clone());
    app.chat_widget
        .set_agents_navigation_enabled(/*enabled*/ true);
    app.handle_tui_event(&mut tui, &mut server, TuiEvent::Key(KeyCode::Down.into()))
        .await?;
    assert!(!app.workflows.lock().unwrap().strip_focused);
    let open = std::iter::from_fn(|| events.try_recv().ok())
        .find(|event| matches!(event, AppEvent::Workflow(WorkflowEvent::Open)))
        .expect("Down opens the workflow command center directly");
    app.handle_event(&mut tui, &mut server, open).await?;
    let area = ratatui::layout::Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 120, /*height*/ 40,
    );
    let mut buffer = ratatui::buffer::Buffer::empty(area);
    crate::render::renderable::Renderable::render(
        &app.chat_widget.as_renderable(),
        area,
        &mut buffer,
    );
    let lines = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>();
    assert!(
        lines[0].to_lowercase().contains("workflow command center"),
        "{}",
        lines.join("\n")
    );
    assert!(
        lines.last().unwrap().to_lowercase().contains("esc"),
        "the command-center footer must use the full viewport: {}",
        lines.join("\n")
    );
    insta::assert_snapshot!("workflow_center_full_height_backdrop", lines.join("\n"));
    app.handle_tui_event(&mut tui, &mut server, TuiEvent::Key(KeyCode::Esc.into()))
        .await?;
    assert!(!app.workflows.lock().unwrap().strip_focused);
    app.handle_tui_event(&mut tui, &mut server, TuiEvent::Key(KeyCode::Left.into()))
        .await?;
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::OpenAgentsOverview))
    );
    app.chat_widget.insert_str("draft survives");
    app.handle_tui_event(&mut tui, &mut server, TuiEvent::Key(KeyCode::Down.into()))
        .await?;
    assert!(!app.workflows.lock().unwrap().strip_focused);
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "draft survives"
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn workflow_policy_popup_keeps_down_and_preserves_the_draft() {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    app.workflows.lock().unwrap().rows.push(WorkflowRow {
        id: "review".into(),
        ..Default::default()
    });
    app.chat_widget.set_workflow_state(app.workflows.clone());
    app.chat_widget.insert_str("unfinished question");
    for (mode, snapshot) in [
        (DynamicWorkflowsMode::Auto, "workflow_policy_auto"),
        (DynamicWorkflowsMode::Explicit, "workflow_policy_explicit"),
        (DynamicWorkflowsMode::Off, "workflow_policy_off"),
    ] {
        app.config.dynamic_workflows_mode = mode;
        app.open_workflow_policy();
        insta::assert_snapshot!(
            snapshot,
            render_bottom_popup(&app.chat_widget, /*width*/ 96)
        );
        app.chat_widget.handle_key_event(KeyCode::Esc.into());
        assert_eq!(
            app.chat_widget.composer_text_with_pending(),
            "unfinished question"
        );
    }
    app.chat_widget.apply_external_edit(String::new());
    app.config.dynamic_workflows_mode = DynamicWorkflowsMode::Auto;
    app.open_workflow_policy();
    app.chat_widget.handle_key_event(KeyCode::Down.into());
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    assert!(!app.workflows.lock().unwrap().strip_focused);
    assert!(
        std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
            event,
            AppEvent::Workflow(WorkflowEvent::SetMode(DynamicWorkflowsMode::Explicit))
        ))
    );
}

#[tokio::test]
async fn workflow_refresh_rejects_old_owner_generation_and_scope_responses() -> Result<()> {
    use crate::workflows::WorkflowListScope;
    use crate::workflows::WorkflowPage;
    use codex_app_server_protocol::ThreadExecutionControl;
    use codex_app_server_protocol::ThreadExecutionControlChangedNotification;
    use codex_protocol::protocol::ExecutionControlState;

    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut server = start_config_write_test_app_server(&app).await?;
    let owner = ThreadId::from_u128(42);
    let old_request = Uuid::from_u128(1);
    let current_request = Uuid::from_u128(2);
    {
        let mut state = app.workflows.lock().unwrap();
        state.owner = Some(owner);
        state.attached_thread = Some(owner);
        state.request_id = Some(current_request);
        state.scope = WorkflowListScope::All;
        state.loading = true;
        state.rows.push(WorkflowRow {
            id: "current".into(),
            owner: Some(owner),
            ..Default::default()
        });
    }
    // The owner is A again after A -> B -> A, but the first A request is obsolete.
    app.handle_workflow_event(
        &mut server,
        WorkflowEvent::Loaded {
            owner,
            request_id: old_request,
            scope: WorkflowListScope::Current,
            result: Ok(WorkflowPage {
                root: owner,
                rows: vec![WorkflowRow {
                    id: "stale".into(),
                    ..Default::default()
                }],
                saved: Vec::new(),
                next_cursor: None,
                saved_next_cursor: None,
            }),
        },
    )
    .await;
    app.handle_workflow_event(
        &mut server,
        WorkflowEvent::ExecutionSnapshot {
            request_id: old_request,
            snapshot: ThreadExecutionControlChangedNotification {
                thread_id: owner.to_string(),
                execution_control: ThreadExecutionControl {
                    root_thread_id: owner.to_string(),
                    state: ExecutionControlState::Paused,
                    can_pause: false,
                },
            },
        },
    )
    .await;
    {
        let state = app.workflows.lock().unwrap();
        assert_eq!(
            (state.loading, state.rows[0].id.as_str(), state.execution),
            (true, "current", None)
        );
    }
    app.workflows.lock().unwrap().refresh_pending = true;
    app.handle_workflow_event(
        &mut server,
        WorkflowEvent::Loaded {
            owner,
            request_id: current_request,
            scope: WorkflowListScope::Project,
            result: Ok(WorkflowPage {
                root: owner,
                rows: vec![WorkflowRow {
                    id: "wrong-scope".into(),
                    ..Default::default()
                }],
                saved: Vec::new(),
                next_cursor: None,
                saved_next_cursor: None,
            }),
        },
    )
    .await;
    {
        let state = app.workflows.lock().unwrap();
        assert_eq!(
            (state.loading, state.rows[0].id.as_str(), state.scope),
            (false, "current", WorkflowListScope::All)
        );
    }
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::Workflow(WorkflowEvent::Refresh)))
    );
    let follow_up_request = Uuid::from_u128(3);
    {
        let mut state = app.workflows.lock().unwrap();
        state.request_id = Some(follow_up_request);
        state.loading = true;
    }
    app.handle_workflow_event(
        &mut server,
        WorkflowEvent::Loaded {
            owner,
            request_id: follow_up_request,
            scope: WorkflowListScope::All,
            result: Ok(WorkflowPage {
                root: owner,
                rows: vec![WorkflowRow {
                    id: "fresh".into(),
                    owner: Some(owner),
                    ..Default::default()
                }],
                saved: Vec::new(),
                next_cursor: Some("next-runs".into()),
                saved_next_cursor: Some("next-saved".into()),
            }),
        },
    )
    .await;
    {
        let state = app.workflows.lock().unwrap();
        assert_eq!(
            (
                state
                    .rows
                    .iter()
                    .map(|row| row.id.clone())
                    .collect::<Vec<_>>(),
                state.page.next.as_deref(),
                state.saved_page.next.as_deref()
            ),
            (
                vec!["fresh".to_string()],
                Some("next-runs"),
                Some("next-saved")
            )
        );
    }
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn workflow_save_name_and_scope_keep_the_composer_draft() {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let owner = ThreadId::from_u128(42);
    app.workflows.lock().unwrap().rows.push(WorkflowRow {
        id: "review".into(),
        title: "Review source".into(),
        owner: Some(owner),
        ..Default::default()
    });
    app.chat_widget.insert_str("unfinished question");
    app.open_save_workflow(owner, "review".into());
    insta::assert_snapshot!(
        "workflow_save_name",
        render_bottom_popup(&app.chat_widget, /*width*/ 96)
    );
    app.chat_widget.handle_key_event(KeyCode::Esc.into());
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "unfinished question"
    );
    app.edit_workflow_name(
        owner,
        "review".into(),
        "bad name".into(),
        /*context*/ None,
    );
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    insta::assert_snapshot!(
        "workflow_save_invalid_name",
        render_bottom_popup(&app.chat_widget, /*width*/ 96)
    );
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
            event,
            AppEvent::Workflow(WorkflowEvent::ChooseSaveScope { .. })
        ))
    );
    app.chat_widget.handle_key_event(KeyCode::End.into());
    for _ in "bad name".chars() {
        app.chat_widget.handle_key_event(KeyCode::Backspace.into());
    }
    app.chat_widget.handle_paste("Review-project".into());
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    let next = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::Workflow(WorkflowEvent::ChooseSaveScope { owner, id, name }) => {
                Some((owner, id, name))
            }
            _ => None,
        })
        .expect("corrected identifier opens the destination picker");
    assert_eq!(next, (owner, "review".into(), "Review-project".into()));
    app.choose_workflow_save_scope(next.0, next.1, next.2);
    insta::assert_snapshot!(
        "workflow_save_scope",
        render_bottom_popup(&app.chat_widget, /*width*/ 96)
    );
    app.chat_widget.handle_key_event(KeyCode::Down.into());
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
        event, AppEvent::Workflow(WorkflowEvent::SaveNamed { owner: selected_owner, id, name, scope: codex_app_server_protocol::WorkflowScope::User })
            if selected_owner == owner && id == "review" && name == "Review-project"
    )));
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "unfinished question"
    );
}

#[tokio::test]
async fn workflow_policy_replaces_legacy_boolean_and_reloads_persisted_mode() -> Result<()> {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let config_path = app.config.codex_home.join("config.toml");
    std::fs::write(&config_path, "[features]\ndynamic_workflows = true\n")?;
    let mut server = start_config_write_test_app_server(&app).await?;
    for mode in [
        DynamicWorkflowsMode::Explicit,
        DynamicWorkflowsMode::Off,
        DynamicWorkflowsMode::Auto,
    ] {
        app.persist_workflow_policy(&mut server, mode).await;
        assert_eq!(app.config.dynamic_workflows_mode, mode);
        let saved: toml::Value = toml::from_str(&std::fs::read_to_string(&config_path)?)?;
        assert_eq!(
            saved["features"]["dynamic_workflows"].clone(),
            toml::Value::try_from(serde_json::json!({"enabled": true, "mode": mode}))?
        );
        app.refresh_in_memory_config_from_disk().await?;
        assert_eq!(app.config.dynamic_workflows_mode, mode);
    }
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn paused_notice_keeps_human_submission_on_the_normal_user_turn_path() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let mut server = start_config_write_test_app_server(&app).await?;
    let started = server.start_thread(&app.config).await?;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    app.chat_widget.set_execution_control(
        Some(codex_protocol::protocol::ExecutionControlState::Paused),
        /*can_pause*/ false,
    );
    app.chat_widget
        .insert_str("Continue with this new instruction");
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    let Op::UserTurn { items, .. } = next_user_turn_op(&mut ops) else {
        unreachable!("helper returns a user turn");
    };
    assert_eq!(
        items,
        vec![UserInput::Text {
            text: "Continue with this new instruction".into(),
            text_elements: Vec::new(),
        }]
    );
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
            event,
            AppEvent::Workflow(WorkflowEvent::ConfirmExecution { .. })
        ))
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn workflow_pause_confirms_parent_scope_and_cancel_preserves_execution() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut server = start_config_write_test_app_server(&app).await?;
    let started = server.start_thread(&app.config).await?;
    let root = started.session.thread_id;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    app.chat_widget.insert_str("draft survives confirmation");
    app.chat_widget
        .show_bottom_pane_view(Box::new(crate::workflows::WorkflowCenter::new(
            app.workflows.clone(),
            app.app_event_tx.clone(),
        )));
    app.show_execution_confirmation(root, "Review change".into(), /*pause*/ true);
    insta::assert_snapshot!(
        "workflow_pause_parent_confirmation",
        render_bottom_popup(&app.chat_widget, /*width*/ 100)
            .replace(&root.to_string(), "ROOT_THREAD")
    );
    app.chat_widget.handle_key_event(KeyCode::Esc.into());
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
            event,
            AppEvent::Workflow(WorkflowEvent::ConfirmExecution { .. })
        ))
    );
    let control =
        crate::app_server_session::workflows::execution_control(&server.request_handle(), root)
            .await
            .map_err(|error| color_eyre::eyre::eyre!(error))?;
    assert_eq!(
        control.state,
        codex_protocol::protocol::ExecutionControlState::Running
    );
    app.chat_widget.handle_key_event(KeyCode::Esc.into());
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "draft survives confirmation"
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn idle_execution_commands_preserve_draft() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut server = start_config_write_test_app_server(&app).await?;
    let started = server.start_thread(&app.config).await?;
    let root = started.session.thread_id;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    while events.try_recv().is_ok() {}
    app.chat_widget.insert_str("keep my draft");
    app.request_execution_control(&mut server, root, /*pause*/ true)
        .await;
    app.request_execution_control(&mut server, root, /*pause*/ false)
        .await;
    let notices = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => {
                Some(lines_to_single_string(&cell.display_lines(100)))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 2);
    assert!(notices[0].contains("Nothing to pause."));
    assert!(notices[1].contains("Conversation isn’t paused."));
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "keep my draft"
    );
    assert!(!app.chat_widget.active_view_is(crate::workflows::CENTER_ID));
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn execution_snapshots_do_not_repeat_transition_notices() {
    use codex_app_server_protocol::ThreadExecutionControl;
    use codex_app_server_protocol::ThreadExecutionControlChangedNotification;
    use codex_protocol::protocol::ExecutionControlState;

    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let root = ThreadId::new();
    app.primary_thread_id = Some(root);
    app.active_thread_id = Some(root);
    app.workflows.lock().unwrap().owner = Some(root);
    while events.try_recv().is_ok() {}
    while ops.try_recv().is_ok() {}
    let selection = app.chat_widget.config_ref().tui_status_line.clone();
    let update = |state, can_pause| ThreadExecutionControlChangedNotification {
        thread_id: root.to_string(),
        execution_control: ThreadExecutionControl {
            root_thread_id: root.to_string(),
            state,
            can_pause,
        },
    };
    app.apply_execution_snapshot(update(
        ExecutionControlState::Running,
        /*can_pause*/ true,
    ));
    app.apply_execution_control(update(
        ExecutionControlState::Pausing,
        /*can_pause*/ false,
    ));
    app.apply_execution_control(update(
        ExecutionControlState::Paused,
        /*can_pause*/ false,
    ));
    app.apply_execution_control(update(
        ExecutionControlState::Paused,
        /*can_pause*/ false,
    ));
    app.apply_execution_snapshot(update(
        ExecutionControlState::Paused,
        /*can_pause*/ false,
    ));
    app.apply_execution_control(update(
        ExecutionControlState::Running,
        /*can_pause*/ true,
    ));
    app.apply_execution_control(update(
        ExecutionControlState::Running,
        /*can_pause*/ false,
    ));
    let notices = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => {
                Some(lines_to_single_string(&cell.display_lines(100)))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 2);
    assert!(notices[0].contains("Conversation paused."));
    assert!(notices[1].contains("Conversation resumed."));
    assert_eq!(app.chat_widget.config_ref().tui_status_line, selection);
    assert!(
        ops.try_recv().is_err(),
        "local execution notices never become model input"
    );
    app.workflows.lock().unwrap().execution_notice_state = None;
    app.apply_execution_snapshot(update(
        ExecutionControlState::Paused,
        /*can_pause*/ false,
    ));
    app.apply_execution_control(update(
        ExecutionControlState::Paused,
        /*can_pause*/ false,
    ));
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::InsertHistoryCell(_)))
    );
}
