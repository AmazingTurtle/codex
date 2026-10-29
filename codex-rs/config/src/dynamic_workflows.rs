use std::num::NonZeroU64;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de::Error;

/// Limits captured when a workflow starts. Native agent capacity still applies.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DynamicWorkflowsConfig {
    /// Maximum accepted agents per run, from 1 through 1,024. Defaults to 32.
    #[serde(deserialize_with = "deserialize_max_agents")]
    #[schemars(range(min = 1, max = 1024))]
    pub max_agents: usize,
    /// Optional token budget, at most JavaScript's largest exact integer.
    /// Omission leaves the budget unbounded.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_max_tokens"
    )]
    #[schemars(range(min = 1, max = 9_007_199_254_740_991_u64))]
    pub max_tokens: Option<NonZeroU64>,
}

impl Default for DynamicWorkflowsConfig {
    fn default() -> Self {
        Self {
            max_agents: 32,
            max_tokens: None,
        }
    }
}

fn deserialize_max_agents<'de, D: Deserializer<'de>>(deserializer: D) -> Result<usize, D::Error> {
    let value = usize::deserialize(deserializer)?;
    if !(1..=1024).contains(&value) {
        return Err(D::Error::custom(
            "dynamic_workflows.max_agents must be between 1 and 1024",
        ));
    }
    Ok(value)
}

fn deserialize_max_tokens<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<NonZeroU64>, D::Error> {
    let value = Option::<NonZeroU64>::deserialize(deserializer)?;
    if value.is_some_and(|value| value.get() > 9_007_199_254_740_991) {
        return Err(D::Error::custom(
            "dynamic_workflows.max_tokens must not exceed 9007199254740991",
        ));
    }
    Ok(value)
}

#[cfg(test)]
#[path = "dynamic_workflows_tests.rs"]
mod tests;
