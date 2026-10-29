//! The strict external backend substitutes only the wire; normal history-cell merging stays real.
use super::*;
#[path = "transcript_projection_tests.rs"]
mod projection;
use crate::workflows::TranscriptTarget;
use crate::workflows::WorkflowEvent;
use codex_app_server_protocol::WorkflowTranscriptListParams;
use codex_app_server_protocol::WorkflowTranscriptTarget;
use pretty_assertions::assert_eq;
use serde_json::json;
#[path = "transcript_live_tests.rs"]
mod live;

#[tokio::test]
async fn workflow_center_reads_bounded_real_transcript_pages_without_switching_chat() -> Result<()>
{
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let root = ThreadId::new();
    let run_owner = ThreadId::new();
    let worker = ThreadId::new();
    app.enqueue_primary_thread_session(
        test_thread_session(root, app.config.cwd.to_path_buf()),
        Vec::new(),
    )
    .await?;
    app.workflows.lock().unwrap().owner = Some(root);
    let displayed = app.current_displayed_thread_id();
    let target = TranscriptTarget {
        attachment: root,
        owner: run_owner,
        run_id: "run".into(),
        execution_id: "execution".into(),
        target: WorkflowTranscriptTarget::Phase {
            phase: "Review".into(),
        },
    };
    let expected = target.clone();
    let cwd = app.config.cwd.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = crate::resolve_remote_addr(&format!("ws://{}", listener.local_addr()?))?;
    let host = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut page = 0;
        super::disconnect::serve_reconnect_requests(tokio_tungstenite::accept_async(stream).await?, move |request| {
            assert_eq!(request.method, "workflow/transcript/list", "scope inspection must never resume or read the entire worker thread");
            let params: WorkflowTranscriptListParams = serde_json::from_value(request.params.unwrap()).unwrap();
            assert_eq!((params.thread_id, params.workflow_id, params.execution_id, params.target, params.limit),
                (run_owner.to_string(), expected.run_id.clone(), Some(expected.execution_id.clone()), expected.target.clone(), Some(100)));
            assert_eq!(params.cursor, (page == 1).then(|| "older".into()));
            let (items, next) = match page {
                0 => (vec![("item2", "Recent answer")], Some("older")),
                1 => (vec![("item1", "Earlier answer")], None),
                2 => (vec![("item2", "Recent answer"), ("item3", r#"{"status":"accepted","count":2}"#)], Some("older")),
                _ => panic!("unexpected transcript page"),
            };
            page += 1;
            std::future::ready(Some(json!({"result": {
                "data":[{"runId":"run","executionId":"execution","stepId":"review","attemptId":"attempt","childThreadId":worker,
                    "label":"Reviewer","phase":"Review","ownedTurnIds":["owned"],"resolvedModel":"actual-model","reasoningEffort":"high","cwd":cwd,
                    "finalTurnId":if page == 3 {Some("owned")} else {None},
                    "finalItemId":if page == 3 {Some("item3")} else {None},
                    "finalResult":if page == 3 {json!({"type":"value","value":{"status":"accepted","count":2}})} else {serde_json::Value::Null},
                    "items":items.into_iter().map(|(id,text)| json!({"turnId":"owned","item":{"type":"agentMessage","id":id,"text":text},"startedAtMs":1,"completedAtMs":2})).collect::<Vec<_>>() }],
                "nextCursor":next,"limitation":null,
            }})))
        }).await
    });
    let mut server = AppServerSession::new(
        crate::backend_id::BackendId::new(),
        crate::connect_remote_app_server(endpoint).await?,
        crate::app_server_session::ThreadParamsMode::Remote,
    );
    let mut retained = None;
    for (index, older) in [false, true, false].into_iter().enumerate() {
        app.read_workflow_transcript(&server, target.clone(), older);
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
        app.handle_workflow_event(&mut server, loaded).await;
        let state = app.workflows.lock().unwrap();
        let transcript = &state.transcripts[&target.cache_key()];
        assert_eq!(
            (transcript.loading, transcript.error.as_deref()),
            (false, None)
        );
        let ids = transcript.groups[0]
            .items
            .iter()
            .map(|item| item.entry.item.id())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            match index {
                0 => vec!["item2"],
                1 => vec!["item1", "item2"],
                _ => vec!["item1", "item2", "item3"],
            }
        );
        let rendered = transcript
            .cells
            .iter()
            .flat_map(|cell| cell.display_lines(100))
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Reviewer · actual-model · high"));
        assert_eq!(rendered.matches("Recent answer").count(), 1);
        if index == 1 {
            retained = Some(Arc::clone(&transcript.groups[0].projected[1]));
        }
        if index == 2 {
            assert!(
                Arc::ptr_eq(
                    retained.as_ref().unwrap(),
                    &transcript.groups[0].projected[1]
                ),
                "unchanged normal cells must retain viewport anchors"
            );
            let final_cell = &transcript.groups[0].projected[2];
            assert_eq!(
                final_cell.copy_source(),
                Some(r#"{"status":"accepted","count":2}"#)
            );
            assert!(
                !final_cell
                    .display_lines(100)
                    .iter()
                    .any(|line| line.to_string().contains(r#"{"status"#)),
                "the exact known final result uses the shared structured renderer"
            );
            assert!(
                rendered.contains("Outcome"),
                "the accepted result remains separate from the real final conversation item"
            );
            assert_eq!(transcript.groups[0].items[2].entry.item.id(), "item3");
        }
        assert_eq!(transcript.next_cursor.is_some(), index != 1);
        assert_eq!(app.current_displayed_thread_id(), displayed);
        assert_eq!(
            state.owner,
            Some(root),
            "cold project inspection must retain the displayed conversation owner"
        );
    }
    server.shutdown().await?;
    let methods = host.await??;
    assert_eq!(
        methods
            .iter()
            .filter(|method| method.as_str() == "workflow/transcript/list")
            .count(),
        3
    );
    Ok(())
}
