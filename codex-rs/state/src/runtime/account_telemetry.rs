use anyhow::Context;
use chrono::DateTime;
use chrono::Utc;
use sqlx::Row;
use sqlx::SqlitePool;
use std::time::Duration as StdDuration;

use super::StateRuntime;
use crate::AccountLimitObservation;
use crate::SqliteConfig;
use crate::UsageResetEvent;

const UNKNOWN_WINDOW_SECONDS: i64 = -1;

/// Read-only access to account telemetry without initializing or migrating runtime databases.
#[derive(Clone)]
pub struct AccountTelemetryReader {
    pool: SqlitePool,
    has_limit_observations: bool,
    has_reset_events: bool,
}

impl AccountTelemetryReader {
    pub async fn open(sqlite: &SqliteConfig) -> anyhow::Result<Self> {
        let pool = sqlite
            .open_read_only_pool(
                &sqlite.state_db_path(),
                Some(StdDuration::from_secs(/*secs*/ 5)),
            )
            .await?;
        let tables = sqlx::query_scalar::<_, String>(
            "SELECT name FROM sqlite_schema
             WHERE type = 'table'
               AND name IN ('account_limit_observations', 'usage_reset_events')",
        )
        .fetch_all(&pool)
        .await?;
        Ok(Self {
            pool,
            has_limit_observations: tables
                .iter()
                .any(|table| table == "account_limit_observations"),
            has_reset_events: tables.iter().any(|table| table == "usage_reset_events"),
        })
    }

    pub async fn list_account_limit_observations(
        &self,
    ) -> anyhow::Result<Vec<AccountLimitObservation>> {
        if !self.has_limit_observations {
            return Ok(Vec::new());
        }
        list_account_limit_observations(&self.pool).await
    }

    pub async fn list_usage_reset_events(&self) -> anyhow::Result<Vec<UsageResetEvent>> {
        if !self.has_reset_events {
            return Ok(Vec::new());
        }
        list_usage_reset_events(&self.pool).await
    }
}

impl StateRuntime {
    /// Atomically claims the next shared account-limit poll interval.
    ///
    /// A stored deadline more than one interval ahead is treated as stale clock state. This keeps
    /// a backward system-clock adjustment from suppressing collection indefinitely.
    pub async fn try_claim_account_limit_poll(
        &self,
        now: DateTime<Utc>,
        interval: chrono::Duration,
    ) -> anyhow::Result<bool> {
        let interval_seconds = interval.num_seconds();
        anyhow::ensure!(interval_seconds > 0, "poll interval must be positive");
        let next_deadline = now
            .checked_add_signed(interval)
            .context("account limit poll deadline is out of range")?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let stored_deadline = sqlx::query_scalar::<_, i64>(
            "SELECT next_poll_at FROM account_telemetry_poll_schedule WHERE singleton = 1",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let should_claim = stored_deadline.is_none_or(|stored_deadline| {
            stored_deadline <= now.timestamp() || stored_deadline > next_deadline.timestamp()
        });
        if should_claim {
            sqlx::query(
                "INSERT INTO account_telemetry_poll_schedule (singleton, next_poll_at)
                 VALUES (1, ?)
                 ON CONFLICT(singleton) DO UPDATE SET next_poll_at = excluded.next_poll_at",
            )
            .bind(next_deadline.timestamp())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(should_claim)
    }

    /// Appends limit observations, coalescing identical reads from concurrent processes.
    pub async fn record_account_limit_observations(
        &self,
        observations: &[AccountLimitObservation],
    ) -> anyhow::Result<()> {
        if observations.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for observation in observations {
            let window_seconds = observation
                .window_seconds
                .map(i64::try_from)
                .transpose()
                .context("account limit window exceeds SQLite integer range")?
                .unwrap_or(UNKNOWN_WINDOW_SECONDS);
            sqlx::query(
                "INSERT OR IGNORE INTO account_limit_observations (
                    captured_at, account_id, limit_id, window_seconds, used_percent, resets_at, source
                 ) VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(observation.captured_at.timestamp())
            .bind(&observation.account_id)
            .bind(&observation.limit_id)
            .bind(window_seconds)
            .bind(observation.used_percent)
            .bind(observation.resets_at.map(|value| value.timestamp()))
            .bind(&observation.source)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Lists normalized observations in stable chronological order.
    pub async fn list_account_limit_observations(
        &self,
    ) -> anyhow::Result<Vec<AccountLimitObservation>> {
        list_account_limit_observations(self.pool.as_ref()).await
    }

    /// Records a reset-credit consumption exactly once per account and idempotency key.
    pub async fn record_usage_reset_event(&self, event: &UsageResetEvent) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO usage_reset_events (consumed_at, account_id, idempotency_key, limit_id)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(account_id, idempotency_key) WHERE idempotency_key IS NOT NULL
             DO UPDATE SET
                 consumed_at = MIN(consumed_at, excluded.consumed_at),
                 limit_id = COALESCE(limit_id, excluded.limit_id)",
        )
        .bind(event.consumed_at.timestamp())
        .bind(&event.account_id)
        .bind(&event.idempotency_key)
        .bind(&event.limit_id)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    /// Lists reset-credit consumptions in stable chronological order.
    pub async fn list_usage_reset_events(&self) -> anyhow::Result<Vec<UsageResetEvent>> {
        list_usage_reset_events(self.pool.as_ref()).await
    }
}

async fn list_account_limit_observations(
    pool: &SqlitePool,
) -> anyhow::Result<Vec<AccountLimitObservation>> {
    let rows = sqlx::query(
        "SELECT captured_at, account_id, limit_id, window_seconds, used_percent, resets_at, source
         FROM account_limit_observations
         ORDER BY captured_at ASC, account_id ASC, limit_id ASC, window_seconds ASC, id ASC",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let window_seconds = row.try_get::<i64, _>("window_seconds")?;
            Ok(AccountLimitObservation {
                captured_at: timestamp(row.try_get("captured_at")?, "captured_at")?,
                account_id: row.try_get("account_id")?,
                limit_id: row.try_get("limit_id")?,
                window_seconds: (window_seconds != UNKNOWN_WINDOW_SECONDS)
                    .then(|| u64::try_from(window_seconds))
                    .transpose()
                    .context("stored account limit window is negative")?,
                used_percent: row.try_get("used_percent")?,
                resets_at: row
                    .try_get::<Option<i64>, _>("resets_at")?
                    .map(|value| timestamp(value, "resets_at"))
                    .transpose()?,
                source: row.try_get("source")?,
            })
        })
        .collect()
}

async fn list_usage_reset_events(pool: &SqlitePool) -> anyhow::Result<Vec<UsageResetEvent>> {
    let rows = sqlx::query(
        "SELECT consumed_at, account_id, idempotency_key, limit_id
         FROM usage_reset_events
         ORDER BY consumed_at ASC, account_id ASC, id ASC",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(UsageResetEvent {
                consumed_at: timestamp(row.try_get("consumed_at")?, "consumed_at")?,
                account_id: row.try_get("account_id")?,
                idempotency_key: row.try_get("idempotency_key")?,
                limit_id: row.try_get("limit_id")?,
            })
        })
        .collect()
}

fn timestamp(value: i64, column: &str) -> anyhow::Result<DateTime<Utc>> {
    DateTime::from_timestamp(value, 0)
        .with_context(|| format!("stored {column} timestamp is out of range: {value}"))
}

#[cfg(test)]
#[path = "account_telemetry_tests.rs"]
mod tests;
