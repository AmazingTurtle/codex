use pretty_assertions::assert_eq;

use super::*;

#[test]
fn debloated_plugin_status_and_json_preserve_enabled_state() {
    let entry = PluginListEntry {
        plugin_id: "browser@openai-bundled".to_string(),
        name: "browser".to_string(),
        marketplace_name: "openai-bundled".to_string(),
        version: Some("1.0.0".to_string()),
        display_version: Some("1.0.0".to_string()),
        installed: true,
        enabled: true,
        debloated: true,
        source: JsonPluginSource::Local {
            path: "/plugin".to_string(),
        },
        marketplace_source: None,
        install_policy: "AVAILABLE",
        auth_policy: "ON_USE",
    };

    assert_eq!(plugin_status(&entry), "installed, enabled, debloated");
    let json = serde_json::to_value(entry).expect("plugin entry should serialize");
    assert_eq!(json["enabled"], true);
    assert_eq!(json["debloated"], true);
}
