//! Normal native notifications and a lifecycle refresh must update an already-open scope.
use super::*;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn empty_first_page_streams_real_item_events_and_terminal_refresh_without_reselecting()
-> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let root = ThreadId::new();
    let child = ThreadId::new();
    app.enqueue_primary_thread_session(
        test_thread_session(root, app.config.cwd.to_path_buf()),
        Vec::new(),
    )
    .await?;
    let target = TranscriptTarget {
        attachment: root,
        owner: root,
        run_id: "run".into(),
        execution_id: "execution".into(),
        target: WorkflowTranscriptTarget::Step {
            step_id: "review".into(),
        },
    };
    {
        let mut state = app.workflows.lock().unwrap();
        state.owner = Some(root);
        state.attached_thread = Some(root);
        state.center.detail_tab = crate::workflows::DetailTab::Transcript;
        state.child = Some(0);
        state.rows = vec![crate::workflows::WorkflowRow {
            id: "run".into(),
            execution_id: "execution".into(),
            owner: Some(root),
            status: "Running".into(),
            children: vec![crate::workflows::WorkflowChild {
                key: "step:review".into(),
                thread_id: Some(child),
                transcript_target: Some(target.target.clone()),
                ..Default::default()
            }],
            ..Default::default()
        }];
    }
    app.chat_widget.show_bottom_pane_view(Box::new(
        crate::workflows::WorkflowCenter::new_with_keymap(
            app.workflows.clone(),
            app.app_event_tx.clone(),
            app.keymap.list.clone(),
        ),
    ));
    let cwd = app.config.cwd.clone();
    let prompt = json!({"turnId":"owned","item":{"type":"userMessage","id":"amsg_task","clientId":null,"content":[{"type":"text","text":"Full native task prompt","textElements":[]}]},"startedAtMs":null,"completedAtMs":null});
    let result = json!({"turnId":"owned","item":{"type":"agentMessage","id":"answer","text":"{\"ok\":true}","phase":"final_answer"},"startedAtMs":1,"completedAtMs":2});
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let (stale_started, stale_received) = tokio::sync::oneshot::channel();
    let (release_stale, stale_gate) = tokio::sync::oneshot::channel();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = crate::resolve_remote_addr(&format!("ws://{}", listener.local_addr()?))?;
    let host = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut stale_started = Some(stale_started);
        let mut stale_gate = Some(stale_gate);
        super::super::disconnect::serve_reconnect_requests(tokio_tungstenite::accept_async(stream).await?,move |request| {
            assert_eq!(request.method,"workflow/transcript/list","native deltas must not introduce unrelated or per-delta RPCs");
            let params:WorkflowTranscriptListParams=serde_json::from_value(request.params.unwrap()).unwrap();
            assert_eq!((params.thread_id,params.workflow_id,params.execution_id,params.target),(root.to_string(),"run".into(),Some("execution".into()),WorkflowTranscriptTarget::Step {step_id:"review".into()}));
            let index=observed.fetch_add(1,Ordering::SeqCst);assert!(index<3);
            let reply=json!({"result":{"data":[{"runId":"run","executionId":"execution","stepId":"review","attemptId":"attempt","childThreadId":child,
                "label":"Reviewer","phase":null,"ownedTurnIds":["owned"],"resolvedModel":"actual","reasoningEffort":"high","cwd":cwd,
                "items":match index {0=>vec![],1=>vec![json!({"turnId":"owned","item":{"type":"agentMessage","id":"answer","text":"Live worker"},"startedAtMs":1,"completedAtMs":null})],_=>vec![prompt.clone(),result.clone()]},
                "finalTurnId":if index<2 {None} else {Some("owned")},"finalItemId":if index<2 {None} else {Some("answer")},
                "finalResult":if index<2 {serde_json::Value::Null} else {json!({"type":"value","value":{"ok":true}})} }],"nextCursor":null,"limitation":null}});
            let gate=if index==1 {stale_started.take().unwrap().send(()).unwrap();stale_gate.take()}else{None};
            async move {if let Some(gate)=gate {gate.await.unwrap();}Some(reply)}
        }).await
    });
    let mut server = AppServerSession::new(
        crate::backend_id::BackendId::new(),
        crate::connect_remote_app_server(endpoint).await?,
        crate::app_server_session::ThreadParamsMode::Remote,
    );
    app.read_workflow_transcript(&server, target.clone(), /*older*/ false);
    load(&mut app, &mut server, &mut events).await?;
    assert!(
        app.workflows.lock().unwrap().transcripts[&target.cache_key()].groups[0]
            .items
            .is_empty()
    );
    let message: ThreadItem =
        serde_json::from_value(json!({"type":"agentMessage","id":"answer","text":""})).unwrap();
    for notification in [
        ServerNotification::ItemStarted(codex_app_server_protocol::ItemStartedNotification {
            thread_id: child.to_string(),
            turn_id: "owned".into(),
            item: message,
            started_at_ms: 1,
        }),
        ServerNotification::AgentMessageDelta(
            codex_app_server_protocol::AgentMessageDeltaNotification {
                thread_id: child.to_string(),
                turn_id: "owned".into(),
                item_id: "answer".into(),
                delta: "Live worker answer".into(),
            },
        ),
    ] {
        app.handle_app_server_event(
            &server,
            codex_app_server_client::AppServerEvent::ServerNotification(Box::new(notification)),
        )
        .await;
    }
    let revision = app.workflows.lock().unwrap().transcripts[&target.cache_key()].content_revision;
    assert!(rendered(&app, &target).contains("Live worker answer"));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "streaming text updates the existing native item without rereading pages"
    );
    app.read_workflow_transcript(&server, target.clone(), /*older*/ false);
    tokio::time::timeout(Duration::from_secs(/*secs*/ 5), stale_received).await??;
    app.apply_workflow_chat_notification(&ServerNotification::ItemStarted(
        codex_app_server_protocol::ItemStartedNotification {
            thread_id: child.to_string(),
            turn_id: "owned".into(),
            item: serde_json::from_value(json!({"type":"agentMessage","id":"answer","text":""}))?,
            started_at_ms: 1,
        },
    ));
    assert!(
        rendered(&app, &target).contains("Live worker answer"),
        "a queued Started cannot erase the hydrated prefix"
    );
    app.apply_workflow_chat_notification(&ServerNotification::AgentMessageDelta(
        codex_app_server_protocol::AgentMessageDeltaNotification {
            thread_id: child.to_string(),
            turn_id: "owned".into(),
            item_id: "answer".into(),
            delta: " tail".into(),
        },
    ));
    assert!(rendered(&app, &target).contains("Live worker answer tail"));
    let tool:ThreadItem=serde_json::from_value(json!({"type":"commandExecution","id":"tool","command":"printf ACTUAL_TOOL","cwd":app.config.cwd,"processId":null,"status":"inProgress","commandActions":[],"aggregatedOutput":null,"exitCode":null,"durationMs":null})).unwrap();
    app.handle_app_server_event(
        &server,
        codex_app_server_client::AppServerEvent::ServerNotification(Box::new(
            ServerNotification::ItemStarted(codex_app_server_protocol::ItemStartedNotification {
                thread_id: child.to_string(),
                turn_id: "owned".into(),
                item: tool.clone(),
                started_at_ms: 1,
            }),
        )),
    )
    .await;
    app.handle_app_server_event(
        &server,
        codex_app_server_client::AppServerEvent::ServerNotification(Box::new(
            ServerNotification::CommandExecutionOutputDelta(
                codex_app_server_protocol::CommandExecutionOutputDeltaNotification {
                    thread_id: child.to_string(),
                    turn_id: "owned".into(),
                    item_id: "tool".into(),
                    delta: "ACTUAL_TOOL\n".into(),
                },
            ),
        )),
    )
    .await;
    assert!(rendered(&app, &target).contains("ACTUAL_TOOL"));
    assert!(
        matches!(&app.workflows.lock().unwrap().transcripts[&target.cache_key()].groups[0].items.iter().find(|item|item.entry.item.id()=="tool").unwrap().entry.item,
        ThreadItem::CommandExecution {aggregated_output:Some(output),..} if output=="ACTUAL_TOOL\n")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let mut completed_tool = tool;
    if let ThreadItem::CommandExecution {
        status,
        aggregated_output,
        exit_code,
        ..
    } = &mut completed_tool
    {
        *status = codex_app_server_protocol::CommandExecutionStatus::Completed;
        *aggregated_output = Some("ACTUAL_TOOL\n".into());
        *exit_code = Some(0);
    }
    app.handle_app_server_event(
        &server,
        codex_app_server_client::AppServerEvent::ServerNotification(Box::new(
            ServerNotification::ItemCompleted(
                codex_app_server_protocol::ItemCompletedNotification {
                    thread_id: child.to_string(),
                    turn_id: "owned".into(),
                    item: completed_tool,
                    completed_at_ms: 2,
                },
            ),
        )),
    )
    .await;
    // A later independent human turn on the same worker is not part of this scope.
    app.apply_workflow_chat_notification(&ServerNotification::AgentMessageDelta(
        codex_app_server_protocol::AgentMessageDeltaNotification {
            thread_id: child.to_string(),
            turn_id: "later-human".into(),
            item_id: "answer".into(),
            delta: "MUST_NOT_APPEAR".into(),
        },
    ));
    assert!(!rendered(&app, &target).contains("MUST_NOT_APPEAR"));
    app.apply_workflow_chat_notification(&ServerNotification::ItemCompleted(
        codex_app_server_protocol::ItemCompletedNotification {
            thread_id: child.to_string(),
            turn_id: "owned".into(),
            item: serde_json::from_value(
                json!({"type":"agentMessage","id":"answer","text":"{\"ok\":true}"}),
            )?,
            completed_at_ms: 2,
        },
    ));
    release_stale.send(()).unwrap();
    load(&mut app, &mut server, &mut events).await?;
    assert!(
        matches!(&app.workflows.lock().unwrap().transcripts[&target.cache_key()].groups[0].items.iter().find(|item|item.entry.item.id()=="answer").unwrap().entry,
        codex_app_server_protocol::ThreadItemEntry {item:ThreadItem::AgentMessage {text,..},completed_at_ms:Some(2),..} if text=="{\"ok\":true}"),
        "the held older RPC must not downgrade the newer native completion"
    );
    let workflow:codex_app_server_protocol::WorkflowRun=serde_json::from_value(json!({"id":"run","executionId":"execution","ownerThreadId":root,"formatVersion":2,"readOnly":false,
        "source":"source","input":{},"status":"succeeded","phase":null,"phases":[],"steps":{"review":{
            "id":"review","label":"Reviewer","phase":null,"options":{"message":"Full native task prompt"},"result":{"ok":true},
            "attempts":[{"id":"attempt","childThreadId":child,"ownedTurnIds":["owned"],"finalTurnId":"owned","resolvedModel":"actual","reasoningEffort":"high","status":"succeeded"}]
        }},"createdAt":1,"updatedAt":2})).unwrap();
    app.handle_app_server_event(
        &server,
        codex_app_server_client::AppServerEvent::ServerNotification(Box::new(
            ServerNotification::WorkflowUpdated(
                codex_app_server_protocol::WorkflowUpdatedNotification {
                    thread_id: root.to_string(),
                    workflow,
                },
            ),
        )),
    )
    .await;
    load(&mut app, &mut server, &mut events).await?;
    let final_chat = rendered(&app, &target);
    assert!(final_chat.contains("Full native task prompt"));
    assert!(
        final_chat.contains("• Response") && final_chat.contains("Outcome"),
        "{final_chat}"
    );
    assert!(!final_chat.contains("• •"), "{final_chat}");
    assert_eq!(
        app.workflows.lock().unwrap().transcripts[&target.cache_key()].groups[0].items[0]
            .entry
            .item
            .id(),
        "amsg_task",
        "a compatibility-projected task must precede its reply and tools"
    );
    assert!(
        app.workflows.lock().unwrap().transcripts[&target.cache_key()].content_revision > revision
    );
    assert_eq!(app.current_displayed_thread_id(), Some(root));
    insta::assert_snapshot!(
        "workflow_live_native_task_tool_and_final",
        crate::chatwidget::tests::helpers::normalize_snapshot_paths(rendered(&app, &target))
    );
    server.shutdown().await?;
    host.await??;
    Ok(())
}

async fn load(
    app: &mut App,
    server: &mut AppServerSession,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> Result<()> {
    let loaded = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(AppEvent::Workflow(event @ WorkflowEvent::TranscriptLoaded { .. })) =
                events.recv().await
            {
                break event;
            }
        }
    })
    .await?;
    app.handle_workflow_event(server, loaded).await;
    Ok(())
}

fn rendered(app: &App, target: &TranscriptTarget) -> String {
    app.workflows.lock().unwrap().transcripts[&target.cache_key()]
        .cells
        .iter()
        .flat_map(|cell| cell.display_lines(100))
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}
