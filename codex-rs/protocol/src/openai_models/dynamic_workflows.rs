use super::ToolMessage;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// Optional wording for scripted agent coordination. Missing/null values use bundled defaults.
#[derive(Debug, Default, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema)]
pub struct DynamicWorkflowToolMessages {
    /// Developer guidance, bounded to 512 UTF-8 bytes; oversized values use the default.
    /// An empty value omits optional prose; runtime policy is always retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<ToolMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ToolMessage>,
}

#[cfg(test)]
#[path = "dynamic_workflows_tests.rs"]
mod tests;
