use std::borrow::Cow;

use crate::sqlite::SqliteWriteQueryExt;
use sqlx::migrate::Migrator;
use sqlx_sqlite::SqlitePool;

// Reserve downstream migration numbers above upstream's sequential range.
// Keep the SQL unchanged so released telemetry checksums remain recognizable.
const ACCOUNT_TELEMETRY_MIGRATION_VERSION: i64 = 1_000_000_001;

pub(crate) static STATE_MIGRATOR: Migrator = sqlx_macros::migrate!("./migrations");
pub(crate) static LOGS_MIGRATOR: Migrator = sqlx_macros::migrate!("./logs_migrations");
pub(crate) static GOALS_MIGRATOR: Migrator = sqlx_macros::migrate!("./goals_migrations");
pub(crate) static MEMORIES_MIGRATOR: Migrator = sqlx_macros::migrate!("./memory_migrations");
pub(crate) static QUEUE_MIGRATOR: Migrator = sqlx_macros::migrate!("./queue_migrations");
pub(crate) static THREAD_HISTORY_MIGRATOR: Migrator =
    sqlx_macros::migrate!("./thread_history_migrations");

/// Allow an older Codex binary to open a database that has already been
/// migrated by a newer binary running in parallel.
///
/// We intentionally ignore applied migration versions that are newer than the
/// embedded migration set. Known migration versions are still validated by
/// checksum, so this only relaxes the "database is ahead of me" case.
fn runtime_migrator(base: &'static Migrator) -> Migrator {
    Migrator {
        migrations: Cow::Borrowed(base.migrations.as_ref()),
        ignore_missing: true,
        locking: base.locking,
        no_tx: base.no_tx,
        table_name: base.table_name.clone(),
        create_schemas: base.create_schemas.clone(),
    }
}

pub(crate) fn runtime_state_migrator() -> Migrator {
    runtime_migrator(&STATE_MIGRATOR)
}

pub(crate) fn runtime_logs_migrator() -> Migrator {
    runtime_migrator(&LOGS_MIGRATOR)
}

pub(crate) fn runtime_goals_migrator() -> Migrator {
    runtime_migrator(&GOALS_MIGRATOR)
}

pub(crate) fn runtime_memories_migrator() -> Migrator {
    runtime_migrator(&MEMORIES_MIGRATOR)
}

pub(crate) fn runtime_queue_migrator() -> Migrator {
    runtime_migrator(&QUEUE_MIGRATOR)
}

// The paginated history projector will call this when it takes ownership of opening the database.
#[allow(dead_code)]
pub(crate) fn runtime_thread_history_migrator() -> Migrator {
    runtime_migrator(&THREAD_HISTORY_MIGRATOR)
}

pub(crate) async fn repair_legacy_migration_versions(
    pool: &SqlitePool,
    migrator: &Migrator,
) -> anyhow::Result<()> {
    repair_legacy_migration_version(
        pool, migrator, /*legacy_version*/ 38, /*current_version*/ 39,
    )
    .await?;
    for legacy_version in [53, 55, 56] {
        repair_legacy_migration_version(
            pool,
            migrator,
            legacy_version,
            ACCOUNT_TELEMETRY_MIGRATION_VERSION,
        )
        .await?;
    }
    Ok(())
}

async fn repair_legacy_migration_version(
    pool: &SqlitePool,
    migrator: &Migrator,
    legacy_version: i64,
    current_version: i64,
) -> anyhow::Result<()> {
    let Some(current_migration) = migrator
        .migrations
        .iter()
        .find(|migration| migration.version == current_version)
    else {
        return Ok(());
    };
    let migrations_table_exists = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?
    .is_some();
    if !migrations_table_exists {
        return Ok(());
    }

    let legacy_migration_needs_repair = sqlx::query_scalar::<_, i64>(
        r#"
SELECT 1
FROM _sqlx_migrations
WHERE version = ?
  AND checksum = ?
  AND NOT EXISTS (
      SELECT 1 FROM _sqlx_migrations WHERE version = ?
  )
        "#,
    )
    .bind(legacy_version)
    .bind(current_migration.checksum.as_ref())
    .bind(current_migration.version)
    .fetch_optional(pool)
    .await?
    .is_some();
    if !legacy_migration_needs_repair {
        return Ok(());
    }

    sqlx::query(
        r#"
UPDATE _sqlx_migrations
SET version = ?, description = ?
WHERE version = ?
  AND checksum = ?
  AND NOT EXISTS (
      SELECT 1 FROM _sqlx_migrations WHERE version = ?
  )
        "#,
    )
    .bind(current_migration.version)
    .bind(current_migration.description.as_ref())
    .bind(legacy_version)
    .bind(current_migration.checksum.as_ref())
    .bind(current_migration.version)
    .execute_with_busy_retry(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "migrations_tests.rs"]
mod tests;
