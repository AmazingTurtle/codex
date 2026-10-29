use super::*;
use crate::test_support::PathBufExt;
use crate::test_support::test_path_buf;
use codex_app_server_protocol::AgentMessageDeltaNotification;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::TurnStartedNotification;
use pretty_assertions::assert_eq;

fn cached_store(id: ThreadId, entries: Vec<ThreadTimelineEntry>) -> ThreadEventStore {
    let session = ThreadSessionState {
        timeline: Some(TimelineHistorySnapshot::Available(entries)),
        daybreak_enabled: false,
        long_context: false,
        wake_state: None,
        windows_sandbox_host: WindowsSandboxHost::Local,
        thread_id: id,
        forked_from_id: None,
        fork_parent_title: None,
        thread_name: None,
        model: "test".into(),
        model_provider_id: "test".into(),
        service_tier: None,
        approval_policy: codex_app_server_protocol::AskForApproval::Never,
        approvals_reviewer: codex_protocol::config_types::ApprovalsReviewer::User,
        permission_profile: codex_protocol::models::PermissionProfile::read_only(),
        active_permission_profile: None,
        cwd: test_path_buf("/project").abs(),
        runtime_workspace_roots: Vec::new(),
        instruction_source_paths: Vec::new(),
        reasoning_effort: None,
        collaboration_mode: None,
        message_history: None,
        network_proxy: None,
        rollout_path: None,
    };
    ThreadEventStore::new_with_session(/*capacity*/ 2, session, Vec::new())
}

fn started(id: ThreadId, turn_id: &str) -> ServerNotification {
    ServerNotification::TurnStarted(TurnStartedNotification {
        thread_id: id.to_string(),
        turn: Turn {
            id: turn_id.into(),
            root_turn_id: None,
            items: Vec::new(),
            items_view: codex_app_server_protocol::TurnItemsView::Full,
            status: TurnStatus::InProgress,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        },
    })
}

fn delta(id: ThreadId, turn_id: &str, item_id: &str) -> ServerNotification {
    ServerNotification::AgentMessageDelta(AgentMessageDeltaNotification {
        thread_id: id.to_string(),
        turn_id: turn_id.into(),
        item_id: item_id.into(),
        delta: "live".into(),
    })
}

#[test]
fn queued_snapshot_facts_do_not_restart_a_completed_turn_or_hide_newer_output() {
    let id = ThreadId::new();
    let item: ThreadItem = serde_json::from_value(serde_json::json!({
        "type":"agentMessage", "id":"answer", "text":"Saved answer",
    }))
    .unwrap();
    let mut store = cached_store(
        id,
        vec![
            ThreadTimelineEntry::Item {
                position: 1,
                turn_id: "done".into(),
                item: Box::new(item.clone()),
            },
            ThreadTimelineEntry::TurnCompleted {
                position: 2,
                turn_id: "done".into(),
                status: TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            },
        ],
    );
    store.push_notification(started(id, "done"));
    store.push_notification(ServerNotification::ItemCompleted(
        ItemCompletedNotification {
            thread_id: id.to_string(),
            turn_id: "done".into(),
            item,
            completed_at_ms: 0,
        },
    ));
    assert_eq!(
        (
            store.active_turn_id.clone(),
            store.buffer.len(),
            store.dirty_timeline_revision()
        ),
        (None, 0, None)
    );
    store.push_notification(started(id, "new"));
    store.push_notification(delta(id, "new", "answer"));
    assert_eq!(
        (store.active_turn_id.as_deref(), store.buffer.len()),
        (Some("new"), 2)
    );
    assert!(store.dirty_timeline_revision().is_some());
}

#[test]
fn queued_started_initializes_a_no_item_turn_missing_from_earlier_turn_metadata() {
    let id = ThreadId::new();
    let mut store = cached_store(
        id,
        vec![ThreadTimelineEntry::TurnStarted {
            position: 1,
            turn_id: "active".into(),
            started_at: None,
        }],
    );
    assert_eq!(store.active_turn_id, None);
    let notification = started(id, "active");
    assert!(!store.timeline_notification_covered(&notification));
    store.push_notification(notification);
    assert_eq!(
        (store.active_turn_id.as_deref(), store.buffer.len()),
        (Some("active"), 1)
    );
    // The same delivery becomes redundant after it has initialized native active state.
    store.push_notification(started(id, "active"));
    assert_eq!(
        (store.active_turn_id.as_deref(), store.buffer.len()),
        (Some("active"), 1)
    );
}

#[test]
fn canonical_completion_replaces_buffered_streaming_before_live_completion_arrives() {
    let id = ThreadId::new();
    let mut store = cached_store(id, Vec::new());
    store.push_notification(started(id, "active"));
    store.push_notification(delta(id, "active", "answer"));
    let terminal = ThreadTimelineEntry::TurnCompleted {
        position: 2,
        turn_id: "active".into(),
        status: TurnStatus::Completed,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    };
    // The RPC sees the committed final answer before its live completion is routed.
    let final_item: ThreadItem = serde_json::from_value(serde_json::json!({
        "type":"agentMessage", "id":"answer", "text":"The complete answer",
    }))
    .unwrap();
    store.rebase_canonical_buffer(&[
        ThreadTimelineEntry::Item {
            position: 1,
            turn_id: "active".into(),
            item: Box::new(final_item),
        },
        terminal,
    ]);
    assert_eq!(
        (store.buffer.len(), store.buffered_agent_message_delta_bytes),
        (0, 0)
    );
}

#[test]
fn dirty_revision_and_uncovered_live_output_survive_replay_buffer_pressure() {
    let id = ThreadId::new();
    let mut store = cached_store(id, Vec::new());
    store.push_notification(started(id, "active"));
    for item in ["first", "second", "third"] {
        store.push_notification(delta(id, "active", item));
    }
    let revision = store
        .dirty_timeline_revision()
        .expect("buffer eviction must not mark history clean");
    let earlier_partial: ThreadItem = serde_json::from_value(serde_json::json!({
        "type":"agentMessage", "id":"third", "text":"An earlier snapshot prefix",
    }))
    .unwrap();
    store.rebase_canonical_buffer(&[ThreadTimelineEntry::Item {
        position: 1,
        turn_id: "active".into(),
        item: Box::new(earlier_partial),
    }]);
    let actual = store
        .buffer
        .iter()
        .filter_map(|event| match event {
            ThreadBufferedEvent::Notification(notification) => Some(notification.as_ref().clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(vec![
            delta(id, "active", "second"),
            delta(id, "active", "third"),
        ])
        .unwrap()
    );
    assert_eq!(store.buffered_agent_message_delta_bytes, 8);
    store.push_notification(delta(id, "active", "fourth"));
    assert_ne!(
        store.dirty_timeline_revision(),
        Some(revision),
        "a racing event must invalidate the captured refresh revision"
    );
}
