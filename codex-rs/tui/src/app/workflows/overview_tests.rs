//! A strict external server proves selected accounting refreshes through the real App RPC route.
use super::*;
use crate::workflows::DetailTab;
use crate::workflows::WorkflowCenter;
use crate::workflows::WorkflowEvent;
use codex_app_server_protocol::WorkflowProgressNotification;
use codex_app_server_protocol::WorkflowReadParams;
use codex_app_server_protocol::WorkflowRun;
use codex_app_server_protocol::WorkflowSummary;
use pretty_assertions::assert_eq;
use serde_json::json;

fn run(root: ThreadId, input: u64) -> WorkflowRun {
    serde_json::from_value(json!({
        "id":"selected-run","executionId":"execution-1","ownerThreadId":root.to_string(),"formatVersion":2,"readOnly":false,
        "source":"return workflow.args;","input":{},"status":"running","phase":null,"phases":[],"createdAt":1,"updatedAt":2,
        "steps":{"step":{"id":"step","logicalId":"audit","label":"Audit","options":{},"attempts":[{"id":"attempt","status":"running","admission":"accepted","childThreadId":ThreadId::from_u128(42).to_string()}],
            "metrics":{"usage":{"inputTokens":input,"outputTokens":10,"cachedInputTokens":0,"totalTokens":input+10},"usageComplete":false,"modelTurns":3,"agentsUsed":1}}}
    })).unwrap()
}

#[tokio::test]
async fn selected_overview_coalesces_progress_and_rejects_a_read_superseded_by_terminal_history()
-> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let root = ThreadId::new();
    app.enqueue_primary_thread_session(
        test_thread_session(root, app.config.cwd.to_path_buf()),
        Vec::new(),
    )
    .await?;
    while events.try_recv().is_ok() {}
    let initial = run(root, 0);
    {
        let mut state = app.workflows.lock().unwrap();
        state.owner = Some(root);
        state.displayed_thread = Some(root);
        state.rows = vec![crate::workflows::project(initial.clone())];
        state.child = Some(0);
    }
    let mut value = serde_json::to_value(&initial)?;
    value["stepCount"] = json!(1);
    value["phaseCount"] = json!(0);
    let summary: WorkflowSummary = serde_json::from_value(value)?;
    let progress = WorkflowProgressNotification {
        thread_id: root.to_string(),
        workflow: summary,
    };
    let permits = Arc::new(tokio::sync::Semaphore::new(0));
    let server_permits = permits.clone();
    let (calls_tx, mut calls) = mpsc::unbounded_channel();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = crate::resolve_remote_addr(&format!("ws://{}", listener.local_addr()?))?;
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_count = count.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        super::disconnect::serve_reconnect_requests(
            tokio_tungstenite::accept_async(stream).await?,
            move |request| {
                let permits = server_permits.clone();
                let calls = calls_tx.clone();
                let count = server_count.clone();
                async move {
                    assert_eq!(request.method, "workflow/read");
                    let params: WorkflowReadParams =
                        serde_json::from_value(request.params.unwrap()).unwrap();
                    assert_eq!(
                        params,
                        WorkflowReadParams {
                            thread_id: root.to_string(),
                            workflow_id: "selected-run".into()
                        }
                    );
                    let index = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    calls.send(index).unwrap();
                    permits.acquire().await.unwrap().forget();
                    Some(json!({"result":{"workflow":run(root,index as u64*100)}}))
                }
            },
        )
        .await
    });
    let client = crate::connect_remote_app_server(endpoint).await?;
    let mut session = AppServerSession::new(
        crate::backend_id::BackendId::new(),
        client,
        crate::app_server_session::ThreadParamsMode::Remote,
    );
    // Hidden centers, Source, and Saved do not start accounting reads.
    app.refresh_workflow_overview_after_progress(&session, &progress);
    assert!(app.workflows.lock().unwrap().overview_request.is_none());
    app.chat_widget
        .show_bottom_pane_view(Box::new(WorkflowCenter::new(
            app.workflows.clone(),
            app.app_event_tx.clone(),
        )));
    for (saved, tab) in [(true, DetailTab::Overview), (false, DetailTab::Source)] {
        {
            let mut state = app.workflows.lock().unwrap();
            state.center.saved = saved;
            state.center.detail_tab = tab;
        }
        app.refresh_workflow_overview_after_progress(&session, &progress);
        assert!(app.workflows.lock().unwrap().overview_request.is_none());
    }
    {
        let mut state = app.workflows.lock().unwrap();
        state.center.saved = false;
        state.center.detail_tab = DetailTab::Overview;
    }
    app.refresh_workflow_overview_after_progress(&session, &progress);
    app.refresh_workflow_overview_after_progress(&session, &progress);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), calls.recv()).await?,
        Some(1)
    );
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    permits.add_permits(1);
    let first = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let event @ AppEvent::Workflow(WorkflowEvent::OverviewLoaded { .. }) =
                events.recv().await.unwrap()
            {
                break event;
            }
        }
    })
    .await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.handle_event(&mut tui, &mut session, first).await?;
    assert_eq!(
        app.workflows.lock().unwrap().rows[0].children[0]
            .overview
            .as_ref()
            .unwrap()
            .metrics
            .as_ref()
            .unwrap()
            .usage
            .as_ref()
            .unwrap()
            .input_tokens,
        100
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), calls.recv()).await?,
        Some(2)
    );
    let mut terminal = run(root, 999);
    terminal.status = codex_app_server_protocol::WorkflowStatus::Succeeded;
    app.workflows
        .lock()
        .unwrap()
        .apply_update(crate::workflows::project(terminal));
    permits.add_permits(1);
    let second = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let event @ AppEvent::Workflow(WorkflowEvent::OverviewLoaded { .. }) =
                events.recv().await.unwrap()
            {
                break event;
            }
        }
    })
    .await?;
    app.handle_event(&mut tui, &mut session, second).await?;
    {
        let state = app.workflows.lock().unwrap();
        assert_eq!(
            (
                state.child,
                state.rows[0].status.as_str(),
                state.rows[0].source.as_str()
            ),
            (Some(0), "Completed", "return workflow.args;")
        );
        assert_eq!(
            state.rows[0].children[0]
                .overview
                .as_ref()
                .unwrap()
                .metrics
                .as_ref()
                .unwrap()
                .usage
                .as_ref()
                .unwrap()
                .input_tokens,
            999
        );
        assert!(state.overview_request.is_none());
    }
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);
    app.chat_widget.show_bottom_pane_view(Box::new(
        crate::workflows::actions::InspectionView::revision(
            crate::workflows::actions::RevisionPreview {
                id: "historical".into(),
                parent_id: None,
                number: 1,
                execution_ids: vec!["historical-execution".into()],
                status: "Superseded".into(),
                source: "return 'historical source';".into(),
                input: json!({}),
                reusable_statements: 0,
                statement_count: 1,
                invalidation_reason: "Historical plan".into(),
                error: None,
            },
        ),
    ));
    app.workflows.lock().unwrap().rows[0].status = "Running".into();
    app.refresh_workflow_overview_after_progress(&session, &progress);
    assert!(
        app.workflows.lock().unwrap().overview_request.is_none(),
        "immutable inspection cannot refresh the live center behind it"
    );
    assert!(!app.chat_widget.active_view_is(crate::workflows::CENTER_ID));
    let area = ratatui::layout::Rect::new(0, 0, 120, 40);
    let mut buffer = ratatui::buffer::Buffer::empty(area);
    crate::render::renderable::Renderable::render(
        &app.chat_widget.as_renderable(),
        area,
        &mut buffer,
    );
    let row = |y| {
        (0..area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
    };
    assert!(
        row(0).contains("Revision 1") && row(39).contains("Esc"),
        "inspection retains the full-height shell"
    );
    session.shutdown().await?;
    server.await??;
    Ok(())
}
