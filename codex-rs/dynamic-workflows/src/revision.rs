//! Immutable plans share one run's admission budget and append-only execution history.

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::ScriptMetadata;
use crate::WorkflowResult;

pub const MAX_WORKFLOW_REVISIONS: usize = 128;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RevisionApplyMode {
    #[default]
    DrainAccepted,
    InterruptAndApply,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowRevisionStatus {
    Draft,
    Fencing,
    Draining,
    Ready,
    #[default]
    Active,
    Superseded,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionStageRequest {
    pub command_id: String,
    pub expected_control_revision: u64,
    pub source: String,
    /// Absent preserves the prior arguments; Value preserves explicit JSON null.
    #[serde(default = "absent_input")]
    pub input: WorkflowResult,
}

fn absent_input() -> WorkflowResult {
    WorkflowResult::Absent
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionApplyRequest {
    pub command_id: String,
    pub expected_control_revision: u64,
    pub revision_id: String,
    #[serde(default)]
    pub mode: RevisionApplyMode,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionResumeRequest {
    pub command_id: String,
    pub expected_control_revision: u64,
    pub revision_id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRevision {
    pub id: String,
    pub parent_id: Option<String>,
    pub number: u32,
    pub created_at: i64,
    pub source: String,
    pub input: Value,
    pub source_hash: String,
    pub metadata: ScriptMetadata,
    pub reusable_statements: u32,
    pub invalidation_reason: String,
    pub statement_count: u32,
    pub status: WorkflowRevisionStatus,
    pub mode: Option<RevisionApplyMode>,
    pub error: Option<String>,
    #[serde(default)]
    pub execution_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRevisionResponse {
    pub revision: WorkflowRevision,
    pub state: crate::steering::WorkflowControlState,
}

/// Facts captured at the scheduling fence; uncertainty is never interpreted as rejection.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowFenceReceipt {
    pub accepted_attempt_ids: Vec<String>,
    pub uncertain_attempt_ids: Vec<String>,
}

/// The native layer validates the exact child and owned turn before cancellation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowInterruptReceipt {
    pub child_thread_id: String,
    pub turn_id: String,
    pub settled: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WorkflowPreparedDisposition {
    KnownRejected,
    Accepted { child_thread_id: String },
    Uncertain,
}
