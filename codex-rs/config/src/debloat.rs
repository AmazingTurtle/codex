use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use serde::de::Error as SerdeError;

use crate::ConfigLayerSource;
use crate::ConfigLayerStack;

const PLUGIN_PREFIX: &str = "plugin.";
const MCP_PREFIX: &str = "mcp.";
const SKILL_PREFIX: &str = "skill.";

/// Configuration for suppressing optional capability providers at startup.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DebloatConfigToml {
    /// Enables debloat filtering. Defaults to `false`.
    #[serde(default)]
    pub enabled: bool,

    /// Explicit allowlist for non-repository capabilities. When debloat is enabled,
    /// absence and an explicitly empty list both disable every plugin and every
    /// non-repository MCP server and standalone skill.
    pub whitelist: Option<Vec<DebloatIdentifier>>,
}

impl TryFrom<toml::Value> for DebloatConfigToml {
    type Error = toml::de::Error;

    fn try_from(value: toml::Value) -> Result<Self, Self::Error> {
        Self::deserialize(value)
    }
}

/// A namespaced capability identifier accepted by the debloat allowlist.
#[derive(Debug, Clone, PartialEq, Eq, Hash, JsonSchema)]
#[schemars(transparent)]
pub struct DebloatIdentifier(#[schemars(with = "String")] String);

impl DebloatIdentifier {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn parse(value: String) -> Result<Self, String> {
        if let Some(plugin_id) = value.strip_prefix(PLUGIN_PREFIX) {
            validate_plugin_id(plugin_id)?;
        } else if let Some(name) = value.strip_prefix(MCP_PREFIX) {
            validate_name(name, "MCP server")?;
        } else if let Some(name) = value.strip_prefix(SKILL_PREFIX) {
            validate_name(name, "skill")?;
        } else {
            return Err(format!(
                "invalid debloat identifier `{value}`; expected `plugin.<plugin>@<marketplace>`, `mcp.<name>`, or `skill.<name>`"
            ));
        }
        Ok(Self(value))
    }
}

impl fmt::Display for DebloatIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for DebloatIdentifier {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DebloatIdentifier {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Effective debloat policy derived from the merged configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct DebloatPolicy {
    enabled: bool,
    whitelist: Option<BTreeSet<String>>,
}

/// Provenance used when applying debloat to a directly configured capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebloatCapabilitySource {
    /// A capability loaded directly from repository configuration or discovery.
    Repository,
    /// A capability loaded from outside the repository, including user and bundled sources.
    External,
}

impl DebloatPolicy {
    pub fn from_config(config: Option<&DebloatConfigToml>) -> Self {
        let Some(config) = config else {
            return Self::default();
        };
        Self {
            enabled: config.enabled,
            whitelist: config.whitelist.as_ref().map(|identifiers| {
                identifiers
                    .iter()
                    .map(|identifier| identifier.as_str().to_string())
                    .collect()
            }),
        }
    }

    pub fn from_layer_stack(stack: &ConfigLayerStack) -> Self {
        let effective_config = stack.effective_config();
        let config = effective_config
            .get("debloat")
            .cloned()
            .map(DebloatConfigToml::try_from)
            .transpose();
        match config {
            Ok(config) => Self::from_config(config.as_ref()),
            Err(error) => {
                tracing::warn!("failed to resolve debloat config from layer stack: {error}");
                Self::default()
            }
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn allows_plugin(&self, plugin_id: &str) -> bool {
        self.allows(PLUGIN_PREFIX, plugin_id, DebloatCapabilitySource::External)
    }

    pub fn allows_mcp(&self, name: &str, source: DebloatCapabilitySource) -> bool {
        self.allows(MCP_PREFIX, name, source)
    }

    pub fn allows_standalone_skill(&self, name: &str, source: DebloatCapabilitySource) -> bool {
        self.allows(SKILL_PREFIX, name, source)
    }

    /// Returns whether discovery needs to inspect non-repository standalone skill roots.
    pub fn allows_external_skill_discovery(&self) -> bool {
        !self.enabled
            || self.whitelist.as_ref().is_some_and(|whitelist| {
                whitelist
                    .iter()
                    .any(|identifier| identifier.starts_with(SKILL_PREFIX))
            })
    }

    fn allows(&self, prefix: &str, name: &str, source: DebloatCapabilitySource) -> bool {
        !self.enabled
            || source == DebloatCapabilitySource::Repository
            || self
                .whitelist
                .as_ref()
                .is_some_and(|whitelist| whitelist.contains(&format!("{prefix}{name}")))
    }
}

/// Returns MCP server names whose effective definition comes from project configuration.
pub fn repository_mcp_server_names(stack: &ConfigLayerStack) -> HashSet<String> {
    let mut sources = HashMap::new();
    for layer in stack.layers_low_to_high() {
        let repository = matches!(layer.name, ConfigLayerSource::Project { .. });
        let Some(servers) = layer
            .config
            .get("mcp_servers")
            .and_then(toml::Value::as_table)
        else {
            continue;
        };
        sources.extend(servers.keys().cloned().map(|name| (name, repository)));
    }
    sources
        .into_iter()
        .filter_map(|(name, repository)| repository.then_some(name))
        .collect()
}

fn validate_plugin_id(plugin_id: &str) -> Result<(), String> {
    let Some((plugin_name, marketplace_name)) = plugin_id.rsplit_once('@') else {
        return Err(format!(
            "invalid debloat plugin identifier `plugin.{plugin_id}`; expected `plugin.<plugin>@<marketplace>`"
        ));
    };
    validate_plugin_segment(plugin_name, "plugin name", true)?;
    validate_plugin_segment(marketplace_name, "marketplace name", false)
}

fn validate_plugin_segment(value: &str, kind: &str, allow_dots: bool) -> Result<(), String> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.starts_with('.')
        || value.ends_with('.')
        || value.contains("..")
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '-' | '_')
                || allow_dots && character == '.'
        })
    {
        return Err(format!("invalid debloat {kind} `{value}`"));
    }
    Ok(())
}

fn validate_name(value: &str, kind: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(format!("invalid debloat {kind} identifier `{value}`"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "debloat_tests.rs"]
mod tests;
