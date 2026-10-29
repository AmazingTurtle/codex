//! App action dispatch preserves draft text and the caller/revision captured by the UI.

use super::*;
use crate::workflows::WorkflowEvent;
use crate::workflows::WorkflowRow;
use crate::workflows::actions::*;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::persisted_workflow_fixture;

async fn open_actions_and_wait(
    app: &mut App,
    server: &mut AppServerSession,
    events: &mut mpsc::UnboundedReceiver<AppEvent>,
    root: ThreadId,
    run_id: &str,
    execution_id: &str,
) -> Result<()> {
    app.handle_workflow_event(
        server,
        WorkflowEvent::ActionsOpen {
            owner: root,
            caller: root,
            run_id: run_id.into(),
            execution_id: execution_id.into(),
            target: ActionTarget::Run,
        },
    )
    .await;
    let context_id = app.workflows.lock().unwrap().action_context_id;
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            if let AppEvent::Workflow(event) = events.recv().await.expect("workflow Actions response") {
                let loaded = matches!(&event,WorkflowEvent::ControlResult {scope,..} if Some(scope.context_id) == context_id);
                app.handle_workflow_event(server,event).await;
                if loaded {break;}
            }
        }
    }).await?;
    assert!(
        app.chat_widget
            .active_view_is(crate::workflows::actions::ACTIONS_VIEW_ID),
        "the matching Actions read must open its picker: {:?}",
        app.workflows.lock().unwrap().error
    );
    Ok(())
}

fn action_snapshot(root: ThreadId) -> ActionSnapshot {
    ActionSnapshot {
        scope: ActionScope {
            context_id: Uuid::new_v4(),
            owner: root,
            caller: root,
            workflow_id: "review-run".into(),
            execution_id: "execution-1".into(),
            expected_control_revision: 7,
            target: ActionTarget::Run,
        },
        title: "Review change".into(),
        availability: vec![ActionAvailability {
            action: Action::Revise,
            unavailable_reason: None,
        }],
        queued_prompt: None,
        source: "workflow.meta({name:'review',description:'Review'});\nreturn 0;".into(),
        input: json!({"original":true}),
        staged_revision_id: None,
        active_revision_id: Some("revision-1".into()),
        budget_summary: "1 / 8 agent starts".into(),
        footer_hidden: false,
    }
}

fn next_intent(events: &mut mpsc::UnboundedReceiver<AppEvent>) -> WorkflowUiIntent {
    std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::Workflow(WorkflowEvent::ControlIntent(intent)) => Some(*intent),
            _ => None,
        })
        .expect("the submitted form emits a workflow intent")
}

async fn attach(
    app: &mut App,
    server: &mut AppServerSession,
    tui: &mut tui::Tui,
) -> Result<ThreadId> {
    let started = server.start_thread(&app.config).await?;
    let root = started.session.thread_id;
    app.replace_chat_widget_with_app_server_thread(
        tui,
        started,
        super::super::session_lifecycle::ThreadAttachPresentation::Fresh,
        /*initial_user_message*/ None,
    )
    .await?;
    app.sync_workflow_presentation();
    Ok(root)
}

fn seed_action_view(app: &mut App, snapshot: ActionSnapshot) {
    let mut state = app.workflows.lock().unwrap();
    state.rows = vec![WorkflowRow {
        owner: Some(snapshot.scope.owner),
        id: snapshot.scope.workflow_id.clone(),
        execution_id: snapshot.scope.execution_id.clone(),
        ..Default::default()
    }];
    state.action_context_id = Some(snapshot.scope.context_id);
    state.action_snapshot = Some(Box::new(snapshot));
    state.action_pending = true;
}

#[tokio::test]
async fn rejected_revision_keeps_source_arguments_and_guard_then_rejects_a_stale_caller()
-> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut server = start_config_write_test_app_server(&app).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let root = attach(&mut app, &mut server, &mut tui).await?;
    let snapshot = action_snapshot(root);
    let scope = snapshot.scope.clone();
    seed_action_view(&mut app, snapshot);
    // A later observation may be cached while the rejected command's old guard remains pinned.
    app.workflows
        .lock()
        .unwrap()
        .action_snapshot
        .as_mut()
        .unwrap()
        .scope
        .expected_control_revision = 8;
    while events.try_recv().is_ok() {}
    let source =
        "workflow.meta({name:'review',description:'Updated review'});\nreturn workflow.args;"
            .to_string();
    let input = json!({"files":["changed.rs"],"keep":"submitted arguments"});
    app.handle_workflow_event(
        &mut server,
        WorkflowEvent::ControlResult {
            scope: scope.clone(),
            outcome: Box::new(WorkflowUiOutcome::Mutation {
                intent: Box::new(WorkflowUiIntent::StageRevision {
                    scope: scope.clone(),
                    source: source.clone(),
                    input: input.clone(),
                }),
                result: Err("Control revision changed. Reopen Actions before submitting.".into()),
            }),
        },
    )
    .await;
    assert_eq!(
        app.workflows
            .lock()
            .unwrap()
            .action_snapshot
            .as_ref()
            .unwrap()
            .scope
            .expected_control_revision,
        8
    );
    assert!(!app.workflows.lock().unwrap().action_pending);
    insta::assert_snapshot!(
        "workflow_revision_backend_error",
        render_bottom_popup(&app.chat_widget, 100)
    );
    app.handle_tui_event(
        &mut tui,
        &mut server,
        TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    )
    .await?;
    let intent = next_intent(&mut events);
    assert_eq!(
        intent,
        WorkflowUiIntent::RevisionArguments {
            scope: scope.clone(),
            source: source.clone(),
            initial_input: input.clone()
        }
    );
    app.handle_workflow_event(&mut server, WorkflowEvent::ControlIntent(Box::new(intent)))
        .await;
    app.handle_tui_event(
        &mut tui,
        &mut server,
        TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    )
    .await?;
    let submitted = next_intent(&mut events);
    assert_eq!(
        submitted,
        WorkflowUiIntent::StageRevision {
            scope: scope.clone(),
            source,
            input
        }
    );
    let other = attach(&mut app, &mut server, &mut tui).await?;
    assert_ne!(root, other);
    app.handle_workflow_event(
        &mut server,
        WorkflowEvent::ControlIntent(Box::new(submitted)),
    )
    .await;
    {
        let state = app.workflows.lock().unwrap();
        assert_eq!(
            (state.action_context_id, state.action_pending),
            (None, false),
            "a stale form cannot enqueue a mutation under the newly attached caller"
        );
    }
    app.handle_workflow_event(
        &mut server,
        WorkflowEvent::ControlResult {
            scope,
            outcome: Box::new(WorkflowUiOutcome::Actions(Err(
                "late old-conversation response".into(),
            ))),
        },
    )
    .await;
    assert!(
        app.workflows.lock().unwrap().error.is_none(),
        "late old attachment responses must not overwrite the new conversation"
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn staged_revision_apply_uses_only_the_successful_acknowledgements_guard() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut server = start_config_write_test_app_server(&app).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let root = attach(&mut app, &mut server, &mut tui).await?;
    let snapshot = action_snapshot(root);
    let scope = snapshot.scope.clone();
    let revision = RevisionPreview {
        id: "revision-2".into(),
        parent_id: Some("revision-1".into()),
        number: 2,
        execution_ids: Vec::new(),
        status: "Draft".into(),
        source: snapshot.source.clone(),
        input: snapshot.input.clone(),
        reusable_statements: 1,
        statement_count: 2,
        invalidation_reason: "The final statement changed".into(),
        error: None,
    };
    seed_action_view(&mut app, snapshot);
    while events.try_recv().is_ok() {}
    app.handle_workflow_event(
        &mut server,
        WorkflowEvent::ControlResult {
            scope: scope.clone(),
            outcome: Box::new(WorkflowUiOutcome::Mutation {
                intent: Box::new(WorkflowUiIntent::StageRevision {
                    scope: scope.clone(),
                    source: revision.source.clone(),
                    input: revision.input.clone(),
                }),
                result: Ok(MutationReceipt {
                    revision: Some(revision),
                    state: Some(codex_app_server_protocol::WorkflowControlState {
                        control_revision: 8,
                        active_revision_id: "revision-1".into(),
                        pending_revision_id: None,
                        status: codex_app_server_protocol::WorkflowRevisionStatus::Active,
                    }),
                }),
            }),
        },
    )
    .await;
    let stored = app
        .workflows
        .lock()
        .unwrap()
        .action_snapshot
        .clone()
        .unwrap();
    assert_eq!(
        (
            stored.scope.expected_control_revision,
            stored.staged_revision_id.as_deref()
        ),
        (8, Some("revision-2"))
    );
    app.handle_tui_event(
        &mut tui,
        &mut server,
        TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    )
    .await?;
    let mut acknowledged = scope;
    acknowledged.expected_control_revision = 8;
    assert_eq!(
        next_intent(&mut events),
        WorkflowUiIntent::ApplyRevision {
            scope: acknowledged,
            revision_id: "revision-2".into(),
            mode: RevisionApplyMode::DrainAccepted
        }
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn footer_visibility_keeps_a_run_inspectable_and_restorable_without_restarting() -> Result<()>
{
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let mut server = start_config_write_test_app_server(&app).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let root = attach(&mut app, &mut server, &mut tui).await?;
    // A persisted external run belongs to a materialized native thread. An
    // untouched start has no durable metadata for the cold ancestry read yet.
    server
        .thread_set_name(root, "Footer visibility".into())
        .await?;
    let run =
        persisted_workflow_fixture::create(&app.config.codex_home, root, app.config.cwd.as_path())?;
    app.discover_workflows_after_attach(&server);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let AppEvent::Workflow(event) =
                events.recv().await.expect("persisted workflow discovery")
            {
                app.handle_workflow_event(&mut server, event).await;
                if app
                    .workflows
                    .lock()
                    .unwrap()
                    .rows
                    .iter()
                    .any(|row| row.id == run.id)
                {
                    break;
                }
            }
        }
    })
    .await?;
    app.chat_widget.show_bottom_pane_view(Box::new(
        crate::workflows::WorkflowCenter::new_with_keymap(
            app.workflows.clone(),
            app.app_event_tx.clone(),
            app.keymap.list.clone(),
        ),
    ));
    open_actions_and_wait(
        &mut app,
        &mut server,
        &mut events,
        root,
        &run.id,
        &run.execution_id,
    )
    .await?;
    // The local footer action is the last row of the scrollable native picker.
    app.handle_tui_event(
        &mut tui,
        &mut server,
        TuiEvent::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
    )
    .await?;
    assert!(render_bottom_popup(&app.chat_widget, 100).contains("Hide from footer"));
    app.handle_tui_event(
        &mut tui,
        &mut server,
        TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    )
    .await?;
    let hide = next_intent(&mut events);
    assert!(matches!(
        &hide,
        WorkflowUiIntent::SetFooterVisibility { visible: false, .. }
    ));
    app.handle_workflow_event(&mut server, WorkflowEvent::ControlIntent(Box::new(hide)))
        .await;
    {
        let state = app.workflows.lock().unwrap();
        assert_eq!(
            state
                .rows
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec![run.id.as_str()]
        );
        assert!(
            state.current_indices().is_empty(),
            "only the footer projection is hidden"
        );
        assert!(
            !state.action_pending,
            "a local visibility change cannot enqueue an RPC"
        );
    }
    app.handle_tui_event(
        &mut tui,
        &mut server,
        TuiEvent::Key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE)),
    )
    .await?;
    let source = render_bottom_popup(&app.chat_widget, 100);
    assert!(
        source.contains("workflow.meta") && source.contains("answer: 42"),
        "hidden workflows remain inspectable in the command center: {source}"
    );
    insta::assert_snapshot!("workflow_footer_hidden_source", source);
    // A fresh real Actions read must retain the local hidden flag rather than
    // resetting it to the backend context builder's default.
    open_actions_and_wait(
        &mut app,
        &mut server,
        &mut events,
        root,
        &run.id,
        &run.execution_id,
    )
    .await?;
    app.handle_tui_event(
        &mut tui,
        &mut server,
        TuiEvent::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
    )
    .await?;
    let menu = render_bottom_popup(&app.chat_widget, 100);
    assert!(menu.contains("Show in footer"));
    insta::assert_snapshot!("workflow_footer_restore_action", menu);
    app.handle_tui_event(
        &mut tui,
        &mut server,
        TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    )
    .await?;
    let show = next_intent(&mut events);
    assert!(matches!(
        &show,
        WorkflowUiIntent::SetFooterVisibility { visible: true, .. }
    ));
    let expected_revision = app
        .workflows
        .lock()
        .unwrap()
        .action_snapshot
        .as_ref()
        .unwrap()
        .scope
        .expected_control_revision;
    app.handle_workflow_event(&mut server, WorkflowEvent::ControlIntent(Box::new(show)))
        .await;
    {
        let state = app.workflows.lock().unwrap();
        assert_eq!(state.current_indices(), vec![0]);
        assert!(state.hidden_from_strip.is_empty());
        assert_eq!(
            state
                .action_snapshot
                .as_ref()
                .unwrap()
                .scope
                .expected_control_revision,
            expected_revision
        );
        assert!(!state.action_pending);
    }
    server.shutdown().await?;
    Ok(())
}
