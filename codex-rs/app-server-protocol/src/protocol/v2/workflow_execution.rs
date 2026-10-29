//! Immutable execution inspection never starts or resumes a workflow.
use super::WorkflowCompletionMode;
use super::WorkflowMetadata;
use super::WorkflowMetrics;
use super::WorkflowResult;
use super::WorkflowStatus;
use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowExecutionReadParams {
    pub thread_id: String,
    pub workflow_id: String,
    pub execution_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowExecutionSnapshot {
    pub run_id: String,
    pub execution_id: String,
    pub revision_id: Option<String>,
    pub source: String,
    pub input: serde_json::Value,
    pub metadata: Option<WorkflowMetadata>,
    pub completion_mode: WorkflowCompletionMode,
    pub status: WorkflowStatus,
    pub partial: bool,
    pub result: WorkflowResult,
    pub error: Option<String>,
    pub metrics: WorkflowMetrics,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowExecutionReadResponse {
    pub execution: WorkflowExecutionSnapshot,
}
