use super::*;
use codex_app_server_protocol::ThreadTimelineEntry;
use codex_app_server_protocol::WorkflowCompletionMode;
use codex_app_server_protocol::WorkflowLifecycleEvent;
use codex_app_server_protocol::WorkflowLifecycleKind;
use codex_app_server_protocol::WorkflowMetrics;
use codex_app_server_protocol::WorkflowResult;
use codex_app_server_protocol::WorkflowStatus;
use pretty_assertions::assert_eq;
use serde_json::json;

fn completed_task(thread: ThreadId, sequence: u64) -> WorkflowLifecycleEvent {
    serde_json::from_value(json!({
        "id":format!("task-fact-{sequence}"), "ownerThreadId":thread.to_string(),
        "runId":"run", "executionId":"execution", "workflowName":"audit",
        "completionMode":"display", "sequence":sequence, "createdAt":1,
        "kind":{"type":"stepCompleted","stepId":format!("step-{sequence}"),
            "attemptId":format!("attempt-{sequence}"),"childThreadId":format!("child-{sequence}"),
            "ownedTurnIds":[format!("turn-{sequence}")],"finalTurnId":format!("turn-{sequence}"),
            "label":format!("Task {sequence}"),"phase":null,"status":"succeeded",
            "result":{"type":"absent"},"error":null,"resolvedModel":null,
            "reasoningEffort":null,"activeDurationMs":220_000}
    }))
    .expect("native lifecycle task completion")
}

#[tokio::test]
async fn adjacent_live_and_replayed_tasks_group_once_until_an_ordinary_message_breaks_them() {
    let (mut chat, mut events, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread = ThreadId::new();
    chat.thread_id = Some(thread);
    while events.try_recv().is_ok() {}
    while ops.try_recv().is_ok() {}
    chat.on_workflow_lifecycle(completed_task(thread, 1));
    let before = chat.transcript.active_cell_revision;
    chat.on_workflow_lifecycle(completed_task(thread, 2));
    assert!(chat.transcript.active_cell_revision > before);
    let replay = (1..=2)
        .map(|sequence| ThreadTimelineEntry::Workflow {
            position: sequence,
            event: Box::new(completed_task(thread, sequence)),
        })
        .collect();
    chat.replay_workflow_timeline(replay, &[], ReplayKind::ThreadSnapshot);
    let group = chat
        .transcript
        .active_cell
        .as_ref()
        .expect("active completed task group")
        .as_any()
        .downcast_ref::<history_cell::WorkflowLifecycleHistoryCell>()
        .expect("workflow presentation");
    assert_eq!(
        group.event_ids().collect::<Vec<_>>(),
        vec!["task-fact-1", "task-fact-2"]
    );
    assert!(
        events.try_recv().is_err(),
        "the group remains one live presentation"
    );
    chat.add_to_history(history_cell::PlainHistoryCell::new(vec![
        "Ordinary reply".into(),
    ]));
    chat.on_workflow_lifecycle(completed_task(thread, 3));
    chat.flush_active_cell();
    let text = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => Some(
                cell.display_lines(88)
                    .iter()
                    .map(Line::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        text,
        vec![
            "• Completed tasks in workflow audit\n  └ Task 1 after 3m 40s\n  └ Task 2 after 3m 40s",
            "Ordinary reply",
            "• Completed task Task 3 in workflow audit after 3m 40s",
        ]
    );
    assert!(
        ops.try_recv().is_err(),
        "presentation never submits model work"
    );
}

#[tokio::test]
async fn task_fact_during_an_active_parent_stream_keeps_the_actual_stream_tail() {
    let (mut chat, mut events, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread = ThreadId::new();
    chat.thread_id = Some(thread);
    while events.try_recv().is_ok() {}
    while ops.try_recv().is_ok() {}
    chat.on_task_started();
    chat.on_agent_message_delta("Ongoing parent answer".into());
    assert!(chat.has_active_stream_tail());
    let before = chat.transcript.active_cell_revision;
    chat.on_workflow_lifecycle(completed_task(thread, 1));
    assert!(chat.has_active_stream_tail());
    assert_eq!(
        chat.transcript.active_cell_revision, before,
        "a background workflow fact must not replace the parent's active cell"
    );
    assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(event,
        AppEvent::InsertHistoryCell(cell) if cell.as_any().is::<history_cell::WorkflowLifecycleHistoryCell>())));
    assert!(ops.try_recv().is_err());
}

#[tokio::test]
async fn a_long_completion_run_splits_its_presentation_without_losing_or_reordering_facts() {
    let (mut chat, mut events, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread = ThreadId::new();
    chat.thread_id = Some(thread);
    while events.try_recv().is_ok() {}
    while ops.try_recv().is_ok() {}
    for sequence in 1..=65 {
        chat.on_workflow_lifecycle(completed_task(thread, sequence));
    }
    chat.flush_active_cell();
    let groups = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => cell
                .as_any()
                .downcast_ref::<history_cell::WorkflowLifecycleHistoryCell>()
                .map(|group| group.event_ids().map(str::to_owned).collect::<Vec<_>>()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(groups.iter().map(Vec::len).collect::<Vec<_>>(), vec![64, 1]);
    assert_eq!(
        groups.into_iter().flatten().collect::<Vec<_>>(),
        (1..=65)
            .map(|sequence| format!("task-fact-{sequence}"))
            .collect::<Vec<_>>()
    );
    assert!(ops.try_recv().is_err());
}

#[tokio::test]
async fn hidden_private_items_break_live_and_replayed_task_groups_without_becoming_visible() {
    // Reconnect may deliver a completed item without its earlier start notification.
    for mode in [
        "cold replay",
        "live start and completion",
        "live completion only",
    ] {
        let (mut chat, mut events, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
        let thread = ThreadId::new();
        chat.thread_id = Some(thread);
        while events.try_recv().is_ok() {}
        while ops.try_recv().is_ok() {}
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
            ThreadTimelineEntry::Workflow {
                position: 1,
                event: Box::new(completed_task(thread, 1)),
            },
            item(
                2,
                json!({"type":"agentMessage","id":"private","text":"PRIVATE_GROUP_BOUNDARY","phase":"commentary"}),
            ),
            ThreadTimelineEntry::Workflow {
                position: 3,
                event: Box::new(completed_task(thread, 2)),
            },
        ];
        for pass in 0..2 {
            if pass == 0 && mode != "cold replay" {
                for entry in entries.clone() {
                    match entry {
                        ThreadTimelineEntry::Workflow { event, .. } => {
                            chat.on_workflow_lifecycle(*event)
                        }
                        ThreadTimelineEntry::Item { item, turn_id, .. } => {
                            if mode != "live completion only"
                                || !matches!(
                                    item.as_ref(),
                                    codex_app_server_protocol::ThreadItem::AgentMessage { .. }
                                )
                            {
                                chat.handle_server_notification(
                                    ServerNotification::ItemStarted(
                                        codex_app_server_protocol::ItemStartedNotification {
                                            thread_id: thread.to_string(),
                                            turn_id: turn_id.clone(),
                                            item: item.as_ref().clone(),
                                            started_at_ms: 0,
                                        },
                                    ),
                                    None,
                                );
                            }
                            chat.handle_server_notification(
                                ServerNotification::ItemCompleted(
                                    codex_app_server_protocol::ItemCompletedNotification {
                                        thread_id: thread.to_string(),
                                        turn_id,
                                        item: *item,
                                        completed_at_ms: 0,
                                    },
                                ),
                                None,
                            );
                        }
                        _ => unreachable!("only native items and workflow facts in this fixture"),
                    }
                }
            } else {
                chat.replay_workflow_timeline(
                    entries.clone(),
                    &[],
                    ReplayKind::ResumeInitialMessages,
                );
            }
            chat.flush_active_cell();
            let cells = std::iter::from_fn(|| events.try_recv().ok())
                .filter_map(|event| match event {
                    AppEvent::InsertHistoryCell(cell) => Some(cell),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert!(
                cells
                    .iter()
                    .flat_map(|cell| cell.raw_lines())
                    .all(|line| !line.to_string().contains("PRIVATE_GROUP_BOUNDARY"))
            );
            let groups = cells
                .iter()
                .filter_map(|cell| {
                    cell.as_any()
                        .downcast_ref::<history_cell::WorkflowLifecycleHistoryCell>()
                })
                .map(|group| group.event_ids().map(str::to_owned).collect::<Vec<_>>())
                .collect::<Vec<_>>();
            assert_eq!(
                groups,
                if pass == 0 {
                    vec![
                        vec!["task-fact-1".to_owned()],
                        vec!["task-fact-2".to_owned()],
                    ]
                } else {
                    Vec::new()
                },
                "{mode}: warm replay cannot duplicate the original presentation"
            );
        }
        assert!(ops.try_recv().is_err());
    }
}

#[tokio::test]
async fn workflow_live_and_replay_share_stable_identity_without_model_submission() {
    let (mut chat, mut events, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread = ThreadId::new();
    chat.thread_id = Some(thread);
    let event = WorkflowLifecycleEvent {
        id: "workflow-result-1".into(),
        owner_thread_id: thread.to_string(),
        run_id: "run".into(),
        execution_id: "execution".into(),
        workflow_name: "audit".into(),
        completion_mode: WorkflowCompletionMode::Display,
        sequence: 3,
        created_at: 1,
        kind: WorkflowLifecycleKind::Completed {
            name: "audit".into(),
            status: WorkflowStatus::Succeeded,
            result: WorkflowResult::Value {
                value: json!({"checked":true}),
            },
            error: None,
            metrics: WorkflowMetrics::default(),
        },
    };
    while events.try_recv().is_ok() {}
    while ops.try_recv().is_ok() {}
    chat.handle_server_notification(
        ServerNotification::WorkflowLifecycleChanged(
            codex_app_server_protocol::WorkflowLifecycleChangedNotification {
                thread_id: thread.to_string(),
                event: event.clone(),
                final_message: None,
            },
        ),
        None,
    );
    chat.replay_workflow_timeline(
        vec![ThreadTimelineEntry::Workflow {
            position: 7,
            event: Box::new(event.clone()),
        }],
        &[],
        ReplayKind::ThreadSnapshot,
    );
    let cells = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::InsertHistoryCell(cell)
                if cell
                    .as_any()
                    .is::<history_cell::WorkflowLifecycleHistoryCell>() =>
            {
                Some(cell)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(cells.len(), 1);
    assert!(
        cells[0]
            .raw_lines()
            .iter()
            .any(|line| line.to_string().contains("checked"))
    );
    assert!(
        ops.try_recv().is_err(),
        "human workflow results never create a model turn"
    );
    let mut foreign = event;
    foreign.id = "foreign".into();
    foreign.owner_thread_id = ThreadId::new().to_string();
    chat.on_workflow_lifecycle(foreign);
    assert!(
        events.try_recv().is_err(),
        "another conversation's result cannot enter this transcript"
    );
}

#[tokio::test]
async fn canonical_initial_replay_preserves_typed_content_before_known_voice_handoff() {
    let (mut chat, mut events, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread = ThreadId::new();
    chat.thread_id = Some(thread);
    while events.try_recv().is_ok() {}
    let items: Vec<codex_app_server_protocol::ThreadItem> = serde_json::from_value(json!([
        {"type":"agentMessage","id":"typed","text":"Earlier typed commentary","phase":"commentary"},
        {"type":"userMessage","id":"voice","content":[{"type":"text","text":"<realtime_delegation><input>question</input></realtime_delegation>","textElements":[]}]},
        {"type":"agentMessage","id":"private","text":"Later voice-private commentary","phase":"commentary"},
    ])).unwrap();
    let mut entries = items
        .into_iter()
        .enumerate()
        .map(|(position, item)| ThreadTimelineEntry::Item {
            position: position as u64,
            turn_id: "shared".into(),
            item: Box::new(item),
        })
        .collect::<Vec<_>>();
    entries.push(ThreadTimelineEntry::TurnCompleted {
        position: 3,
        turn_id: "shared".into(),
        status: codex_app_server_protocol::TurnStatus::Completed,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    });
    chat.replay_workflow_timeline(entries, &[], ReplayKind::ResumeInitialMessages);
    let text = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => Some(
                cell.raw_lines()
                    .iter()
                    .map(Line::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Earlier typed commentary"));
    assert!(!text.contains("Later voice-private commentary"));
}
