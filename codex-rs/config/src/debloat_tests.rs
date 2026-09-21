use pretty_assertions::assert_eq;

use super::*;
use crate::ConfigLayerEntry;
use crate::config_toml::ConfigToml;
use codex_utils_absolute_path::AbsolutePathBuf;

#[test]
fn rejects_malformed_identifiers() {
    for identifier in ["plugin.browser", "mcp.", "skill.two words", "other.value"] {
        let config = format!("[debloat]\nwhitelist = [\"{identifier}\"]");
        assert!(
            toml::from_str::<ConfigToml>(&config).is_err(),
            "{identifier}"
        );
    }
}

#[test]
fn default_policy_keeps_only_repository_capabilities() {
    for whitelist in [None, Some(Vec::new())] {
        let policy = DebloatPolicy::from_config(Some(&DebloatConfigToml {
            enabled: true,
            whitelist,
        }));

        assert!(!policy.allows_plugin("browser@openai-bundled"));
        assert!(!policy.allows_plugin("custom@personal"));
        assert!(!policy.allows_mcp("user", DebloatCapabilitySource::External));
        assert!(policy.allows_mcp("repo", DebloatCapabilitySource::Repository));
        assert!(!policy.allows_standalone_skill("personal", DebloatCapabilitySource::External));
        assert!(policy.allows_standalone_skill("repo", DebloatCapabilitySource::Repository));
        assert!(!policy.allows_external_skill_discovery());
    }
}

#[test]
fn explicit_whitelist_allows_matching_external_capabilities() {
    let config: ConfigToml = toml::from_str(
        "[debloat]\nenabled = true\nwhitelist = [\"mcp.kept\", \"mcp.kept\", \"skill.kept\", \"plugin.kept@test\"]",
    )
    .unwrap();
    let policy = DebloatPolicy::from_config(config.debloat.as_ref());

    assert!(policy.allows_mcp("kept", DebloatCapabilitySource::External));
    assert!(!policy.allows_mcp("other", DebloatCapabilitySource::External));
    assert!(policy.allows_mcp("other", DebloatCapabilitySource::Repository));
    assert!(policy.allows_plugin("kept@test"));
    assert!(!policy.allows_plugin("custom@personal"));
    assert!(policy.allows_standalone_skill("kept", DebloatCapabilitySource::External));
    assert!(!policy.allows_standalone_skill("personal", DebloatCapabilitySource::External));
    assert!(policy.allows_external_skill_discovery());
}

#[test]
fn repository_mcp_names_only_include_effective_project_definitions() {
    let base = AbsolutePathBuf::from_absolute_path(std::env::current_dir().unwrap())
        .expect("current directory should be absolute");
    let layer = |name, server: &str| {
        ConfigLayerEntry::new(
            name,
            toml::from_str(&format!("[mcp_servers.{server}]\ncommand = \"true\"")).unwrap(),
        )
    };
    let stack = ConfigLayerStack::new(
        vec![
            layer(
                ConfigLayerSource::PackagedDefaults {
                    file: base.join("package/config.toml"),
                },
                "packaged",
            ),
            layer(
                ConfigLayerSource::User {
                    file: base.join("user/config.toml"),
                    profile: None,
                },
                "overridden",
            ),
            layer(
                ConfigLayerSource::User {
                    file: base.join("user/config.toml"),
                    profile: None,
                },
                "user",
            ),
            ConfigLayerEntry::new(
                ConfigLayerSource::Project {
                    dot_codex_folder: base.join("repo/.codex"),
                },
                toml::from_str(
                    "[mcp_servers.project]\ncommand = \"true\"\n[mcp_servers.overridden]\ncommand = \"true\"",
                )
                .unwrap(),
            ),
            layer(ConfigLayerSource::SessionFlags, "session"),
            layer(
                ConfigLayerSource::LegacyManagedConfigTomlFromFile {
                    file: base.join("system/config.toml"),
                },
                "overridden",
            ),
        ],
        Default::default(),
        Default::default(),
    )
    .unwrap();

    assert_eq!(
        repository_mcp_server_names(&stack),
        HashSet::from(["project".to_string()])
    );
}
