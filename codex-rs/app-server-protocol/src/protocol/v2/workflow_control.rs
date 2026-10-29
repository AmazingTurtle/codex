//! Experimental durable commands; labels are presentation, never target identity.
use super::WorkflowResult;
use super::WorkflowRevisionStatus;
use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowControlTarget {
    Run,
    Phase {
        title: String,
    },
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Step {
        logical_id: String,
        expected_attempt_id: Option<String>,
        expected_turn_id: Option<String>,
    },
    FutureStep {
        scope: String,
        key: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowControlOperation {
    Guide { message: String },
    EditQueued { prompt: String },
    Skip { replacement: WorkflowResult },
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowControlParams {
    pub thread_id: String,
    pub workflow_id: String,
    pub command_id: String,
    pub expected_control_revision: u64,
    pub target: WorkflowControlTarget,
    pub operation: WorkflowControlOperation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowControlCommandStatus {
    Pending,
    Applied,
    Rejected,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowControlCommand {
    pub id: String,
    pub revision_id: String,
    pub control_revision: u64,
    pub created_at: i64,
    pub target: WorkflowControlTarget,
    pub operation: WorkflowControlOperation,
    pub status: WorkflowControlCommandStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowControlState {
    pub control_revision: u64,
    pub active_revision_id: String,
    pub pending_revision_id: Option<String>,
    pub status: WorkflowRevisionStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowControlResponse {
    pub command: WorkflowControlCommand,
    pub state: WorkflowControlState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowControlListParams {
    pub thread_id: String,
    pub workflow_id: String,
    /// Hydrate one captured step's current owned turn, independently of command pages.
    #[ts(optional=nullable)]
    pub active_target: Option<WorkflowControlTarget>,
    #[ts(optional=nullable)]
    pub cursor: Option<String>,
    #[ts(optional=nullable)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowControlListResponse {
    pub data: Vec<WorkflowControlCommand>,
    pub next_cursor: Option<String>,
    pub state: WorkflowControlState,
    /// Exact live targets only; empty when native ownership cannot be verified.
    #[serde(default)]
    pub active_targets: Vec<WorkflowControlActiveTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowControlActiveTarget {
    pub logical_id: String,
    pub attempt_id: String,
    pub turn_id: String,
}
