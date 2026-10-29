//! Experimental durable JavaScript workflows and reusable script libraries.

use crate::JsonSchema;
use crate::TS;
use codex_utils_path_uri::LegacyAppPathString;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
#[path = "workflow_metrics.rs"]
mod metrics;
pub use metrics::*;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Stopped,
    Interrupted,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRun {
    pub id: String,
    #[serde(default)]
    pub execution_id: String,
    #[serde(default = "metrics::legacy_format_version")]
    pub format_version: u32,
    #[serde(default = "metrics::legacy_read_only")]
    pub read_only: bool,
    #[serde(default)]
    pub metadata: Option<WorkflowMetadata>,
    #[serde(default)]
    pub limits: WorkflowRunLimits,
    #[serde(default)]
    pub accepted_starts: u32,
    #[serde(default)]
    pub phase_progress: Vec<WorkflowPhaseProgress>,
    #[serde(default)]
    pub metrics: WorkflowMetrics,
    #[serde(default)]
    pub logs: Vec<WorkflowLog>,
    pub owner_thread_id: String,
    pub project_root: Option<LegacyAppPathString>,
    pub source: String,
    pub input: Value,
    pub status: WorkflowStatus,
    pub phase: Option<String>,
    pub phases: Vec<String>,
    pub steps: BTreeMap<String, WorkflowStep>,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowStep {
    pub id: String,
    #[serde(default)]
    pub logical_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub disposition: WorkflowStepDisposition,
    #[serde(default)]
    pub control_outcome: Option<WorkflowStepControlOutcome>,
    #[serde(default)]
    pub effective_options: Option<Value>,
    pub phase: Option<String>,
    pub options: Value,
    pub attempts: Vec<WorkflowAttempt>,
    pub result: Option<Value>,
    #[serde(default)]
    pub metrics: WorkflowMetrics,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub completed_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowStepDisposition {
    #[default]
    Native,
    Skipped,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowStepControlOutcome {
    pub command_id: String,
    pub revision_id: String,
    pub replacement: super::WorkflowResult,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowAttempt {
    pub id: String,
    /// The execution that originally admitted this attempt; empty for older records.
    #[serde(default)]
    pub execution_id: String,
    #[serde(default)]
    pub admission: WorkflowAdmissionStatus,
    pub child_thread_id: Option<String>,
    #[serde(default)]
    pub resolved_model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub owned_turn_ids: Vec<String>,
    #[serde(default)]
    pub final_turn_id: Option<String>,
    pub status: WorkflowStatus,
    pub usage: Option<Value>,
    pub error: Option<String>,
    #[serde(default)]
    pub metrics: WorkflowMetrics,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub completed_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowSummary {
    pub id: String,
    #[serde(default)]
    pub execution_id: String,
    #[serde(default = "metrics::legacy_format_version")]
    pub format_version: u32,
    #[serde(default = "metrics::legacy_read_only")]
    pub read_only: bool,
    #[serde(default)]
    pub metadata: Option<WorkflowMetadata>,
    #[serde(default)]
    pub limits: WorkflowRunLimits,
    #[serde(default)]
    pub accepted_starts: u32,
    #[serde(default)]
    pub phase_progress: Vec<WorkflowPhaseProgress>,
    #[serde(default)]
    pub metrics: WorkflowMetrics,
    pub owner_thread_id: String,
    pub project_root: Option<LegacyAppPathString>,
    pub status: WorkflowStatus,
    pub phase: Option<String>,
    pub phase_count: u32,
    pub step_count: u32,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowStartParams {
    pub thread_id: String,
    /// Human display is independent of an explicitly requested parent continuation.
    #[ts(optional = nullable)]
    pub completion_mode: Option<super::WorkflowCompletionMode>,
    /// Supply exactly one of source and savedWorkflowId.
    #[ts(optional = nullable)]
    pub source: Option<String>,
    #[ts(optional = nullable)]
    pub saved_workflow_id: Option<String>,
    #[serde(default = "empty_input")]
    pub input: Value,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowListParams {
    pub thread_id: String,
    /// Defaults to the owning independent conversation.
    #[ts(optional = nullable)]
    pub scope: Option<WorkflowListScope>,
    #[ts(optional = nullable)]
    pub cursor: Option<String>,
    #[ts(optional = nullable)]
    pub limit: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowListScope {
    Current,
    Project,
    All,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowListResponse {
    pub data: Vec<WorkflowSummary>,
    pub next_cursor: Option<String>,
}

macro_rules! workflow_request {
    ($params:ident, $response:ident) => {
        #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
        #[serde(rename_all = "camelCase")]
        #[ts(export_to = "v2/")]
        pub struct $params {
            pub thread_id: String,
            pub workflow_id: String,
        }

        #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
        #[serde(rename_all = "camelCase")]
        #[ts(export_to = "v2/")]
        pub struct $response {
            pub workflow: WorkflowRun,
        }
    };
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowStartResponse {
    pub workflow: WorkflowRun,
}

workflow_request!(WorkflowReadParams, WorkflowReadResponse);
workflow_request!(WorkflowStopParams, WorkflowStopResponse);
workflow_request!(WorkflowRetryParams, WorkflowRetryResponse);
workflow_request!(WorkflowRecoverParams, WorkflowRecoverResponse);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowScope {
    Project,
    User,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SavedWorkflow {
    pub id: String,
    pub name: String,
    pub scope: WorkflowScope,
    pub source: String,
    #[serde(default)]
    pub metadata: Option<WorkflowMetadata>,
    #[serde(default = "metrics::legacy_read_only")]
    pub read_only: bool,
    #[serde(default)]
    pub read_only_reason: Option<String>,
    #[serde(default = "metrics::legacy_format_version")]
    pub format_version: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SavedWorkflowSummary {
    pub id: String,
    pub name: String,
    pub scope: WorkflowScope,
    #[serde(default)]
    pub metadata: Option<WorkflowMetadata>,
    #[serde(default = "metrics::legacy_read_only")]
    pub read_only: bool,
    #[serde(default)]
    pub read_only_reason: Option<String>,
    #[serde(default = "metrics::legacy_format_version")]
    pub format_version: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowSaveParams {
    pub thread_id: String,
    pub workflow_id: String,
    pub name: String,
    pub scope: WorkflowScope,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowSaveResponse {
    pub saved_workflow: SavedWorkflow,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowSavedListParams {
    pub thread_id: String,
    #[ts(optional = nullable)]
    pub cursor: Option<String>,
    #[ts(optional = nullable)]
    pub limit: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowSavedListResponse {
    pub data: Vec<SavedWorkflowSummary>,
    pub next_cursor: Option<String>,
    /// Host-local library directories for the existing filesystem watch API.
    #[serde(default)]
    pub watch_roots: Vec<LegacyAppPathString>,
}

macro_rules! saved_request {
    ($params:ident) => {
        #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
        #[serde(rename_all = "camelCase")]
        #[ts(export_to = "v2/")]
        pub struct $params {
            pub thread_id: String,
            pub saved_workflow_id: String,
        }
    };
}

saved_request!(WorkflowSavedReadParams);
saved_request!(WorkflowSavedDeleteParams);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowSavedReadResponse {
    pub saved_workflow: SavedWorkflow,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[ts(export_to = "v2/")]
pub struct WorkflowSavedDeleteResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowUpdatedNotification {
    pub thread_id: String,
    pub workflow: WorkflowRun,
}

/// Compact accounting checkpoints; full step details remain in workflow/updated and workflow/read.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowProgressNotification {
    pub thread_id: String,
    pub workflow: WorkflowSummary,
}

fn empty_input() -> Value {
    serde_json::json!({})
}
