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

    /// Explicit capability allowlist. Absence selects the default debloat policy;
    /// an explicitly empty list disables every plugin, MCP server, and standalone skill.
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

    pub fn has_explicit_whitelist(&self) -> bool {
        self.enabled && self.whitelist.is_some()
    }

    pub fn allows_plugin(&self, plugin_id: &str, openai_managed: bool) -> bool {
        self.allows(PLUGIN_PREFIX, plugin_id, !openai_managed)
    }

    pub fn allows_mcp(&self, name: &str, user_controlled: bool) -> bool {
        self.allows(MCP_PREFIX, name, user_controlled)
    }

    pub fn allows_standalone_skill(&self, name: &str, bundled: bool) -> bool {
        self.allows(SKILL_PREFIX, name, !bundled)
    }

    fn allows(&self, prefix: &str, name: &str, default_when_enabled: bool) -> bool {
        if !self.enabled {
            return true;
        }
        match &self.whitelist {
            Some(whitelist) => whitelist.contains(&format!("{prefix}{name}")),
            None => default_when_enabled,
        }
    }
}

/// Returns MCP server names explicitly supplied by user-controlled config layers.
pub fn user_controlled_mcp_server_names(stack: &ConfigLayerStack) -> HashSet<String> {
    let mut sources = HashMap::new();
    for layer in stack.layers_low_to_high() {
        let user_controlled = matches!(
            layer.name,
            ConfigLayerSource::User { .. }
                | ConfigLayerSource::Project { .. }
                | ConfigLayerSource::SessionFlags
        );
        let Some(servers) = layer
            .config
            .get("mcp_servers")
            .and_then(toml::Value::as_table)
        else {
            continue;
        };
        sources.extend(servers.keys().cloned().map(|name| (name, user_controlled)));
    }
    sources
        .into_iter()
        .filter_map(|(name, user_controlled)| user_controlled.then_some(name))
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
