use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;

/// A point-in-time observation of one metered account limit window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountLimitObservation {
    pub captured_at: DateTime<Utc>,
    pub account_id: String,
    pub limit_id: String,
    pub window_seconds: Option<u64>,
    pub used_percent: Option<f64>,
    pub resets_at: Option<DateTime<Utc>>,
    pub source: String,
}

/// A successful consumption of a banked usage-reset credit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageResetEvent {
    pub consumed_at: DateTime<Utc>,
    pub account_id: String,
    pub idempotency_key: Option<String>,
    pub limit_id: Option<String>,
}
