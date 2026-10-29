use super::*;
use codex_protocol::ThreadId;
use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::AgentMessageItem;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::AgentMessageContentDeltaEvent;
use codex_protocol::protocol::ExecCommandOutputDeltaEvent;
use codex_protocol::protocol::ExecOutputStream;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ItemStartedEvent;
use pretty_assertions::assert_eq;
use serde_json::json;

fn started(id: &str) -> EventMsg {
    serde_json::from_value(json!({"type":"task_started","turn_id":id})).unwrap()
}
fn agent(id: &str, text: &str) -> TurnItem {
    TurnItem::AgentMessage(AgentMessageItem {
        id: id.into(),
        content: vec![AgentMessageContent::Text { text: text.into() }],
        phase: None,
        memory_citation: None,
        delivery: None,
        questions: None,
    })
}
fn begin(thread: ThreadId, turn: &str, item: TurnItem) -> EventMsg {
    EventMsg::ItemStarted(ItemStartedEvent {
        thread_id: thread,
        turn_id: turn.into(),
        item,
        started_at_ms: 1,
    })
}
fn delta(thread: ThreadId, turn: &str, id: &str, text: &str) -> EventMsg {
    EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent {
        thread_id: thread.to_string(),
        turn_id: turn.into(),
        item_id: id.into(),
        delta: text.into(),
    })
}
fn empty(turn: &str) -> Turn {
    serde_json::from_value(json!({"id":turn,"rootTurnId":null,"items":[],"itemsView":"full","status":"inProgress","error":null,"startedAt":null,"completedAt":null,"durationMs":null})).unwrap()
}

#[test]
fn actual_started_prefix_and_command_output_survive_midstream_snapshot_and_settle_once() {
    let child = ThreadId::new();
    let mut state = super::super::ThreadState::default();
    state.track_current_turn_event("owned", &started("owned"));
    state.track_current_turn_event("owned", &begin(child, "owned", agent("native-final", "")));
    state.track_current_turn_event(
        "owned",
        &delta(child, "owned", "native-final", "actual prefix"),
    );
    state.track_current_turn_event(
        "foreign",
        &delta(child, "foreign", "native-final", "MUST_NOT_APPEAR"),
    );
    state.track_current_turn_event("owned", &begin(child, "owned", agent("native-final", "")));
    let cwd = codex_utils_absolute_path::AbsolutePathBuf::try_from(std::env::temp_dir()).unwrap();
    let command:TurnItem=serde_json::from_value(json!({"type":"CommandExecution","id":"actual-tool","command":["printf","actual"],"cwd":codex_utils_path_uri::PathUri::from(cwd),"parsed_cmd":[],"source":"agent","status":"in_progress"})).unwrap();
    state.track_current_turn_event("owned", &begin(child, "owned", command));
    state.track_current_turn_event(
        "owned",
        &EventMsg::ExecCommandOutputDelta(ExecCommandOutputDeltaEvent {
            call_id: "actual-tool".into(),
            stream: ExecOutputStream::Stdout,
            chunk: b"actual tool output".to_vec(),
        }),
    );
    let snapshot = state.inspection_active_turn_snapshot().unwrap().unwrap();
    assert_eq!(
        snapshot
            .items
            .iter()
            .filter(|item| item.item.id() == "native-final")
            .count(),
        1
    );
    assert!(
        matches!(&snapshot.items[0].item,ThreadItem::AgentMessage {id,text,..} if id=="native-final" && text=="actual prefix")
    );
    assert_eq!(
        (
            snapshot.items[0].started_at_ms,
            snapshot.items[0].completed_at_ms
        ),
        (Some(1), None)
    );
    assert!(snapshot.items.iter().any(|entry|matches!(&entry.item,ThreadItem::CommandExecution {id,aggregated_output:Some(output),..} if id=="actual-tool" && output=="actual tool output")));
    state.track_current_turn_event(
        "owned",
        &EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id: child,
            turn_id: "owned".into(),
            item: agent("native-final", "actual completed answer"),
            started_at_ms: Some(1),
            completed_at_ms: 2,
        }),
    );
    state.track_current_turn_event(
        "owned",
        &delta(child, "owned", "native-final", "STALE_TAIL"),
    );
    let settled = state.inspection_active_turn_snapshot().unwrap().unwrap();
    assert_eq!(
        settled
            .items
            .iter()
            .filter(|item| item.item.id() == "native-final")
            .count(),
        1
    );
    let completed = settled
        .items
        .iter()
        .find(|entry| entry.item.id() == "native-final")
        .unwrap();
    assert!(
        matches!(&completed.item,ThreadItem::AgentMessage {text,..} if text=="actual completed answer")
    );
    assert_eq!(
        (completed.started_at_ms, completed.completed_at_ms),
        (Some(1), Some(2))
    );
    state.track_current_turn_event("owned",&serde_json::from_value(json!({"type":"task_complete","turn_id":"owned","last_agent_message":"actual completed answer"})).unwrap());
    state.track_current_turn_event("next", &started("next"));
    assert!(
        state
            .inspection_active_turn_snapshot()
            .unwrap()
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn bounded_live_snapshots_report_incompleteness_and_reset_on_the_next_turn() {
    let child = ThreadId::new();
    let mut live = ActiveItems::default();
    live.observe("owned", &started("owned"));
    for index in 0..=MAX_ITEMS {
        live.observe(
            "owned",
            &begin(child, "owned", agent(&format!("item-{index}"), "task")),
        );
    }
    assert!(
        live.merge(empty("owned"))
            .unwrap_err()
            .contains("bounded live snapshot")
    );
    live.observe("new", &started("new"));
    live.observe("new", &begin(child, "new", agent("only", "")));
    live.observe(
        "new",
        &delta(child, "new", "only", &"x".repeat(MAX_BYTES + 1)),
    );
    assert!(live.merge(empty("new")).is_err());
    live.reset();
    assert_eq!(
        live.merge(empty("new")).unwrap().items,
        Vec::<ThreadItem>::new()
    );
}
