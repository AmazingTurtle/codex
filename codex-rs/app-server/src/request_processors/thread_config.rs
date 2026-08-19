use super::config_load_error;
use crate::config_manager::ConfigManager;
use crate::error_code::internal_error;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_cloud_config::cloud_config_bundle_loader_for_chatgpt_account;
use codex_config::CloudConfigBundleLoader;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_core::config::ConfigOverrides;
use codex_login::PendingChatgptAccountSelection;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

pub(super) struct ThreadConfigResolution {
    pub(super) config: Config,
    pub(super) cloud_config_bundle: Option<CloudConfigBundleLoader>,
    chatgpt_account_selection: Option<PendingChatgptAccountSelection>,
}

impl ThreadConfigResolution {
    pub(super) async fn into_config(
        mut self,
        thread_manager: &ThreadManager,
        allow_provider_model_fallback: bool,
    ) -> Result<Config, JSONRPCErrorError> {
        if let Some(selection) = self.chatgpt_account_selection {
            let binding = selection.binding();
            thread_manager
                .validate_chatgpt_account_for_config(
                    &self.config,
                    binding,
                    allow_provider_model_fallback,
                )
                .await
                .map_err(|err| {
                    internal_error(format!("failed to validate ChatGPT account: {err}"))
                })?;
            let binding = selection.commit().await.map_err(|err| {
                internal_error(format!("failed to commit ChatGPT account selection: {err}"))
            })?;
            self.config = self.config.with_session_chatgpt_account_binding(binding);
        }
        Ok(self.config)
    }
}

pub(super) async fn load_thread_config(
    config_manager: &ConfigManager,
    thread_manager: &ThreadManager,
    request_overrides: Option<HashMap<String, Value>>,
    typesafe_overrides: ConfigOverrides,
    fallback_cwd: Option<PathBuf>,
    allow_provider_model_fallback: bool,
) -> Result<ThreadConfigResolution, JSONRPCErrorError> {
    let current_cli_overrides = config_manager.current_cli_overrides();
    let mut config = config_manager
        .load_with_cli_overrides_and_cloud_config_bundle(
            &current_cli_overrides,
            request_overrides.clone(),
            typesafe_overrides.clone(),
            fallback_cwd.clone(),
            CloudConfigBundleLoader::default(),
        )
        .await
        .map_err(|err| config_load_error(&err))?;
    let chatgpt_account_selection = thread_manager
        .select_chatgpt_account_for_config(&config, allow_provider_model_fallback)
        .await
        .map_err(|err| internal_error(format!("failed to select ChatGPT account: {err}")))?;
    let cloud_config_bundle = chatgpt_account_selection.as_ref().map(|selection| {
        cloud_config_bundle_loader_for_chatgpt_account(
            thread_manager.auth_manager(),
            selection.binding().account_id.clone(),
            config.chatgpt_base_url.clone(),
            config.codex_home.to_path_buf(),
            config.http_client_factory(),
        )
    });
    if let Some(cloud_config_bundle) = cloud_config_bundle.as_ref() {
        config = config_manager
            .load_with_cli_overrides_and_cloud_config_bundle(
                &current_cli_overrides,
                request_overrides,
                typesafe_overrides,
                fallback_cwd,
                cloud_config_bundle.clone(),
            )
            .await
            .map_err(|err| config_load_error(&err))?;
    } else {
        config = config_manager
            .load_for_cwd(request_overrides, typesafe_overrides, fallback_cwd)
            .await
            .map_err(|err| config_load_error(&err))?;
    }

    Ok(ThreadConfigResolution {
        config,
        cloud_config_bundle,
        chatgpt_account_selection,
    })
}
