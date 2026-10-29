use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use codex_protocol::AgentPath;
use codex_protocol::ResponseItemId;
use codex_protocol::ThreadId;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::HistoryPosition;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::ResponseItemEnvelope;
use codex_rollout::RolloutItem;
use codex_rollout::RolloutLine;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::MAX_PERSISTED_INPUT_IDS;
use crate::ThreadStore;
use crate::local::test_support::test_config;
use crate::local::test_support::write_session_file_with_history_mode;

fn response(id: &str, order: u64) -> RolloutItem {
    let item: ResponseItem = serde_json::from_value(json!({
        "type":"message","id":id,"role":"user","content":[{"type":"input_text","text":"Delivered input"}],
    })).unwrap();
    let mut envelope = ResponseItemEnvelope::from(item);
    envelope.metadata = Some(serde_json::from_value(json!({"user_input_order":order})).unwrap());
    RolloutItem::ResponseItem(envelope)
}

fn mail(id: &str) -> RolloutItem {
    let mut mail = InterAgentCommunication::new(
        AgentPath::root().join("worker").unwrap(),
        AgentPath::root(),
        Vec::new(),
        "Delivered worker completion".to_string(),
        /*trigger_turn*/ false,
    );
    mail.id = Some(ResponseItemId::from_server(id.to_string()));
    RolloutItem::InterAgentCommunication(mail)
}

fn compaction() -> RolloutItem {
    serde_json::from_value(json!({"type":"compacted","payload":{
        "message":"Earlier work summarized","replacement_history":[],"window_number":1,
    }}))
    .unwrap()
}

fn key(id: &str) -> PersistedInputId {
    PersistedInputId::ResponseItem(ResponseItemId::from_server(id.to_string()))
}

fn append(path: &Path, ordinal: u64, item: RolloutItem) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    let line = RolloutLine {
        timestamp: "2025-01-03T13:00:00Z".to_string(),
        ordinal: Some(ordinal),
        item,
    };
    writeln!(file, "{}", serde_json::to_string(&line).unwrap()).unwrap();
}

#[tokio::test]
async fn persisted_input_lookup_finds_raw_receipts_before_compaction_in_compressed_history() {
    let home = TempDir::new().unwrap();
    let uuid = uuid::Uuid::from_u128(/*v*/ 4101);
    let thread_id = ThreadId::from_string(&uuid.to_string()).unwrap();
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-00-00",
        uuid,
        ThreadHistoryMode::Paginated,
    )
    .unwrap();
    append(
        &path,
        /*ordinal*/ 1,
        response("old-input", /*order*/ 7),
    );
    append(&path, /*ordinal*/ 2, mail("old-mail"));
    append(&path, /*ordinal*/ 3, RolloutItem::EventMsg(serde_json::from_value(json!({
        "type":"workflow_lifecycle","id":"workflow-ui","ownerThreadId":thread_id,"runId":"run","executionId":"execution","workflowName":"Review","completionMode":"display","sequence":0,"createdAt":1,
        "kind":{"type":"started","name":"Review","description":"Inspect"}
    })).unwrap()));
    append(&path, /*ordinal*/ 4, compaction());
    append(
        &path,
        /*ordinal*/ 5,
        response("new-input", /*order*/ 8),
    );
    let mut agent_message = ResponseItemEnvelope::from(serde_json::from_value::<ResponseItem>(json!({
        "type":"agent_message","id":"agent-message","author":"/worker","recipient":"/","content":[],
    })).unwrap());
    agent_message.metadata = Some(serde_json::from_value(json!({"user_input_order":99})).unwrap());
    append(
        &path,
        /*ordinal*/ 6,
        RolloutItem::ResponseItem(agent_message),
    );
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    assert!(matches!(
        store
            .load_history(crate::LoadThreadHistoryParams {
                thread_id,
                include_archived: false
            })
            .await,
        Err(ThreadStoreError::Unsupported {
            operation: "paginated_threads"
        })
    ));
    let params = FindPersistedInputIdsParams {
        thread_id,
        ids: vec![
            key("old-input"),
            key("old-mail"),
            PersistedInputId::WorkflowLifecycle("workflow-ui".into()),
            key("absent"),
            PersistedInputId::UserInputOrder(7),
            PersistedInputId::UserInputOrder(99),
            key("old-input"),
        ],
    };
    let expected = BTreeSet::from([
        key("old-input"),
        key("old-mail"),
        PersistedInputId::WorkflowLifecycle("workflow-ui".into()),
        PersistedInputId::UserInputOrder(7),
    ]);
    assert_eq!(
        store
            .find_persisted_input_ids(params.clone())
            .await
            .unwrap(),
        expected
    );
    // Mail can share an ordering marker with not-yet-recorded user input. Only
    // the actual user-role record proves delivery of that acceptance order.
    append(
        &path,
        /*ordinal*/ 7,
        response("accepted-after-mail", /*order*/ 99),
    );
    assert_eq!(
        store
            .find_persisted_input_ids(FindPersistedInputIdsParams {
                thread_id,
                ids: vec![PersistedInputId::UserInputOrder(99)]
            })
            .await
            .unwrap(),
        BTreeSet::from([PersistedInputId::UserInputOrder(99)])
    );
    let mut expected = expected;
    expected.insert(PersistedInputId::UserInputOrder(99));
    let input = std::fs::File::open(&path).unwrap();
    let output = std::fs::File::create(path.with_extension("jsonl.zst")).unwrap();
    zstd::stream::copy_encode(input, output, /*level*/ 3).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(
        store.find_persisted_input_ids(params).await.unwrap(),
        expected
    );
}

#[tokio::test]
async fn persisted_input_lookup_respects_frozen_parent_cutoff_and_local_child_delta() {
    let home = TempDir::new().unwrap();
    let parent_uuid = uuid::Uuid::from_u128(/*v*/ 4102);
    let child_uuid = uuid::Uuid::from_u128(/*v*/ 4103);
    let parent_id = ThreadId::from_string(&parent_uuid.to_string()).unwrap();
    let child_id = ThreadId::from_string(&child_uuid.to_string()).unwrap();
    let parent = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-00-01",
        parent_uuid,
        ThreadHistoryMode::Paginated,
    )
    .unwrap();
    append(
        &parent,
        /*ordinal*/ 1,
        response("inherited", /*order*/ 1),
    );
    let cutoff = std::fs::metadata(&parent).unwrap().len();
    append(
        &parent,
        /*ordinal*/ 2,
        response("after-fork", /*order*/ 2),
    );
    let child = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-00-02",
        child_uuid,
        ThreadHistoryMode::Paginated,
    )
    .unwrap();
    let mut meta: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&child).unwrap().trim()).unwrap();
    meta["payload"]["history_base"] = serde_json::to_value(HistoryPosition {
        thread_id: parent_id,
        end_ordinal_exclusive: 2,
        end_byte_offset: cutoff,
    })
    .unwrap();
    meta["ordinal"] = json!(2);
    std::fs::write(&child, format!("{meta}\n")).unwrap();
    append(
        &child,
        /*ordinal*/ 3,
        response("child-input", /*order*/ 3),
    );
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let found = store
        .find_persisted_input_ids(FindPersistedInputIdsParams {
            thread_id: child_id,
            ids: vec![key("inherited"), key("after-fork"), key("child-input")],
        })
        .await
        .unwrap();
    assert_eq!(
        found,
        BTreeSet::from([key("inherited"), key("child-input")])
    );
}

#[tokio::test]
async fn persisted_input_lookup_distinguishes_absence_from_unreadable_history_and_bounds_requests()
{
    let home = TempDir::new().unwrap();
    let uuid = uuid::Uuid::from_u128(/*v*/ 4104);
    let thread_id = ThreadId::from_string(&uuid.to_string()).unwrap();
    let path = write_session_file_with_history_mode(
        home.path(),
        "2025-01-03T13-00-03",
        uuid,
        ThreadHistoryMode::Legacy,
    )
    .unwrap();
    append(
        &path,
        /*ordinal*/ 1,
        response("delivered", /*order*/ 4),
    );
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    assert_eq!(
        store
            .find_persisted_input_ids(FindPersistedInputIdsParams {
                thread_id,
                ids: vec![key("delivered"), key("missing")]
            })
            .await
            .unwrap(),
        BTreeSet::from([key("delivered")])
    );
    let too_many = FindPersistedInputIdsParams {
        thread_id,
        ids: vec![key("delivered"); MAX_PERSISTED_INPUT_IDS + 1],
    };
    assert!(matches!(
        store.find_persisted_input_ids(too_many).await,
        Err(ThreadStoreError::InvalidRequest { .. })
    ));
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"not-json\n")
        .unwrap();
    assert!(matches!(
        store
            .find_persisted_input_ids(FindPersistedInputIdsParams {
                thread_id,
                ids: vec![key("missing")]
            })
            .await,
        Err(ThreadStoreError::Internal { .. })
    ));
    std::fs::remove_file(&path).unwrap();
    assert!(
        store
            .find_persisted_input_ids(FindPersistedInputIdsParams {
                thread_id,
                ids: vec![key("missing")]
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn persisted_input_lookup_keeps_known_unmaterialized_threads_unmaterialized() {
    let home = TempDir::new().unwrap();
    let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
    let thread_id = ThreadId::new();
    store
        .create_thread(crate::CreateThreadParams {
            attached_thread: false,
            creator_user_id: None,
            creator_account_id: None,
            session_id: thread_id.into(),
            thread_id,
            extra_config: None,
            forked_from_id: None,
            parent_thread_id: None,
            source: codex_protocol::protocol::SessionSource::Exec,
            thread_source: None,
            originator: "test".to_string(),
            base_instructions: Default::default(),
            dynamic_tools: Vec::new(),
            selected_capability_roots: Vec::new(),
            multi_agent_version: None,
            history_mode: ThreadHistoryMode::Paginated,
            history_base: None,
            subagent_history_start_ordinal: None,
            initial_window_id: uuid::Uuid::now_v7().to_string(),
            runtime_workspace_roots: None,
            metadata: crate::ThreadPersistenceMetadata {
                cwd: Some(home.path().to_path_buf()),
                model_provider: "test".to_string(),
                memory_mode: codex_protocol::protocol::ThreadMemoryMode::Enabled,
            },
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .find_persisted_input_ids(FindPersistedInputIdsParams {
                thread_id,
                ids: vec![key("never-delivered")]
            })
            .await
            .unwrap(),
        BTreeSet::new()
    );
    assert_eq!(store.feedback_rollout_path(thread_id).await.unwrap(), None);
    store.discard_thread(thread_id).await.unwrap();
}
