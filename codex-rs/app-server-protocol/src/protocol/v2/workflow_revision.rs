//! Whole-program revisions fence native admission before replaying a safe prefix.
use super::WorkflowControlState;
use super::WorkflowMetadata;
use super::WorkflowResult;
use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowRevisionStatus {
    Draft,
    Fencing,
    Draining,
    Ready,
    Active,
    Superseded,
    Blocked,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowRevisionApplyMode {
    #[default]
    DrainAccepted,
    InterruptAndApply,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRevision {
    pub id: String,
    pub parent_id: Option<String>,
    pub number: u32,
    pub created_at: i64,
    pub execution_ids: Vec<String>,
    pub source: String,
    pub input: serde_json::Value,
    pub metadata: WorkflowMetadata,
    pub reusable_statements: u32,
    pub statement_count: u32,
    pub status: WorkflowRevisionStatus,
    pub invalidation_reason: String,
    pub mode: Option<WorkflowRevisionApplyMode>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRevisionStageParams {
    pub thread_id: String,
    pub workflow_id: String,
    pub command_id: String,
    pub expected_control_revision: u64,
    pub source: String,
    /// Absent reuses the current arguments; value preserves an explicit JSON null.
    pub input: WorkflowResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRevisionApplyParams {
    pub thread_id: String,
    pub workflow_id: String,
    pub command_id: String,
    pub expected_control_revision: u64,
    pub revision_id: String,
    #[ts(optional=nullable)]
    pub mode: Option<WorkflowRevisionApplyMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRevisionResumeParams {
    pub thread_id: String,
    pub workflow_id: String,
    pub command_id: String,
    pub expected_control_revision: u64,
    pub revision_id: String,
}

macro_rules! revision_response {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
        #[serde(rename_all = "camelCase")]
        #[ts(export_to = "v2/")]
        pub struct $name {
            pub revision: WorkflowRevision,
            pub state: WorkflowControlState,
        }
    };
}
revision_response!(WorkflowRevisionStageResponse);
revision_response!(WorkflowRevisionApplyResponse);
revision_response!(WorkflowRevisionResumeResponse);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRevisionListParams {
    pub thread_id: String,
    pub workflow_id: String,
    #[ts(optional=nullable)]
    pub cursor: Option<String>,
    #[ts(optional=nullable)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRevisionListResponse {
    pub data: Vec<WorkflowRevision>,
    pub next_cursor: Option<String>,
    pub state: WorkflowControlState,
}
