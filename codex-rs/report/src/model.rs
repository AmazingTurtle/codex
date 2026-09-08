use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

pub const REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ReportData {
    #[ts(type = "number")]
    pub schema_version: u32,
    pub generated_at: String,
    pub usage: Vec<UsageRecord>,
    pub limits: Vec<LimitRecord>,
    pub resets: Vec<ResetRecord>,
    pub diagnostics: Diagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct UsageRecord {
    pub timestamp: String,
    pub thread_id: String,
    pub response_id: Option<String>,
    pub account_id: Option<String>,
    pub requested_model: Option<String>,
    pub requested_service_tier: Option<String>,
    pub reported_model: Option<String>,
    pub reported_service_tier: Option<String>,
    #[ts(type = "number")]
    pub input_tokens: i64,
    #[ts(type = "number")]
    pub cached_input_tokens: i64,
    #[ts(type = "number")]
    pub cache_write_input_tokens: i64,
    #[ts(type = "number")]
    pub output_tokens: i64,
    #[ts(type = "number")]
    pub reasoning_output_tokens: i64,
    pub source: UsageSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum UsageSource {
    Exact,
    LegacyApproximate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct LimitRecord {
    pub timestamp: String,
    pub account_id: String,
    pub limit_id: String,
    #[ts(type = "number | null")]
    pub window_seconds: Option<u64>,
    pub used_percent: Option<f64>,
    pub resets_at: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ResetRecord {
    pub timestamp: String,
    pub account_id: String,
    pub idempotency_key: Option<String>,
    pub limit_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct Diagnostics {
    #[ts(type = "number")]
    pub files_discovered: u64,
    #[ts(type = "number")]
    pub files_scanned: u64,
    #[ts(type = "number")]
    pub file_read_errors: u64,
    #[ts(type = "number")]
    pub discovery_errors: u64,
    #[ts(type = "number")]
    pub telemetry_read_errors: u64,
    #[ts(type = "number")]
    pub oversized_records: u64,
    #[ts(type = "number")]
    pub records_parsed: u64,
    #[ts(type = "number")]
    pub malformed_records: u64,
    #[ts(type = "number")]
    pub duplicate_usage_records: u64,
    #[ts(type = "number")]
    pub conflicting_usage_records: u64,
    #[ts(type = "number")]
    pub exact_usage_records: u64,
    #[ts(type = "number")]
    pub legacy_usage_records: u64,
}

pub fn typescript_contract() -> String {
    format!(
        "// GENERATED CODE! DO NOT MODIFY BY HAND!\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n{}\n",
        exported(UsageSource::decl()),
        exported(UsageRecord::decl()),
        exported(LimitRecord::decl()),
        exported(ResetRecord::decl()),
        exported(Diagnostics::decl()),
        exported(ReportData::decl()),
    )
}

fn exported(declaration: String) -> String {
    format!("export {declaration}")
}
