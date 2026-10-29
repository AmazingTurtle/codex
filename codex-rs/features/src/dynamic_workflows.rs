use crate::FeatureConfig;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Persistent policy for launching scripted agent coordination.
/// This policy never grants permission to delegate beyond user and repository instructions.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DynamicWorkflowsMode {
    /// Let the model choose when authorized scripted coordination is useful.
    #[default]
    Auto,
    /// Require a human skill invocation or an explicitly selected saved workflow.
    Explicit,
    /// Prevent new launches while retaining control of existing runs.
    Off,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DynamicWorkflowsConfigToml {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<DynamicWorkflowsMode>,
}

impl FeatureConfig for DynamicWorkflowsConfigToml {
    fn enabled(&self) -> Option<bool> {
        self.enabled
    }
}
