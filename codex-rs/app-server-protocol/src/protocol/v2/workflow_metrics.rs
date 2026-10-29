//! Shared bounded workflow declarations and authoritative progress snapshots.
use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowPhaseDefinition {
    pub title: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowMetadata {
    pub name: String,
    pub description: String,
    pub phases: Vec<WorkflowPhaseDefinition>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRunLimits {
    pub max_agents: u32,
    pub max_tokens: Option<u64>,
}
impl Default for WorkflowRunLimits {
    fn default() -> Self {
        Self {
            max_agents: 32,
            max_tokens: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowPhaseStatus {
    #[default]
    Pending,
    Active,
    Completed,
    Skipped,
    Failed,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowPhaseProgress {
    pub title: String,
    pub status: WorkflowPhaseStatus,
    pub steps_discovered: u32,
    pub steps_completed: u32,
    #[serde(default)]
    pub metrics: WorkflowMetrics,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub completed_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowTokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Included in inputTokens; it must not be added to input a second time.
    pub cached_input_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowMetrics {
    /// Null means no reported usage checkpoint is available; zero is an observed zero.
    pub usage: Option<WorkflowTokenUsage>,
    pub usage_complete: bool,
    #[serde(default)]
    pub usage_incomplete_reason: Option<String>,
    pub active_duration_ms: Option<u64>,
    pub wall_duration_ms: Option<u64>,
    /// Unique usage-bearing model responses; null means the count is unknown.
    #[serde(default)]
    pub model_turns: Option<u64>,
    #[serde(default)]
    pub agents_used: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowLog {
    pub sequence: u64,
    pub message: String,
    pub phase: Option<String>,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowAdmissionStatus {
    #[default]
    NotRequested,
    Reserved,
    Accepted,
    Rejected,
    Uncertain,
}

pub(super) fn legacy_format_version() -> u32 {
    1
}
pub(super) fn legacy_read_only() -> bool {
    true
}
