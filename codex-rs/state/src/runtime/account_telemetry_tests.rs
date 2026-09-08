use chrono::TimeZone;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;

use super::*;
use crate::runtime::test_support::unique_temp_dir;

async fn runtime(home: &std::path::Path) -> anyhow::Result<std::sync::Arc<StateRuntime>> {
    StateRuntime::init(
        crate::SqliteConfig::new_for_testing(home.abs()),
        "test-provider".to_string(),
    )
    .await
}

#[tokio::test]
async fn concurrent_limit_writes_are_deduplicated_and_sorted() -> anyhow::Result<()> {
    let home = unique_temp_dir();
    let first_runtime = runtime(&home).await?;
    let second_runtime = runtime(&home).await?;
    let early = AccountLimitObservation {
        captured_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        account_id: "account-b".to_string(),
        limit_id: "codex".to_string(),
        window_seconds: Some(18_000),
        used_percent: Some(20.0),
        resets_at: None,
        source: "account/rateLimits/readMany".to_string(),
    };
    let late = AccountLimitObservation {
        captured_at: Utc.timestamp_opt(1_700_000_060, 0).unwrap(),
        account_id: "account-a".to_string(),
        limit_id: "codex".to_string(),
        window_seconds: None,
        used_percent: None,
        resets_at: Some(Utc.timestamp_opt(1_700_003_600, 0).unwrap()),
        source: "account/rateLimits/readMany".to_string(),
    };

    let first_observations = [late.clone(), early.clone()];
    let second_observations = [early.clone()];
    let (first, second) = tokio::join!(
        first_runtime.record_account_limit_observations(&first_observations),
        second_runtime.record_account_limit_observations(&second_observations),
    );
    first?;
    second?;

    assert_eq!(
        first_runtime.list_account_limit_observations().await?,
        vec![early, late]
    );
    let reader =
        AccountTelemetryReader::open(&crate::SqliteConfig::new_for_testing(home.as_path().abs()))
            .await?;
    assert_eq!(
        reader.list_account_limit_observations().await?,
        first_runtime.list_account_limit_observations().await?
    );
    Ok(())
}

#[tokio::test]
async fn read_only_reader_treats_pre_telemetry_schema_as_empty() -> anyhow::Result<()> {
    let home = unique_temp_dir();
    std::fs::create_dir_all(&home)?;
    let sqlite = crate::SqliteConfig::new_for_testing(home.as_path().abs());
    let pool = sqlite.open_read_write_pool(&sqlite.state_db_path()).await?;
    sqlx::query("CREATE TABLE legacy_state (id INTEGER PRIMARY KEY)")
        .execute(&pool)
        .await?;
    pool.close().await;

    let reader = AccountTelemetryReader::open(&sqlite).await?;

    assert_eq!(reader.list_account_limit_observations().await?, Vec::new());
    assert_eq!(reader.list_usage_reset_events().await?, Vec::new());
    Ok(())
}

#[tokio::test]
async fn reset_idempotency_survives_concurrent_runtimes() -> anyhow::Result<()> {
    let home = unique_temp_dir();
    let first_runtime = runtime(&home).await?;
    let second_runtime = runtime(&home).await?;
    let event = UsageResetEvent {
        consumed_at: Utc.timestamp_opt(1_700_000_060, 0).unwrap(),
        account_id: "account-a".to_string(),
        idempotency_key: Some("reset-once".to_string()),
        limit_id: Some("credit-1".to_string()),
    };

    let (first, second) = tokio::join!(
        first_runtime.record_usage_reset_event(&event),
        second_runtime.record_usage_reset_event(&event),
    );
    first?;
    second?;

    assert_eq!(first_runtime.list_usage_reset_events().await?, vec![event]);
    Ok(())
}

#[tokio::test]
async fn poll_claim_is_shared_and_recovers_from_clock_rollback() -> anyhow::Result<()> {
    let home = unique_temp_dir();
    let first_runtime = runtime(&home).await?;
    let second_runtime = runtime(&home).await?;
    let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
    let interval = chrono::Duration::minutes(5);

    let (first, second) = tokio::join!(
        first_runtime.try_claim_account_limit_poll(now, interval),
        second_runtime.try_claim_account_limit_poll(now, interval),
    );
    assert_eq!(
        [first?, second?]
            .into_iter()
            .filter(|claimed| *claimed)
            .count(),
        1
    );
    assert!(
        !first_runtime
            .try_claim_account_limit_poll(now + chrono::Duration::seconds(299), interval)
            .await?
    );
    assert!(
        first_runtime
            .try_claim_account_limit_poll(now + interval, interval)
            .await?
    );
    assert!(
        first_runtime
            .try_claim_account_limit_poll(now - chrono::Duration::hours(1), interval)
            .await?
    );
    Ok(())
}
