use std::collections::BTreeSet;

use codex_protocol::ResponseItemId;
use codex_rollout::ResponseItemEnvelope;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::FindPersistedInputIdsParams;
use crate::PersistedInputId;

#[tokio::test]
async fn persisted_input_lookup_matches_raw_record_ids_and_user_order_without_history_loading() {
    let store = InMemoryThreadStore::default();
    let thread_id = ThreadId::new();
    let id = ResponseItemId::from_server("persisted-input".to_string());
    let mut envelope = ResponseItemEnvelope::from(serde_json::from_value::<codex_protocol::models::ResponseItem>(json!({
        "type":"message","id":id,"role":"user","content":[{"type":"input_text","text":"Recorded input"}],
    })).unwrap());
    envelope.metadata = Some(serde_json::from_value(json!({"user_input_order":12})).unwrap());
    let mut agent_message = ResponseItemEnvelope::from(serde_json::from_value::<codex_protocol::models::ResponseItem>(json!({
        "type":"agent_message","id":"agent-message","author":"/worker","recipient":"/","content":[],
    })).unwrap());
    agent_message.metadata = Some(serde_json::from_value(json!({"user_input_order":13})).unwrap());
    store.state.lock().await.histories.insert(
        thread_id,
        vec![
            RolloutItem::ResponseItem(envelope.clone()),
            RolloutItem::ResponseItem(agent_message),
        ],
    );
    let ids = vec![
        PersistedInputId::ResponseItem(id.clone()),
        PersistedInputId::UserInputOrder(12),
        PersistedInputId::UserInputOrder(13),
    ];
    assert_eq!(
        store
            .find_persisted_input_ids(FindPersistedInputIdsParams { thread_id, ids })
            .await
            .unwrap(),
        BTreeSet::from([
            PersistedInputId::ResponseItem(id),
            PersistedInputId::UserInputOrder(12)
        ])
    );
    envelope.metadata = Some(serde_json::from_value(json!({"user_input_order":13})).unwrap());
    store
        .state
        .lock()
        .await
        .histories
        .get_mut(&thread_id)
        .unwrap()
        .push(RolloutItem::ResponseItem(envelope));
    assert_eq!(
        store
            .find_persisted_input_ids(FindPersistedInputIdsParams {
                thread_id,
                ids: vec![PersistedInputId::UserInputOrder(13)]
            })
            .await
            .unwrap(),
        BTreeSet::from([PersistedInputId::UserInputOrder(13)])
    );
    assert_eq!(store.calls().await.load_history, 0);
    assert!(matches!(
        store
            .find_persisted_input_ids(FindPersistedInputIdsParams {
                thread_id: ThreadId::new(),
                ids: vec![PersistedInputId::UserInputOrder(12)]
            })
            .await,
        Err(ThreadStoreError::ThreadNotFound { .. })
    ));
}
