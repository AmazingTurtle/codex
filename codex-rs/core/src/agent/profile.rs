//! Applies a named user profile without changing the parent session's authority.

use super::role::DEFAULT_ROLE_NAME;
use super::role::resolve_role_config;
use crate::config::Config;
use crate::config::ConfigOverrides;
use crate::config::load_config_toml_with_layer_stack;
use crate::config::resolve_profile_v2_config_path;
use crate::session::session::Session;
use crate::thread_manager::build_models_manager;
use codex_config::ConfigLayerEntry;
use codex_config::ConfigLayerSource;
use codex_config::ConfigLayerStack;
use codex_config::LoaderOverrides;
use codex_exec_server::LOCAL_FS;
use codex_features::Feature;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::protocol::SessionSource;
use codex_thread_store::StoredThread;
use codex_utils_output_truncation::approx_token_count;

const MAX_PROFILE_IDENTIFIER_CHARS: usize = 64;
const MAX_PROFILE_HINT_BYTES: usize = 1024;
const MAX_PROFILE_INSTRUCTION_TOKENS: usize = 9_800;

pub(crate) async fn apply_role_profile(
    config: &mut Config,
    role_name: Option<&str>,
) -> Result<bool, String> {
    let role_name = role_name.unwrap_or(DEFAULT_ROLE_NAME);
    let Some(role) = resolve_role_config(config, role_name).cloned() else {
        return Ok(false);
    };
    let Some(profile) = role.profile.clone() else {
        return Ok(false);
    };
    let result = async {
        let path = resolve_profile_v2_config_path(&config.codex_home, &profile);
        if !tokio::fs::try_exists(&path).await? {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{} does not exist", path.display())));
        }
        // Reuse profile-v2 validation and path resolution. Only the selected user
        // layer is projected onto the parent's already captured configuration.
        let loaded = load_config_toml_with_layer_stack(
            &config.codex_home,
            /*cwd*/ None,
            Vec::new(),
            LoaderOverrides {
                user_config_path: Some(path),
                user_config_profile: Some(profile.clone()),
                ignore_project_config: true,
                ignore_managed_requirements: true,
                ..Default::default()
            },
        ).await?;
        let profile_layer = loaded.config_layer_stack.all_layers_low_to_high()
            .find(|layer| matches!(&layer.name, ConfigLayerSource::User { profile: Some(name), .. } if name == profile.as_str()))
            .ok_or_else(|| std::io::Error::other("profile layer was not loaded"))?;
        let mut layers = config.config_layer_stack.all_layers_low_to_high().cloned().collect::<Vec<_>>();
        let layer = ConfigLayerEntry::new(ConfigLayerSource::SessionFlags, profile_layer.config.clone());
        layers.insert(layers.partition_point(|existing| existing.name <= layer.name), layer);
        let stack = ConfigLayerStack::new(layers, config.config_layer_stack.requirements().clone(), config.config_layer_stack.requirements_toml().clone())?
            .with_user_and_project_exec_policy_rules_ignored(config.config_layer_stack.ignore_user_and_project_exec_policy_rules());
        let cfg = stack.effective_config().try_into().map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        let mut next = Config::load_config_with_layer_stack(
            LOCAL_FS.as_ref(), cfg,
            ConfigOverrides {
                cwd: Some(config.cwd.to_path_buf()),
                approval_policy: Some(config.permissions.approval_policy.value()),
                approvals_reviewer: Some(config.approvals_reviewer),
                permission_profile: Some(config.permissions.permission_profile().clone()),
                workspace_roots: Some(config.permissions.workspace_roots().to_vec()),
                codex_self_exe: config.codex_self_exe.clone(),
                codex_linux_sandbox_exe: config.codex_linux_sandbox_exe.clone(),
                main_execve_wrapper_exe: config.main_execve_wrapper_exe.clone(),
                ..Default::default()
            }, config.codex_home.clone(), stack,
        ).await?;
        if profile_layer.config.get("model_provider").is_none()
            && profile_layer.config.get("model_providers").and_then(toml::Value::as_table).is_none_or(|providers| !providers.contains_key(&config.model_provider_id)) {
            next.model_provider_id.clone_from(&config.model_provider_id);
            next.model_provider.clone_from(&config.model_provider);
        }
        if next.model_provider != config.model_provider && profile_layer.config.get("model_catalog_json").is_none() {
            next.model_catalog = None;
        } else if profile_layer.config.get("model_catalog_json").is_none() {
            next.model_catalog.clone_from(&config.model_catalog);
        }
        if profile_layer.config.get("model").is_none() { next.model.clone_from(&config.model); }
        if profile_layer.config.get("model_reasoning_effort").is_none() { next.model_reasoning_effort.clone_from(&config.model_reasoning_effort); }
        if profile_layer.config.get("model_reasoning_summary").is_none() { next.model_reasoning_summary = config.model_reasoning_summary; }
        if profile_layer.config.get("developer_instructions").is_none() { next.developer_instructions.clone_from(&config.developer_instructions); }
        if next.base_instructions.is_none()
            && config.base_instructions_provenance == Some(codex_protocol::models::BaseInstructionsProvenance::Custom) {
            next.base_instructions.clone_from(&config.base_instructions);
            next.base_instructions_provenance.clone_from(&config.base_instructions_provenance);
        }
        for (name, instructions) in [("model instructions", next.base_instructions.as_deref()), ("developer instructions", next.developer_instructions.as_deref())] {
            if instructions.is_some_and(|text| approx_token_count(text) > MAX_PROFILE_INSTRUCTION_TOKENS) {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("profile {name} exceed {MAX_PROFILE_INSTRUCTION_TOKENS} estimated tokens")));
            }
        }
        next.permissions = config.permissions.clone();
        next.approvals_reviewer = config.approvals_reviewer;
        next.application_network_policy = config.application_network_policy.clone();
        next.agents_enabled = config.agents_enabled;
        next.agent_max_threads = config.agent_max_threads;
        next.agent_max_depth = config.agent_max_depth;
        next.multi_agent_v2.max_concurrent_threads_per_session = config.multi_agent_v2.max_concurrent_threads_per_session;
        let backend = profile_layer.config.get("features").and_then(|features| features.get("multi_agent_v2"));
        let disables_descendants = backend.and_then(toml::Value::as_bool) == Some(false)
            || backend.and_then(|value| value.get("enabled")).and_then(toml::Value::as_bool) == Some(false);
        next.features.set_enabled(Feature::MultiAgentV2, config.features.enabled(Feature::MultiAgentV2) && !disables_descendants).map_err(std::io::Error::other)?;
        for feature in [Feature::NetworkProxy, Feature::GuardianApproval] {
            next.features.set_enabled(feature, config.features.enabled(feature)).map_err(std::io::Error::other)?;
        }
        next.agent_roles.insert(role_name.to_string(), role.clone());
        // The agent graph and persistence belong to the root, not the selected profile.
        next.experimental_thread_store = config.experimental_thread_store.clone();
        next.sqlite = config.sqlite.clone();
        *config = next;
        Ok(())
    }.await;
    result.map_err(|err: std::io::Error| {
        format!("Could not load agent profile `{profile}`: {err}")
    })?;
    Ok(true)
}

/// Disabled profile workers retain their parent's protocol lineage for durable reload,
/// but cannot create descendants or expose collaboration tools of their own.
pub(crate) fn is_leaf_worker(config: &Config, source: &SessionSource) -> bool {
    !config.features.enabled(Feature::MultiAgentV2)
        && source
            .get_agent_role()
            .as_deref()
            .and_then(|role| config.agent_roles.get(role))
            .is_some_and(|role| role.profile.is_some())
}

/// Reload profile defaults while preserving the child's recorded provider and effort.
pub(crate) async fn apply_role_profile_for_resume(
    config: &mut Config,
    role_name: Option<&str>,
    stored: &StoredThread,
) -> Result<bool, String> {
    // Older agents without a recorded role must not acquire a newly configured default profile.
    let Some(role_name) = role_name else {
        return Ok(false);
    };
    if !apply_role_profile(config, Some(role_name)).await? {
        return Ok(false);
    }
    if config.model_provider_id != stored.model_provider {
        return Err(format!(
            "Cannot resume agent profile: it no longer selects the recorded provider `{}`",
            stored.model_provider
        ));
    }
    config
        .model_reasoning_effort
        .clone_from(&stored.reasoning_effort);
    Ok(true)
}

/// Reuse the current catalog only when it describes the child's provider and settings.
pub(crate) async fn models_manager_for_config(
    session: &Session,
    config: &Config,
) -> SharedModelsManager {
    let parent = session.get_config().await;
    if parent.model_provider == config.model_provider
        && parent.model_catalog == config.model_catalog
    {
        return session.services.models_manager.clone();
    }
    let manager = build_models_manager(config, session.services.auth_manager.clone());
    manager
        .list_models(
            RefreshStrategy::OnlineIfUncached,
            config.http_client_factory(),
        )
        .await;
    manager
}

/// Profile hints are deliberately bounded and contain only user-facing identifiers.
pub(crate) fn profiles_description(config: &Config) -> String {
    let mut description = String::new();
    for (role_name, role) in &config.agent_roles {
        let Some(profile) = &role.profile else {
            continue;
        };
        let path = resolve_profile_v2_config_path(&config.codex_home, profile);
        let profile_config = std::fs::read_to_string(path)
            .ok()
            .and_then(|contents| toml::from_str::<toml::Value>(&contents).ok());
        let model = profile_config
            .as_ref()
            .and_then(|value| value.get("model"))
            .and_then(toml::Value::as_str)
            .or(config.model.as_deref())
            .unwrap_or("inherited");
        let provider = profile_config
            .as_ref()
            .and_then(|value| value.get("model_provider"))
            .and_then(toml::Value::as_str)
            .unwrap_or(&config.model_provider_id);
        let role_name = role_name
            .chars()
            .take(MAX_PROFILE_IDENTIFIER_CHARS)
            .collect::<String>();
        let profile = profile
            .chars()
            .take(MAX_PROFILE_IDENTIFIER_CHARS)
            .collect::<String>();
        let provider = provider
            .chars()
            .take(MAX_PROFILE_IDENTIFIER_CHARS)
            .collect::<String>();
        let model = model
            .chars()
            .take(MAX_PROFILE_IDENTIFIER_CHARS)
            .collect::<String>();
        let hint = format!(
            "\n- `{role_name}` uses profile `{profile}`: provider `{provider}`, default model `{model}`. Selecting this role uses its profile defaults; model overrides are validated against its catalog. The listed model overrides describe the parent provider. When changing provider, use fork_turns=\"none\"."
        );
        if description.len() + hint.len() > MAX_PROFILE_HINT_BYTES {
            break;
        }
        description.push_str(&hint);
    }
    description
}
