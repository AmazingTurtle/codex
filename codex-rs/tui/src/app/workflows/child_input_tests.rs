//! Real App selection/submission consumes the backend-verified child capability.
//! The strict wire fixture substitutes the server; API acceptance owns membership validation.
use super::*;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::workflows::WorkflowEvent;
use codex_app_server_protocol::ThreadTimelineListParams;
use codex_app_server_protocol::ThreadTimelineListResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnSteerParams;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::net::TcpListener;

#[tokio::test]
async fn selected_workflow_child_submits_human_input_without_overriding_its_settings() -> Result<()>
{
    exercise_child_input(ChildCapability::Workflow).await
}

#[tokio::test]
async fn selected_ordinary_child_remains_readonly_and_preserves_blocked_input() -> Result<()> {
    exercise_child_input(ChildCapability::ReadOnly).await
}

#[tokio::test]
async fn selected_legacy_child_retains_existing_turn_settings_behavior() -> Result<()> {
    exercise_child_input(ChildCapability::Legacy).await
}

enum ChildCapability {
    ReadOnly,
    Workflow,
    Legacy,
}

async fn exercise_child_input(capability: ChildCapability) -> Result<()> {
    let (writable, structured_path) = match capability {
        ChildCapability::ReadOnly => (false, true),
        ChildCapability::Workflow => (true, true),
        ChildCapability::Legacy => (true, false),
    };
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let root = ThreadId::new();
    let child = ThreadId::new();
    let root_session = test_thread_session(root, app.config.cwd.to_path_buf());
    app.enqueue_primary_thread_session(root_session, Vec::new())
        .await?;
    let root_settings = (
        app.chat_widget.config_ref().permissions.clone(),
        app.chat_widget.current_model().to_owned(),
    );
    if writable && structured_path {
        app.chat_widget
            .restore_user_message_to_composer("Root draft before inspecting work".into());
    }
    let source: codex_app_server_protocol::SessionSource =
        codex_protocol::protocol::SessionSource::SubAgent(
            codex_protocol::protocol::SubAgentSource::ThreadSpawn {
                parent_thread_id: root,
                depth: 1,
                agent_path: if structured_path {
                    Some("/root/reviewer".parse().expect("valid fixture agent path"))
                } else {
                    None
                },
                agent_nickname: Some("Reviewer".into()),
                agent_role: Some("worker".into()),
            },
        )
        .into();
    let thread = json!({
        "id": child, "sessionId": root, "parentThreadId": root,
        "preview": "Review source", "ephemeral": false, "modelProvider": "test-provider",
        "model": "gpt-child", "createdAt": 1, "updatedAt": 2,
        "status": {"type":"idle"}, "cwd": app.config.cwd, "cliVersion":"0.0.0",
        "source": source, "canAcceptDirectInput": writable, "turns": []
    });
    let cwd = app.config.cwd.clone();
    let root_thread = json!({"id":root,"sessionId":root,"preview":"Main conversation","ephemeral":false,"modelProvider":"test-provider","createdAt":1,"updatedAt":2,"status":{"type":"idle"},"cwd":cwd,"cliVersion":"0.0.0","source":"cli","turns":[]});
    let _: codex_app_server_protocol::Thread = serde_json::from_value(thread.clone())?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = crate::resolve_remote_addr(&format!("ws://{}", listener.local_addr()?))?;
    app.app_server_target = AppServerTarget::Remote {
        endpoint: endpoint.clone(),
    };
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        super::disconnect::serve_reconnect_requests(tokio_tungstenite::accept_async(stream).await?, |request| {
                std::future::ready(Some(match request.method.as_str() {
                    "thread/read" => { let id=&request.params.as_ref().unwrap()["threadId"];assert!(id==&child.to_string() || id==&root.to_string());json!({"result":{"thread":if id==&root.to_string() {&root_thread} else {&thread}}}) }
                    "thread/resume" => json!({"result": {"thread":thread,"model":"gpt-child","modelProvider":"test-provider","cwd":cwd,"approvalPolicy":"on-request","approvalsReviewer":"user","sandbox":{"type":"readOnly"},"reasoningEffort":"low"}}),
                    "thread/goal/get" => json!({"result":{"goal":null}}),
                    "thread/timeline/list" => {
                        let params: ThreadTimelineListParams = serde_json::from_value(request.params.unwrap()).unwrap();
                        assert_eq!((params.thread_id, params.cursor), (child.to_string(), None));
                        assert!(params.limit.is_some_and(|limit| (1..=100).contains(&limit)), "selection must request a bounded timeline page");
                        json!({"result":ThreadTimelineListResponse {data:Vec::new(), next_cursor:None, active_realtime_session_at_page_start:None}})
                    }
                    "thread/loaded/list" | "thread/list" | "thread/turns/list" => json!({"result":{"data":[],"nextCursor":null}}),
                    "turn/start" => {
                        assert!(writable);
                        let params: TurnStartParams = serde_json::from_value(request.params.unwrap()).unwrap();
                        if structured_path { assert_eq!(params, TurnStartParams {
                            thread_id: child.to_string(),
                            client_user_message_id: params.client_user_message_id.clone(),
                            input: vec![UserInput::Text { text: "Follow this human instruction".into(), text_elements: Vec::new() }],
                            ..Default::default()
                        }); } else {
                            assert_eq!(params.cwd.as_deref(), Some(cwd.as_path()));
                            assert_eq!(params.model.as_deref(), Some("gpt-child"));
                        }
                        assert!(params.client_user_message_id.is_some());
                        json!({"result":{"turn":test_turn("human",TurnStatus::InProgress,Vec::new())}})
                    }
                    "turn/steer" => {
                        assert!(writable);
                        let params: TurnSteerParams = serde_json::from_value(request.params.unwrap()).unwrap();
                        assert_eq!((params.thread_id, params.expected_turn_id, params.additional_context, params.input),
                            (child.to_string(), "running".into(), None, vec![UserInput::Text { text: "Steer this active child".into(), text_elements: Vec::new() }]));
                        json!({"result":{"turnId":"running"}})
                    }
                    method => panic!("unexpected child-input request: {method}"),
                }))
            }).await
    });
    let client = crate::connect_remote_app_server(endpoint).await?;
    let mut session = AppServerSession::new(
        crate::backend_id::BackendId::new(),
        client,
        crate::app_server_session::ThreadParamsMode::Remote,
    );
    let mut tui = crate::tui::test_support::make_test_tui()?;
    if writable && structured_path {
        {
            let mut state = app.workflows.lock().unwrap();
            state.owner = Some(root);
            state.displayed_thread = Some(root);
            state.rows = vec![crate::workflows::WorkflowRow {
                id: "review-run".into(),
                owner: Some(root),
                title: "Review source".into(),
                children: vec![crate::workflows::WorkflowChild {
                    key: "step:review".into(),
                    thread_id: Some(child),
                    transcript_target: Some(
                        codex_app_server_protocol::WorkflowTranscriptTarget::Step {
                            step_id: "review".into(),
                        },
                    ),
                    ..Default::default()
                }],
                ..Default::default()
            }];
            state.child = Some(0);
        }
        let (center_tx, mut center_events) = tokio::sync::mpsc::unbounded_channel();
        let mut center = crate::workflows::WorkflowCenter::new(
            app.workflows.clone(),
            AppEventSender::new(center_tx),
        );
        center.handle_key_event(KeyCode::Enter.into());
        let event = center_events
            .try_recv()
            .expect("Enter selects the exact workflow worker");
        assert!(matches!(&event,AppEvent::Workflow(WorkflowEvent::SelectAgent(id)) if *id==child));
        assert!(center.is_complete());
        app.handle_event(&mut tui, &mut session, event).await?;
        let selection = std::iter::from_fn(|| events.try_recv().ok())
            .find(|event| matches!(event,AppEvent::SelectAgentThread(id) if *id==child))
            .expect("normal agent navigation event");
        app.handle_event(&mut tui, &mut session, selection).await?;
    } else {
        app.select_agent_thread(&mut tui, &mut session, child)
            .await?;
    }
    assert_eq!(app.current_displayed_thread_id(), Some(child));
    assert_eq!(
        session.preserves_child_turn_settings(child),
        writable && structured_path
    );
    // The original test widget uses an op channel; selected real App widgets
    // route through AppEvent instead. Root attachment only refreshes skills.
    let attachment_ops = std::iter::from_fn(|| ops.try_recv().ok()).collect::<Vec<_>>();
    assert!(
        attachment_ops
            .iter()
            .all(|op| matches!(op, Op::ListSkills { .. })),
        "unexpected root attachment operations: {attachment_ops:?}"
    );
    let child_settings = (
        app.chat_widget.config_ref().permissions.clone(),
        app.chat_widget.current_model().to_string(),
    );
    while events.try_recv().is_ok() {}
    if !writable {
        let empty_composer = render_bottom_popup(&app.chat_widget, 100);
        assert!(
            empty_composer.contains("direct input is disabled"),
            "{empty_composer}"
        );
    }
    app.chat_widget
        .restore_user_message_to_composer("Follow this human instruction".into());
    app.chat_widget.handle_key_event(KeyCode::Enter.into());
    if writable {
        let op = submitted_child_turn(&mut events);
        assert!(
            app.try_submit_active_thread_op_via_app_server(&mut session, child, &op)
                .await?
        );
        app.enqueue_thread_notification(
            child,
            ServerNotification::TurnStarted(codex_app_server_protocol::TurnStartedNotification {
                thread_id: child.to_string(),
                turn: test_turn("running", TurnStatus::InProgress, Vec::new()),
            }),
        )
        .await?;
        app.drain_active_thread_events(&mut tui).await?;
        app.chat_widget
            .restore_user_message_to_composer("Steer this active child".into());
        app.chat_widget.handle_key_event(KeyCode::Enter.into());
        let op = submitted_child_turn(&mut events);
        assert!(
            app.try_submit_active_thread_op_via_app_server(&mut session, child, &op)
                .await?
        );
        assert_eq!(
            (
                app.chat_widget.config_ref().permissions.clone(),
                app.chat_widget.current_model().to_string()
            ),
            child_settings
        );
        for expected in ["pause", "workflows"] {
            app.chat_widget
                .restore_user_message_to_composer(format!("/{expected}").into());
            app.chat_widget.handle_key_event(KeyCode::Enter.into());
            assert!(
                std::iter::from_fn(|| events.try_recv().ok()).any(|event| match event {
                    AppEvent::Workflow(crate::workflows::WorkflowEvent::Pause) =>
                        expected == "pause",
                    AppEvent::Workflow(crate::workflows::WorkflowEvent::Open) =>
                        expected == "workflows",
                    _ => false,
                })
            );
        }
        if structured_path {
            app.chat_widget
                .restore_user_message_to_composer("Keep this worker draft".into());
            {
                let mut state = app.workflows.lock().unwrap();
                state.owner = Some(root);
                state.displayed_thread = Some(child);
                state.child = None;
            }
            let (tx, mut center_events) = tokio::sync::mpsc::unbounded_channel();
            let mut center = crate::workflows::WorkflowCenter::new(
                app.workflows.clone(),
                AppEventSender::new(tx),
            );
            center.handle_key_event(KeyCode::Enter.into());
            let event = center_events.try_recv().expect("workflow root opens Main");
            assert!(
                matches!(&event,AppEvent::Workflow(WorkflowEvent::SelectAgent(id)) if *id==root)
            );
            app.handle_event(&mut tui, &mut session, event).await?;
            let selection = std::iter::from_fn(|| events.try_recv().ok())
                .find(|event| matches!(event,AppEvent::SelectAgentThread(id) if *id==root))
                .expect("Main uses normal thread selection");
            app.handle_event(&mut tui, &mut session, selection).await?;
            assert_eq!(app.current_displayed_thread_id(), Some(root));
            assert_eq!(
                (
                    app.chat_widget.config_ref().permissions.clone(),
                    app.chat_widget.current_model().to_owned()
                ),
                root_settings
            );
            assert_eq!(
                app.chat_widget.composer_text_with_pending(),
                "Root draft before inspecting work"
            );
            assert!(center.is_complete());
        }
    } else {
        assert!(ops.try_recv().is_err());
        let blocked_events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            !blocked_events
                .iter()
                .any(|event| matches!(event, AppEvent::CodexOp(Op::UserTurn { .. })))
        );
        assert_eq!(
            app.chat_widget.composer_text_with_pending(),
            "Follow this human instruction"
        );
        let messages = blocked_events
            .into_iter()
            .filter_map(|event| match event {
                AppEvent::InsertHistoryCell(cell) => {
                    Some(lines_to_single_string(&cell.display_lines(100)))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            vec!["■ This sub-agent is controlled by its parent. Direct input is disabled."]
        );
    }
    session.shutdown().await?;
    let methods = server.await??;
    assert!(
        methods
            .iter()
            .any(|method| method == "thread/timeline/list"),
        "selecting the child must hydrate its canonical timeline before testing direct input"
    );
    Ok(())
}

fn submitted_child_turn(events: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>) -> Op {
    std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::CodexOp(op @ Op::UserTurn { .. }) => Some(op),
            _ => None,
        })
        .expect("selected child submits through the App event route")
}
