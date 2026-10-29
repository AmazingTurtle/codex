//! Bounded actual worker chat, selected by durable workflow ownership.
use super::ThreadItemEntry;
use crate::JsonSchema;
use crate::TS;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowTranscriptTarget {
    Run,
    Phase {
        phase: String,
    },
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Step {
        step_id: String,
    },
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Attempt {
        step_id: String,
        attempt_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowTranscriptListParams {
    pub thread_id: String,
    pub workflow_id: String,
    pub target: WorkflowTranscriptTarget,
    #[ts(optional=nullable)]
    pub execution_id: Option<String>,
    #[ts(optional=nullable)]
    pub cursor: Option<String>,
    #[ts(optional=nullable)]
    /// Bounds durable items per page. An initial current-execution page may also
    /// include bounded live items with stable IDs; clients deduplicate those IDs.
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowTranscriptGroup {
    pub run_id: String,
    pub execution_id: String,
    pub step_id: String,
    pub attempt_id: String,
    pub child_thread_id: String,
    pub label: String,
    pub phase: Option<String>,
    pub owned_turn_ids: Vec<String>,
    pub resolved_model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub cwd: AbsolutePathBuf,
    pub final_turn_id: Option<String>,
    pub final_item_id: Option<String>,
    pub final_result: Option<WorkflowTranscriptResult>,
    pub items: Vec<ThreadItemEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowTranscriptResult {
    Value { value: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowTranscriptListResponse {
    pub data: Vec<WorkflowTranscriptGroup>,
    pub next_cursor: Option<String>,
    pub limitation: Option<String>,
}
