//! Immutable workflow facts share the ordinary rollout checkpoint transaction.
use super::thread_history_error;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;
use codex_app_server_protocol::WorkflowLifecycleEvent;

pub(super) async fn insert(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    thread_id: &str,
    ordinal: i64,
    event: WorkflowLifecycleEvent,
) -> ThreadStoreResult<()> {
    if event.owner_thread_id != thread_id {
        return Err(ThreadStoreError::InvalidRequest {
            message: "workflow event belongs to another thread".into(),
        });
    }
    insert_event(transaction, thread_id, ordinal, event).await
}

pub(super) async fn insert_projected(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    thread_id: &str,
    owner_thread_id: codex_protocol::ThreadId,
    ordinal: i64,
    proof: super::workflow_origin::ProvenWorkflowEvent,
) -> ThreadStoreResult<()> {
    if proof.thread_id() != owner_thread_id {
        return Err(ThreadStoreError::InvalidRequest {
            message: "workflow projection proof belongs to another thread".into(),
        });
    }
    let event = proof.into_event();
    if event.owner_thread_id == thread_id {
        insert(transaction, thread_id, ordinal, event).await
    } else {
        insert_event(transaction, thread_id, ordinal, event).await
    }
}

async fn insert_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    thread_id: &str,
    ordinal: i64,
    event: WorkflowLifecycleEvent,
) -> ThreadStoreResult<()> {
    let json = serde_json::to_string(&event).map_err(thread_history_error)?;
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT event_json FROM thread_workflow_events WHERE thread_id = ? AND event_id = ?",
    )
    .bind(thread_id)
    .bind(&event.id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(thread_history_error)?;
    if let Some(existing) = existing {
        if existing != json {
            return Err(ThreadStoreError::Conflict {
                message: "workflow event ID has conflicting immutable content".into(),
            });
        }
        return Ok(());
    }
    sqlx::query("INSERT INTO thread_workflow_events (thread_id,event_id,rollout_ordinal,event_json) VALUES (?,?,?,?)")
        .bind(thread_id).bind(&event.id).bind(ordinal).bind(json).execute(&mut **transaction).await.map_err(thread_history_error)?;
    Ok(())
}
