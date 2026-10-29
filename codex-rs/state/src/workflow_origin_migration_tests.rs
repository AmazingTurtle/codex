//! A released history database upgrades through the same opener used by the CLI.
use super::THREAD_HISTORY_MIGRATOR;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use sqlx::migrate::Migrator;
use sqlx_sqlite::SqlitePool;
use std::borrow::Cow;

async fn projected_rows(pool: &SqlitePool, thread: &str) -> Vec<(String, i64, String)> {
    sqlx::query_as(
        r#"
SELECT 'turn/' || turn_id, rollout_ordinal, status FROM thread_turns WHERE thread_id = ?
UNION ALL SELECT 'item/' || item_id, rollout_ordinal, item_json FROM thread_items WHERE thread_id = ?
UNION ALL SELECT 'workflow/' || event_id, rollout_ordinal, event_json FROM thread_workflow_events WHERE thread_id = ?
ORDER BY 2, 1
        "#,
    )
    .bind(thread).bind(thread).bind(thread)
    .fetch_all(pool).await.expect("read the actual projected history rows")
}

#[tokio::test]
async fn workflow_origin_upgrade_preserves_history_and_checkpoint_and_accepts_older_writers() {
    let home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&home)
        .await
        .expect("create sqlite home");
    let _cleanup = scopeguard::guard(home.clone(), |home| {
        let _ = std::fs::remove_dir_all(home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(home.as_path().abs());
    let before_origins = Migrator {
        migrations: Cow::Owned(
            THREAD_HISTORY_MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 10)
                .cloned()
                .collect(),
        ),
        ignore_missing: true,
        locking: THREAD_HISTORY_MIGRATOR.locking,
        no_tx: THREAD_HISTORY_MIGRATOR.no_tx,
        table_name: THREAD_HISTORY_MIGRATOR.table_name.clone(),
        create_schemas: THREAD_HISTORY_MIGRATOR.create_schemas.clone(),
    };
    let old_pool = sqlite
        .open_thread_history_db(&before_origins, /*telemetry_override*/ None)
        .await
        .expect("open the released pre-origin history schema");
    let thread = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO thread_turns (thread_id,turn_id,rollout_ordinal,status) VALUES (?,'original-turn',10,'completed')")
        .bind(&thread).execute(&old_pool).await.expect("retain the original completed conversation");
    sqlx::query("INSERT INTO thread_items (thread_id,turn_id,item_id,rollout_ordinal,created_at_ms,item_json) VALUES (?,'original-turn','original-item',11,100,?)")
        .bind(&thread).bind(r#"{"type":"agentMessage","id":"original-item","text":"Retained ordinary conversation"}"#)
        .execute(&old_pool).await.expect("retain the original chat item");
    let event = serde_json::json!({"id":"original-report","ownerThreadId":uuid::Uuid::new_v4().to_string(),"runId":"original-run","executionId":"original-execution","workflowName":"Original report","completionMode":"display","sequence":0,"createdAt":1,"kind":{"type":"started","name":"Original report","description":"Inherited original report"}}).to_string();
    sqlx::query("INSERT INTO thread_workflow_events (thread_id,event_id,rollout_ordinal,event_json) VALUES (?,'original-report',12,?)")
        .bind(&thread).bind(event).execute(&old_pool).await.expect("retain immutable inherited workflow history");
    sqlx::query("INSERT INTO thread_history_projection_state (thread_id,next_rollout_byte_offset,next_rollout_ordinal) VALUES (?,128,13)")
        .bind(&thread).execute(&old_pool).await.expect("retain the existing durable projection checkpoint");
    let original = projected_rows(&old_pool, &thread).await;
    old_pool.close().await;

    // The real runtime opener must embed and apply the newly added migration.
    // A stale migrate! expansion fails this column read, rather than passing a
    // static assertion about a migration number or a seeded current database.
    let pool = crate::open_thread_history_db(&sqlite)
        .await
        .expect("upgrade through the public runtime history opener");
    assert_eq!(projected_rows(&pool, &thread).await, original);
    let checkpoint: (i64, i64, Option<String>) = sqlx::query_as("SELECT next_rollout_byte_offset,next_rollout_ordinal,workflow_origin_state FROM thread_history_projection_state WHERE thread_id = ?")
        .bind(&thread).fetch_one(&pool).await.expect("the added nullable origin state is available to projection readers");
    assert_eq!(checkpoint, (128, 13, None));
    let origin = serde_json::json!({"thread_id":thread,"header_seen":true,"prefix_open":false,"expected_source":null,"source_owners":[]}).to_string();
    sqlx::query(
        "UPDATE thread_history_projection_state SET workflow_origin_state = ? WHERE thread_id = ?",
    )
    .bind(&origin)
    .bind(&thread)
    .execute(&pool)
    .await
    .expect("new writers can retain their origin checkpoint");
    pool.close().await;

    let older_pool = sqlite
        .open_thread_history_db(&before_origins, /*telemetry_override*/ None)
        .await
        .expect("an older additive-schema writer still opens the database");
    let older_thread = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO thread_history_projection_state (thread_id,next_rollout_byte_offset,next_rollout_ordinal) VALUES (?,40,2)")
        .bind(&older_thread).execute(&older_pool).await.expect("older writers can omit the new nullable origin field");
    older_pool.close().await;
    let reopened = crate::open_thread_history_db(&sqlite)
        .await
        .expect("reopen the upgraded database without rewriting history");
    assert_eq!(projected_rows(&reopened, &thread).await, original);
    let checkpoint: (i64, i64, Option<String>) = sqlx::query_as("SELECT next_rollout_byte_offset,next_rollout_ordinal,workflow_origin_state FROM thread_history_projection_state WHERE thread_id = ?")
        .bind(&thread).fetch_one(&reopened).await.expect("read the unchanged durable checkpoint");
    assert_eq!(checkpoint, (128, 13, Some(origin)));
    let older_origin: Option<String> = sqlx::query_scalar(
        "SELECT workflow_origin_state FROM thread_history_projection_state WHERE thread_id = ?",
    )
    .bind(&older_thread)
    .fetch_one(&reopened)
    .await
    .expect("read the older writer's new checkpoint");
    assert_eq!(older_origin, None);
    reopened.close().await;
}
