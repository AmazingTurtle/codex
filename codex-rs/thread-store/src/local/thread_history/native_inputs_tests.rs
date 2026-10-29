//! A pre-fix projection checkpoint must not hide the actual native task input forever.
use super::*;
use codex_app_server_protocol::ThreadItem;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test]
async fn existing_checkpoint_recovers_only_true_native_input_in_the_visible_turn() {
    let (_home, store, thread_id) = store_with_mode(ThreadHistoryMode::Paginated).await;
    let lineage = store
        .resolve_rollout_lineage_for_history_page(thread_id)
        .await
        .unwrap();
    let path = lineage.segments().last().unwrap().rollout_path.clone();
    let native:codex_protocol::models::ResponseItem=serde_json::from_value(json!({
        "type":"agent_message","id":"amsg_original","author":"/root","recipient":"/root/reviewer",
        "content":[{"type":"input_text","text":"The actual native task.\nKeep this entire prompt."}],
        "internal_chat_message_metadata_passthrough":{"turn_id":"owned-turn"}
    })).unwrap();
    let mut raw = fs::read(&path).unwrap();
    for (ordinal,item) in [
        (1,RolloutItem::EventMsg(serde_json::from_value(json!({"type":"task_started","turn_id":"owned-turn"})).unwrap())),
        (2,RolloutItem::ResponseItem(native.into())),
        (3,RolloutItem::EventMsg(serde_json::from_value(json!({"type":"task_complete","turn_id":"owned-turn","last_agent_message":"Finished"})).unwrap())),
    ] {
        let line=RolloutLine {timestamp:"2026-10-30T12:00:00Z".into(),ordinal:Some(ordinal),item};
        raw.extend(serde_json::to_vec(&line).unwrap());raw.push(b'\n');
    }
    fs::write(&path, &raw).unwrap();
    crate::local::thread_history_materialization::materialize_to_sqlite(
        &store, thread_id, thread_id, &path,
    )
    .await
    .unwrap();
    let mut params = item_params(thread_id, Some("owned-turn"), None, 10, SortDirection::Asc);
    let projected = store.list_items(params.clone()).await.unwrap();
    assert_eq!(projected.items.len(), 1);
    let db = history_db(&store).await;
    let checkpoint=sqlx::query_as::<_,(i64,i64)>("SELECT next_rollout_byte_offset,next_rollout_ordinal FROM thread_history_projection_state WHERE thread_id=?")
        .bind(thread_id.to_string()).fetch_one(db).await.unwrap();
    sqlx::query("DELETE FROM thread_items WHERE thread_id=? AND item_id='amsg_original'")
        .bind(thread_id.to_string())
        .execute(db)
        .await
        .unwrap();
    sqlx::query("UPDATE thread_turns SET first_user_item_id=NULL WHERE thread_id=?")
        .bind(thread_id.to_string())
        .execute(db)
        .await
        .unwrap();
    let recovered = store.list_items(params.clone()).await.unwrap();
    assert_eq!(
        recovered.items, projected.items,
        "compatibility repair must preserve exact turn, item ID, prompt and chronology"
    );
    let item: ThreadItem = serde_json::from_slice(&recovered.items[0].item_json).unwrap();
    assert_eq!(
        item,
        ThreadItem::UserMessage {
            id: "amsg_original".into(),
            client_id: None,
            content: vec![codex_app_server_protocol::UserInput::Text {
                text: "The actual native task.\nKeep this entire prompt.".into(),
                text_elements: Vec::new()
            }]
        }
    );
    assert_eq!(sqlx::query_as::<_,(i64,i64)>("SELECT next_rollout_byte_offset,next_rollout_ordinal FROM thread_history_projection_state WHERE thread_id=?")
        .bind(thread_id.to_string()).fetch_one(db).await.unwrap(),checkpoint);
    assert_eq!(
        fs::read(&path).unwrap(),
        raw,
        "projection repair must never rewrite model history"
    );
    assert_eq!(
        store.list_items(params.clone()).await.unwrap().items,
        recovered.items,
        "repeated reads must not duplicate native input"
    );
    params.turn_id = Some("another-turn".into());
    assert!(store.list_items(params).await.unwrap().items.is_empty());
}
