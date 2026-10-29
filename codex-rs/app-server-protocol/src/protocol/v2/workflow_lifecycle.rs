//! Durable human-facing workflow history, separate from model input.
use super::WorkflowMetrics;
use super::WorkflowStatus;
use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
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
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
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
        status: WorkflowStatus,
        result: WorkflowResult,
        error: Option<String>,
        resolved_model: Option<String>,
        reasoning_effort: Option<String>,
        #[serde(default)]
        active_duration_ms: Option<u64>,
    },
    Completed {
        name: String,
        status: WorkflowStatus,
        result: WorkflowResult,
        error: Option<String>,
        metrics: WorkflowMetrics,
    },
    #[ts(rename_all = "camelCase")]
    Interrupted {
        name: String,
        reason: String,
        native_settled: bool,
        metrics: WorkflowMetrics,
    },
    ResultShown {
        name: String,
        status: WorkflowStatus,
        result: WorkflowResult,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowResult {
    Absent,
    Value { value: Value },
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowCompletionMode {
    #[default]
    Display,
    Continue,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowLifecycleChangedNotification {
    pub thread_id: String,
    pub event: WorkflowLifecycleEvent,
    #[serde(default)]
    pub final_message: Option<super::WorkflowAgentMessageProvenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowResultDisplayParams {
    pub thread_id: String,
    pub workflow_id: String,
    #[ts(optional=nullable)]
    pub execution_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowResultDisplayResponse {
    pub event: WorkflowLifecycleEvent,
}

impl From<codex_protocol::workflow::WorkflowLifecycleEvent> for WorkflowLifecycleEvent {
    fn from(event: codex_protocol::workflow::WorkflowLifecycleEvent) -> Self {
        use codex_protocol::workflow as core;
        let status = |status| match status {
            core::WorkflowLifecycleStatus::Pending => WorkflowStatus::Pending,
            core::WorkflowLifecycleStatus::Running => WorkflowStatus::Running,
            core::WorkflowLifecycleStatus::Succeeded => WorkflowStatus::Succeeded,
            core::WorkflowLifecycleStatus::Failed => WorkflowStatus::Failed,
            core::WorkflowLifecycleStatus::Stopped => WorkflowStatus::Stopped,
            core::WorkflowLifecycleStatus::Interrupted => WorkflowStatus::Interrupted,
        };
        let result = |result| match result {
            core::WorkflowResult::Absent => WorkflowResult::Absent,
            core::WorkflowResult::Value { value } => WorkflowResult::Value { value },
        };
        let metrics = |metrics: core::WorkflowLifecycleMetrics| WorkflowMetrics {
            usage: metrics.usage.map(|usage| super::WorkflowTokenUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cached_input_tokens: usage.cached_input_tokens,
                total_tokens: usage.total_tokens,
            }),
            usage_complete: metrics.usage_complete,
            usage_incomplete_reason: metrics.usage_incomplete_reason,
            active_duration_ms: metrics.active_duration_ms,
            wall_duration_ms: metrics.wall_duration_ms,
            model_turns: metrics.model_turns,
            agents_used: metrics.agents_used,
        };
        let kind = match event.kind {
            core::WorkflowLifecycleKind::Started { name, description } => {
                WorkflowLifecycleKind::Started { name, description }
            }
            core::WorkflowLifecycleKind::StepSkipped {
                step_id,
                logical_id,
                label,
                phase,
                command_id,
                result: output,
            } => WorkflowLifecycleKind::StepSkipped {
                step_id,
                logical_id,
                label,
                phase,
                command_id,
                result: result(output),
            },
            core::WorkflowLifecycleKind::StepCompleted {
                step_id,
                attempt_id,
                child_thread_id,
                owned_turn_ids,
                final_turn_id,
                label,
                phase,
                status: state,
                result: output,
                error,
                resolved_model,
                reasoning_effort,
                active_duration_ms,
            } => WorkflowLifecycleKind::StepCompleted {
                step_id,
                attempt_id,
                child_thread_id,
                owned_turn_ids,
                final_turn_id,
                label,
                phase,
                status: status(state),
                result: result(output),
                error,
                resolved_model,
                reasoning_effort,
                active_duration_ms,
            },
            core::WorkflowLifecycleKind::Completed {
                name,
                status: state,
                result: output,
                error,
                metrics: snapshot,
            } => WorkflowLifecycleKind::Completed {
                name,
                status: status(state),
                result: result(output),
                error,
                metrics: metrics(snapshot),
            },
            core::WorkflowLifecycleKind::Interrupted {
                name,
                reason,
                native_settled,
                metrics: snapshot,
            } => WorkflowLifecycleKind::Interrupted {
                name,
                reason,
                native_settled,
                metrics: metrics(snapshot),
            },
            core::WorkflowLifecycleKind::ResultShown {
                name,
                status: state,
                result: output,
                error,
            } => WorkflowLifecycleKind::ResultShown {
                name,
                status: status(state),
                result: result(output),
                error,
            },
        };
        Self {
            id: event.id,
            owner_thread_id: event.owner_thread_id,
            run_id: event.run_id,
            execution_id: event.execution_id,
            workflow_name: event.workflow_name,
            completion_mode: match event.completion_mode {
                core::WorkflowCompletionMode::Display => WorkflowCompletionMode::Display,
                core::WorkflowCompletionMode::Continue => WorkflowCompletionMode::Continue,
            },
            sequence: event.sequence,
            created_at: event.created_at,
            kind,
        }
    }
}
