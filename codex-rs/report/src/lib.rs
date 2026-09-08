//! Local, offline telemetry report generation from persisted Codex data.

mod extract;
mod html;
mod limits;
mod model;
mod rollout_reader;
mod rollout_wire;
mod usage_values;

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use chrono::SecondsFormat;
use codex_state::AccountTelemetryReader;
use codex_state::SqliteConfig;
use codex_utils_absolute_path::AbsolutePathBuf;

pub use extract::extract_report_data;
pub use html::render_report_html;
pub use model::Diagnostics;
pub use model::LimitRecord;
pub use model::ReportData;
pub use model::ResetRecord;
pub use model::UsageRecord;
pub use model::UsageSource;
pub use model::typescript_contract;

/// Generates a retained, self-contained HTML report from the supplied Codex home.
pub async fn generate_report(codex_home: &Path) -> Result<PathBuf> {
    let codex_home = codex_home.to_path_buf();
    let extraction_home = codex_home.clone();
    let mut report = tokio::task::spawn_blocking(move || {
        std::fs::read_dir(extraction_home.as_path())
            .with_context(|| format!("read Codex home {}", extraction_home.display()))?;
        Ok::<_, anyhow::Error>(extract_report_data(extraction_home.as_path()))
    })
    .await
    .context("join report extraction task")??;
    match read_account_telemetry(codex_home.as_path()).await {
        Ok(Some((limits, resets))) => {
            report.limits.extend(limits);
            report.limits.sort_by(|left, right| {
                left.timestamp
                    .cmp(&right.timestamp)
                    .then_with(|| left.account_id.cmp(&right.account_id))
                    .then_with(|| left.limit_id.cmp(&right.limit_id))
                    .then_with(|| left.window_seconds.cmp(&right.window_seconds))
            });
            report.resets = resets;
        }
        Ok(None) => {}
        Err(_) => report.diagnostics.telemetry_read_errors += 1,
    }
    tokio::task::spawn_blocking(move || write_report(report))
        .await
        .context("join report writer task")?
}

fn write_report(report: ReportData) -> Result<PathBuf> {
    let html = render_report_html(&report)?;
    let report_dir = tempfile::Builder::new()
        .prefix("better-codex-report-")
        .tempdir()
        .context("create report directory")?;
    let report_path = report_dir.path().join("report.html");
    std::fs::write(report_path.as_path(), html)
        .with_context(|| format!("write telemetry report to {}", report_path.display()))?;
    Ok(report_dir.keep().join("report.html"))
}

async fn read_account_telemetry(
    codex_home: &Path,
) -> Result<Option<(Vec<LimitRecord>, Vec<ResetRecord>)>> {
    let codex_home = AbsolutePathBuf::from_absolute_path(codex_home)
        .context("resolve Codex home for telemetry")?;
    let sqlite = SqliteConfig::from_sqlite_home(codex_home);
    if !sqlite.state_db_path().is_file() {
        return Ok(None);
    }
    let reader = AccountTelemetryReader::open(&sqlite).await?;
    let limits = reader
        .list_account_limit_observations()
        .await?
        .into_iter()
        .map(|observation| LimitRecord {
            timestamp: observation
                .captured_at
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            account_id: observation.account_id,
            limit_id: observation.limit_id,
            window_seconds: observation.window_seconds,
            used_percent: observation.used_percent,
            resets_at: observation
                .resets_at
                .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)),
            source: observation.source,
        })
        .collect();
    let resets = reader
        .list_usage_reset_events()
        .await?
        .into_iter()
        .map(|event| ResetRecord {
            timestamp: event
                .consumed_at
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            account_id: event.account_id,
            idempotency_key: event.idempotency_key,
            limit_id: event.limit_id,
        })
        .collect();
    Ok(Some((limits, resets)))
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
