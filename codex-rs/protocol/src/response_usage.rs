//! Per-response usage metadata reported by the upstream service, without aggregation.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

/// Usage metadata reported for one upstream response.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq, TS, JsonSchema)]
pub struct ResponseUsageMetadata {
    pub amount: Option<String>,
    pub metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_service_tier: Option<String>,
}
