use pretty_assertions::assert_eq;

use super::*;
use crate::ConfigLayerEntry;
use crate::config_toml::ConfigToml;
use codex_utils_absolute_path::AbsolutePathBuf;

#[test]
fn parses_absent_and_explicit_empty_whitelists_distinctly() {
    let absent: ConfigToml = toml::from_str("[debloat]\nenabled = true").unwrap();
    let empty: ConfigToml = toml::from_str("[debloat]\nenabled = true\nwhitelist = []").unwrap();

    assert_eq!(
        DebloatPolicy::from_config(absent.debloat.as_ref()).has_explicit_whitelist(),
        false
    );
    assert_eq!(
        DebloatPolicy::from_config(empty.debloat.as_ref()).has_explicit_whitelist(),
        true
    );
}

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
fn default_policy_keeps_custom_plugins_user_mcp_and_non_bundled_skills() {
    let policy = DebloatPolicy::from_config(Some(&DebloatConfigToml {
        enabled: true,
        whitelist: None,
    }));

    assert!(!policy.allows_plugin("browser@openai-bundled", true));
    assert!(policy.allows_plugin("custom@personal", false));
    assert!(!policy.allows_mcp("product", false));
    assert!(policy.allows_mcp("user", true));
    assert!(!policy.allows_standalone_skill("imagegen", true));
    assert!(policy.allows_standalone_skill("personal", false));
}

#[test]
fn explicit_whitelist_is_strict_and_deduplicated() {
    let config: ConfigToml =
        toml::from_str("[debloat]\nenabled = true\nwhitelist = [\"mcp.kept\", \"mcp.kept\"]")
            .unwrap();
    let policy = DebloatPolicy::from_config(config.debloat.as_ref());

    assert!(policy.allows_mcp("kept", false));
    assert!(!policy.allows_mcp("other", true));
    assert!(!policy.allows_plugin("custom@personal", false));
    assert!(!policy.allows_standalone_skill("personal", false));
}

#[test]
fn user_controlled_mcp_names_only_include_supported_layers() {
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
            layer(
                ConfigLayerSource::Project {
                    dot_codex_folder: base.join("repo/.codex"),
                },
                "project",
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
        user_controlled_mcp_server_names(&stack),
        HashSet::from([
            "project".to_string(),
            "session".to_string(),
            "user".to_string(),
        ])
    );
}
