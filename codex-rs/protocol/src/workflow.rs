//! Durable workflow presentation facts. These are never model input.
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowLifecycleEvent {
    pub id: String,
    pub owner_thread_id: String,
    pub run_id: String,
    pub execution_id: String,
    pub workflow_name: String,
    pub completion_mode: WorkflowCompletionMode,
    pub sequence: u64,
    pub created_at: i64,
    pub kind: WorkflowLifecycleKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(tag = "type", rename_all = "camelCase")]
pub enum WorkflowLifecycleKind {
    Started {
        name: String,
        description: String,
    },
    #[ts(rename_all = "camelCase")]
    StepSkipped {
        step_id: String,
        logical_id: String,
        label: String,
        phase: Option<String>,
        command_id: String,
        result: WorkflowResult,
    },
    #[ts(rename_all = "camelCase")]
    StepCompleted {
        step_id: String,
        attempt_id: String,
        child_thread_id: String,
        owned_turn_ids: Vec<String>,
        final_turn_id: Option<String>,
        label: String,
        phase: Option<String>,
        status: WorkflowLifecycleStatus,
        result: WorkflowResult,
        error: Option<String>,
        resolved_model: Option<String>,
        reasoning_effort: Option<String>,
        #[serde(default)]
        active_duration_ms: Option<u64>,
    },
    Completed {
        name: String,
        status: WorkflowLifecycleStatus,
        result: WorkflowResult,
        error: Option<String>,
        metrics: WorkflowLifecycleMetrics,
    },
    #[ts(rename_all = "camelCase")]
    Interrupted {
        name: String,
        reason: String,
        native_settled: bool,
        metrics: WorkflowLifecycleMetrics,
    },
    ResultShown {
        name: String,
        status: WorkflowLifecycleStatus,
        result: WorkflowResult,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum WorkflowLifecycleStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Stopped,
    Interrupted,
}

/// An absent result differs from a successful JSON null.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase")]
pub enum WorkflowResult {
    Absent,
    Value { value: Value },
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum WorkflowCompletionMode {
    #[default]
    Display,
    Continue,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowLifecycleMetrics {
    pub usage: Option<WorkflowLifecycleTokenUsage>,
    pub usage_complete: bool,
    pub usage_incomplete_reason: Option<String>,
    pub active_duration_ms: Option<u64>,
    pub wall_duration_ms: Option<u64>,
    #[serde(default)]
    pub model_turns: Option<u64>,
    #[serde(default)]
    pub agents_used: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowLifecycleTokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub total_tokens: u64,
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
