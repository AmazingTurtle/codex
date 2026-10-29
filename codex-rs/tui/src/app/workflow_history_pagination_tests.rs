use super::*;
use crate::app::tests::make_test_app_with_channels;
use pretty_assertions::assert_eq;
use serde_json::json;

fn task_fact(thread: ThreadId, sequence: u64) -> ThreadTimelineEntry {
    ThreadTimelineEntry::Workflow {
        position:sequence,
        event:Box::new(serde_json::from_value(json!({
            "id":format!("fact-{sequence}"),"ownerThreadId":thread.to_string(),
            "runId":"run","executionId":"execution","workflowName":"audit",
            "completionMode":"display","sequence":sequence,"createdAt":1,
            "kind":{"type":"stepCompleted","stepId":format!("step-{sequence}"),
                "attemptId":format!("attempt-{sequence}"),"childThreadId":format!("child-{sequence}"),
                "ownedTurnIds":[format!("turn-{sequence}")],"finalTurnId":null,
                "label":format!("Task {sequence}"),"phase":null,"status":"succeeded",
                "result":{"type":"absent"},"error":null,"resolvedModel":null,
                "reasoningEffort":null,"activeDurationMs":220_000}
        })).expect("persisted workflow fact")),
    }
}

#[tokio::test]
async fn every_workflow_page_split_preserves_one_ordered_group_and_its_original_facts() {
    for split in 1..4 {
        let (mut app, _events, _ops) = make_test_app_with_channels().await;
        let thread = ThreadId::new();
        let cwd = app.config.cwd.clone();
        let entries = (1..=4)
            .map(|sequence| task_fact(thread, sequence))
            .collect::<Vec<_>>();
        let newer = app.workflow_timeline_cells(
            thread,
            entries[split..].to_vec(),
            &[],
            &cwd,
            entries.clone(),
            &[],
        );
        let older = app.workflow_timeline_cells(
            thread,
            entries[..split].to_vec(),
            &[],
            &cwd,
            entries.clone(),
            &[],
        );
        let boundary = older.len();
        app.transcript_cells = older.into_iter().chain(newer).collect();
        app.join_older_workflow_group(boundary, &entries);
        assert_eq!(app.transcript_cells.len(), 1, "page split {split}");
        let group = app.transcript_cells[0]
            .as_any()
            .downcast_ref::<history_cell::WorkflowLifecycleHistoryCell>()
            .expect("joined workflow facts");
        assert_eq!(
            group.event_ids().collect::<Vec<_>>(),
            vec!["fact-1", "fact-2", "fact-3", "fact-4"]
        );
        assert_eq!(
            group
                .display_lines(88)
                .iter()
                .map(Line::to_string)
                .collect::<Vec<_>>(),
            vec![
                "• Completed tasks in workflow audit",
                "  └ Task 1 after 3m 40s",
                "  └ Task 2 after 3m 40s",
                "  └ Task 3 after 3m 40s",
                "  └ Task 4 after 3m 40s",
            ]
        );
    }
}

#[tokio::test]
async fn an_ordinary_turn_between_pages_prevents_workflow_group_joining() {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let thread = ThreadId::new();
    let cwd = app.config.cwd.clone();
    let entries = vec![
        task_fact(thread, 1),
        ThreadTimelineEntry::TurnStarted {
            position: 2,
            turn_id: "ordinary".into(),
            started_at: None,
        },
        task_fact(thread, 3),
    ];
    app.transcript_cells = app.workflow_timeline_cells(
        thread,
        entries,
        &[],
        &cwd,
        vec![
            task_fact(thread, 1),
            ThreadTimelineEntry::TurnStarted {
                position: 2,
                turn_id: "ordinary".into(),
                started_at: None,
            },
            task_fact(thread, 3),
        ],
        &[],
    );
    assert_eq!(
        app.transcript_cells.len(),
        2,
        "an ordinary lifecycle boundary remains meaningful even before its first item"
    );
}

#[tokio::test]
async fn an_older_workflow_page_hydrates_the_live_group_without_duplicate_rows() {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let thread = ThreadId::new();
    let cwd = app.config.cwd.clone();
    app.chat_widget
        .handle_thread_session(crate::app::tests::test_thread_session(
            thread,
            cwd.to_path_buf(),
        ));
    let entries = vec![task_fact(thread, 1), task_fact(thread, 2)];
    let ThreadTimelineEntry::Workflow { event, .. } = &entries[1] else {
        unreachable!("workflow fixture");
    };
    app.chat_widget
        .on_workflow_lifecycle(event.as_ref().clone());
    let before = app
        .chat_widget
        .active_cell_transcript_lines(88)
        .expect("live group");
    assert_eq!(
        before.iter().map(Line::to_string).collect::<Vec<_>>(),
        vec!["• Completed task Task 2 in workflow audit after 3m 40s"]
    );
    app.transcript_cells = app.workflow_timeline_cells(
        thread,
        entries[..1].to_vec(),
        &[],
        &cwd,
        entries.clone(),
        &[],
    );
    app.join_older_workflow_group(/*boundary*/ 1, &entries);
    assert!(
        app.transcript_cells.is_empty(),
        "the older committed row is absorbed exactly once"
    );
    assert_eq!(
        app.chat_widget
            .active_cell_transcript_lines(88)
            .expect("hydrated live group")
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>(),
        vec![
            "• Completed tasks in workflow audit",
            "  └ Task 1 after 3m 40s",
            "  └ Task 2 after 3m 40s",
        ]
    );
    assert!(
        app.workflow_timeline_cells(thread, entries.clone(), &[], &cwd, entries, &[])
            .is_empty(),
        "overlapping pages do not redisplay stable lifecycle IDs"
    );
}

#[tokio::test]
async fn a_hidden_private_item_remains_a_group_boundary_inside_an_older_page() {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let thread = ThreadId::new();
    let cwd = app.config.cwd.clone();
    let item = |position, value| ThreadTimelineEntry::Item {
        position,
        turn_id: "voice-turn".into(),
        item: Box::new(serde_json::from_value(value).expect("native voice item")),
    };
    let entries = vec![
        item(
            0,
            json!({"type":"userMessage","id":"handoff","content":[{
                "type":"text","text":"<realtime_delegation><input>question</input></realtime_delegation>","textElements":[]
            }]}),
        ),
        task_fact(thread, 1),
        item(
            2,
            json!({"type":"agentMessage","id":"private","text":"PRIVATE_GROUP_BOUNDARY","phase":"commentary"}),
        ),
        task_fact(thread, 3),
    ];
    let cells =
        app.workflow_timeline_cells(thread, entries.clone(), &[], &cwd, entries.clone(), &[]);
    let groups = cells
        .iter()
        .filter_map(|cell| {
            cell.as_any()
                .downcast_ref::<history_cell::WorkflowLifecycleHistoryCell>()
        })
        .map(|group| group.event_ids().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    assert_eq!(groups, vec![vec!["fact-1"], vec!["fact-3"]]);
    assert!(
        cells
            .iter()
            .flat_map(|cell| cell.raw_lines())
            .all(|line| !line.to_string().contains("PRIVATE_GROUP_BOUNDARY"))
    );
    assert!(
        app.workflow_timeline_cells(thread, entries.clone(), &[], &cwd, entries, &[])
            .iter()
            .all(|cell| !cell
                .as_any()
                .is::<history_cell::WorkflowLifecycleHistoryCell>()),
        "overlapping warm pages retain the original lifecycle IDs without redisplay"
    );
}

#[tokio::test]
async fn older_projection_uses_cached_handoff_order_and_retained_delegation_facts() {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let thread = ThreadId::new();
    let cwd = app.config.cwd.clone();
    let items: Vec<ThreadItem> = serde_json::from_value(json!([
        {"type":"agentMessage","id":"typed","text":"Earlier typed commentary","phase":"commentary"},
        {"type":"userMessage","id":"voice","content":[{"type":"text","text":"<realtime_delegation><input>question</input></realtime_delegation>","textElements":[]}]},
        {"type":"agentMessage","id":"private","text":"Later voice-private commentary","phase":"commentary"},
    ])).unwrap();
    let known = items
        .into_iter()
        .enumerate()
        .map(|(position, item)| ThreadTimelineEntry::Item {
            position: position as u64,
            turn_id: "shared".into(),
            item: Box::new(item),
        })
        .collect::<Vec<_>>();
    let text = |cells: Vec<Arc<dyn HistoryCell>>| {
        cells
            .iter()
            .flat_map(|cell| cell.raw_lines())
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let before =
        app.workflow_timeline_cells(thread, known[..1].to_vec(), &[], &cwd, known.clone(), &[]);
    assert!(
        text(before).contains("Earlier typed commentary"),
        "a cached later marker must not hide an earlier typed item"
    );
    let after =
        app.workflow_timeline_cells(thread, known[2..].to_vec(), &[], &cwd, known.clone(), &[]);
    assert!(!text(after).contains("Later voice-private commentary"));
    let truncated = app.workflow_timeline_cells(
        thread,
        known[2..].to_vec(),
        &[],
        &cwd,
        known[2..].to_vec(),
        &["shared".into()],
    );
    assert!(
        !text(truncated).contains("Later voice-private commentary"),
        "retained delegation remains authoritative when the marker falls outside the loaded page"
    );
}
