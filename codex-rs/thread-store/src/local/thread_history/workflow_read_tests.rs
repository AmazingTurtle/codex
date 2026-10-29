use super::*;
use codex_app_server_protocol::ThreadItem;
use pretty_assertions::assert_eq;

#[path = "workflow_origin_tests.rs"]
mod origin_tests;

#[tokio::test]
async fn workflow_timeline_pages_deduplicate_immutable_events_without_creating_turns() {
    let (_home, store, thread_id) = store_with_mode(ThreadHistoryMode::Paginated).await;
    let db = history_db(&store).await;
    insert_item(db, thread_id, "human-turn", "before", 10).await;
    insert_item(db, thread_id, "human-turn", "after", 30).await;
    let event:codex_app_server_protocol::WorkflowLifecycleEvent=serde_json::from_value(serde_json::json!({
        "id":"workflow-event","ownerThreadId":thread_id,"runId":"run","executionId":"execution",
        "workflowName":"Review","completionMode":"display","sequence":2,"createdAt":20,
        "kind":{"type":"completed","name":"Review","status":"succeeded","result":{"type":"value","value":null},"error":null,
        "metrics":{"usage":null,"usageComplete":false,"usageIncompleteReason":null,"activeDurationMs":0,"wallDurationMs":0}}
    })).expect("typed workflow fact");
    let mut transaction = db.begin().await.expect("begin");
    crate::local::thread_history::workflow::insert(
        &mut transaction,
        &thread_id.to_string(),
        20,
        event.clone(),
    )
    .await
    .expect("project workflow event");
    crate::local::thread_history::workflow::insert(
        &mut transaction,
        &thread_id.to_string(),
        25,
        event.clone(),
    )
    .await
    .expect("duplicate retains original position");
    let mut changed = event.clone();
    changed.kind = codex_app_server_protocol::WorkflowLifecycleKind::Started {
        name: "conflicting".into(),
        description: "wrong".into(),
    };
    assert!(matches!(
        crate::local::thread_history::workflow::insert(
            &mut transaction,
            &thread_id.to_string(),
            26,
            changed
        )
        .await,
        Err(ThreadStoreError::Conflict { .. })
    ));
    let mut foreign = event.clone();
    foreign.owner_thread_id = ThreadId::new().to_string();
    assert!(matches!(
        crate::local::thread_history::workflow::insert(
            &mut transaction,
            &thread_id.to_string(),
            27,
            foreign
        )
        .await,
        Err(ThreadStoreError::InvalidRequest { .. })
    ));
    transaction.commit().await.expect("commit immutable event");
    let latest = store
        .list_timeline(ListTimelineParams {
            thread_id,
            cursor: None,
            page_size: 2,
        })
        .await
        .expect("latest mixed page");
    assert_eq!(
        latest.items[0],
        ThreadTimelineEntry::Workflow {
            position: 20,
            event: Box::new(event.clone())
        }
    );
    assert!(matches!(
        &latest.items[1],
        ThreadTimelineEntry::Item { position: 30, .. }
    ));
    let older = store
        .list_timeline(ListTimelineParams {
            thread_id,
            cursor: latest.next_cursor,
            page_size: 2,
        })
        .await
        .expect("older page");
    assert_eq!(older.items.len(), 1);
    assert!(matches!(
        &older.items[0],
        ThreadTimelineEntry::Item { position: 10, .. }
    ));
    assert_eq!(older.next_cursor, None);
    let turns: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM thread_turns WHERE thread_id = ?")
        .bind(thread_id.to_string())
        .fetch_one(db)
        .await
        .expect("turn count");
    assert_eq!(turns, 0);
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM thread_workflow_events WHERE thread_id = ?")
            .bind(thread_id.to_string())
            .fetch_one(db)
            .await
            .expect("event count");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn legacy_timeline_keeps_workflow_events_between_real_messages_across_cold_pages() {
    use std::io::Write;
    let home = TempDir::new().expect("home");
    let config = test_config(home.path());
    let thread_id = ThreadId::new();
    let path = crate::local::test_support::write_session_file(
        home.path(),
        "2025-01-03T13-00-00",
        uuid::Uuid::parse_str(&thread_id.to_string()).unwrap(),
    )
    .expect("legacy session");
    let lifecycle:EventMsg=serde_json::from_value(serde_json::json!({
        "type":"workflow_lifecycle","id":"legacy-ui","ownerThreadId":thread_id,"runId":"run","executionId":"execution","workflowName":"Legacy conversation","completionMode":"display","sequence":0,"createdAt":20,
        "kind":{"type":"started","name":"Legacy conversation","description":"A new workflow on an existing chat"}
    })).unwrap();
    let mut writer = fs::OpenOptions::new().append(true).open(&path).unwrap();
    for item in [
        RolloutItem::EventMsg(serde_json::from_value(serde_json::json!({"type":"task_started","turn_id":"actual-turn","model_context_window":null})).expect("legacy turn start")),
        RolloutItem::EventMsg(serde_json::from_value(serde_json::json!({"type":"agent_message","message":"Before workflow"})).expect("assistant message")),
        RolloutItem::EventMsg(lifecycle.clone()),
        RolloutItem::EventMsg(serde_json::from_value(serde_json::json!({"type":"agent_message","message":"After workflow"})).expect("assistant message")),
    ] {writeln!(writer,"{}",serde_json::to_string(&RolloutLine {timestamp:"2025-01-03T13:00:00Z".into(),ordinal:None,item}).unwrap()).unwrap();}
    drop(writer);
    let bytes = fs::read(&path).unwrap();
    let store = LocalThreadStore::new(config.clone(), /*state_db*/ None);
    let latest = store
        .list_timeline(ListTimelineParams {
            thread_id,
            cursor: None,
            page_size: 2,
        })
        .await
        .expect("legacy mixed timeline");
    assert_eq!(latest.items.len(), 2);
    assert!(
        matches!(&latest.items[0],ThreadTimelineEntry::Workflow {event,..} if event.id=="legacy-ui")
    );
    assert!(
        matches!(&latest.items[1],ThreadTimelineEntry::Item {item,..} if matches!(item.as_ref(),ThreadItem::AgentMessage {text,..} if text=="After workflow"))
    );
    let reopened = LocalThreadStore::new(config, /*state_db*/ None);
    let older = reopened
        .list_timeline(ListTimelineParams {
            thread_id,
            cursor: latest.next_cursor,
            page_size: 100,
        })
        .await
        .expect("cold older legacy timeline");
    assert!(older.items.iter().any(|entry|matches!(entry,ThreadTimelineEntry::Item {item,..} if matches!(item.as_ref(),ThreadItem::AgentMessage {text,..} if text=="Before workflow"))));
    assert!(
        !older
            .items
            .iter()
            .any(|entry| matches!(entry, ThreadTimelineEntry::Workflow { .. }))
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
}
