//! Presentation proof for an exact native workflow final message, never model input.
use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

#[cfg(test)]
#[path = "workflow_presentation_tests.rs"]
mod tests;

// The enum derive only registers experimental variants, not named variant fields.
// Register this optional presentation field with the existing schema exporter.
inventory::submit! {
    crate::experimental_api::ExperimentalField {
        type_name: "ThreadItem",
        field_name: "workflow",
        reason: "item/agentMessage.workflow",
    }
}

impl crate::experimental_api::ExperimentalApi for super::ThreadItem {
    fn experimental_reason(&self) -> Option<&'static str> {
        match self {
            Self::AgentMessage {
                workflow: Some(_), ..
            } => Some("item/agentMessage.workflow"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowAgentMessageProvenance {
    pub run_id: String,
    pub execution_id: String,
    pub attempt_id: String,
    pub child_thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub format: WorkflowAgentMessageFormat,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowAgentMessageFormat {
    /// A typed string result remains ordinary Markdown, even if it resembles JSON.
    Text,
    /// The original final message was verified against a typed non-string JSON result.
    Json,
}
