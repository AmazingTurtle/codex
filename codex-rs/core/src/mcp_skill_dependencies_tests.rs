use std::collections::HashMap;
use std::sync::Arc;

use codex_config::load_global_mcp_servers;
use codex_protocol::protocol::SkillScope;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;

fn skill_with_stdio_mcp_dependency(
    name: &str,
    command: &str,
    state_dir: &std::path::Path,
) -> SkillMetadata {
    SkillMetadata {
        name: name.to_string(),
        description: "controlled skill".to_string(),
        short_description: None,
        interface: None,
        dependencies: Some(codex_skills::SkillDependencies {
            tools: vec![SkillToolDependency {
                r#type: "mcp".to_string(),
                value: name.to_string(),
                description: None,
                transport: Some("stdio".to_string()),
                command: Some(command.to_string()),
                url: None,
                oauth_callback_port: None,
            }],
        }),
        policy: None,
        path_to_skills_md: AbsolutePathBuf::from_absolute_path(state_dir.join(format!("{name}.md")))
            .expect("fixture skill path"),
        scope: SkillScope::Repo,
        plugin_id: None,
        remote_plugin_id: None,
    }
}

#[tokio::test]
async fn installs_only_dependencies_in_the_authorized_prompt_set() {
    let state = tempfile::tempdir().expect("create skill fixture");
    let denied = skill_with_stdio_mcp_dependency(
        "previously-denied",
        "codex-mcp-dependency-test-denied",
        state.path(),
    );
    let newly_approved = skill_with_stdio_mcp_dependency(
        "newly-approved",
        "codex-mcp-dependency-test-approved",
        state.path(),
    );
    let skills = vec![denied, newly_approved];
    let collected = collect_missing_mcp_dependencies(&skills, &HashMap::new());
    let denied_config = collected
        .get("previously-denied")
        .expect("denied candidate must exist");
    let denied_key = canonical_mcp_server_key("previously-denied", denied_config);
    let (session, mut turn_context) = crate::session::tests::make_session_and_context().await;
    session.record_mcp_dependency_prompted([denied_key]).await;

    let authorized = filter_prompted_mcp_dependencies(&session, &collected).await;
    assert_eq!(
        vec!["newly-approved".to_string()],
        sorted_server_names(&authorized)
    );

    let mut config = turn_context.config.as_ref().clone();
    config.codex_home = AbsolutePathBuf::from_absolute_path(state.path().join("codex-home"))
        .expect("isolated Codex config path");
    std::fs::create_dir_all(config.codex_home.as_path()).expect("create isolated Codex config");
    let mut features = codex_features::Features::with_defaults();
    features.enable(codex_features::Feature::SkillMcpDependencyInstall);
    config.features = crate::config::ManagedFeatures::from(features);
    turn_context.config = Arc::new(config.clone());

    maybe_install_mcp_dependencies(&session, &turn_context, &config, authorized, None).await;

    let persisted = load_global_mcp_servers(config.codex_home.as_path())
        .await
        .expect("read installed MCP dependencies");
    assert_eq!(
        vec!["newly-approved".to_string()],
        sorted_server_names(&persisted)
    );
}

fn sorted_server_names(servers: &HashMap<String, McpServerConfig>) -> Vec<String> {
    let mut names = servers.keys().cloned().collect::<Vec<_>>();
    names.sort();
    names
}
