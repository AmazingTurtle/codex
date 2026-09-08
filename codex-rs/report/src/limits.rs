use chrono::DateTime;
use chrono::SecondsFormat;

use crate::LimitRecord;
use crate::rollout_wire::MinimalRateLimitSnapshot;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct HistoricalLimitKey {
    thread_id: String,
    timestamp: String,
    limit_id: String,
    window_slot: u8,
    window_seconds: Option<u64>,
    used_percent_bits: u64,
    resets_at: Option<String>,
}

pub(super) struct HistoricalLimit {
    pub(super) key: HistoricalLimitKey,
    pub(super) record: LimitRecord,
}

pub(super) fn historical_limits(
    timestamp: &str,
    thread_id: Option<&str>,
    snapshot: MinimalRateLimitSnapshot,
) -> (Vec<HistoricalLimit>, u64) {
    let limit_id = snapshot.limit_id.unwrap_or_else(|| "codex".to_string());
    let mut limits = Vec::new();
    let mut malformed = 0;
    for (window_slot, window) in [snapshot.primary, snapshot.secondary]
        .into_iter()
        .enumerate()
        .filter_map(|(slot, window)| window.map(|window| (slot as u8, window)))
    {
        let Some(used_percent) = window.used_percent.as_f64() else {
            malformed += 1;
            continue;
        };
        if !used_percent.is_finite() || used_percent < 0.0 {
            malformed += 1;
            continue;
        }
        let window_seconds = window
            .window_minutes
            .and_then(|minutes| u64::try_from(minutes).ok())
            .and_then(|minutes| minutes.checked_mul(60));
        let resets_at = window.resets_at.and_then(|timestamp| {
            DateTime::from_timestamp(timestamp, 0)
                .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Millis, true))
        });
        limits.push(HistoricalLimit {
            key: HistoricalLimitKey {
                thread_id: thread_id.unwrap_or("unknown").to_string(),
                timestamp: timestamp.to_string(),
                limit_id: limit_id.clone(),
                window_slot,
                window_seconds,
                used_percent_bits: used_percent.to_bits(),
                resets_at: resets_at.clone(),
            },
            record: LimitRecord {
                timestamp: timestamp.to_string(),
                account_id: "unknown".to_string(),
                limit_id: limit_id.clone(),
                window_seconds,
                used_percent: Some(used_percent),
                resets_at,
                source: "rollout".to_string(),
            },
        });
    }
    (limits, malformed)
}
