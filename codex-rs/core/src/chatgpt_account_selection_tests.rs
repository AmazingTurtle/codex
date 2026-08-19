use super::*;
use chrono::Utc;
use codex_config::ManagedAuthPolicy;
use codex_config::types::AuthCredentialsStoreMode;
use codex_config::types::ChatgptAccountSelection;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthConfig;
use codex_login::AuthDotJson;
use codex_login::AuthKeyringBackendKind;
use codex_login::save_auth;
use codex_login::token_data::IdTokenInfo;
use codex_login::token_data::TokenData;
use codex_models_manager::bundled_models_response;
use codex_models_manager::manager::ModelsManager;
use codex_models_manager::manager::ModelsManagerFuture;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::error::Result as CoreResult;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelsResponse;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::Semaphore;
use tokio::sync::TryLockError;
use tokio::time::timeout;

#[derive(Debug)]
struct BlockingModelsManager {
    active: AtomicUsize,
    max_active: AtomicUsize,
    started: AtomicUsize,
    release: Semaphore,
}

#[derive(Clone, Copy, Debug)]
enum CatalogOutcome {
    Compatible,
    Empty,
    Error,
}

#[derive(Debug)]
struct OutcomeModelsManager {
    outcomes: HashMap<String, CatalogOutcome>,
}

impl ModelsManager for OutcomeModelsManager {
    fn list_models_for_account<'a>(
        &'a self,
        account_id: &'a str,
        _http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'a, CoreResult<Vec<ModelPreset>>> {
        Box::pin(async move {
            match self.outcomes.get(account_id).copied() {
                Some(CatalogOutcome::Compatible) => {
                    let preset = bundled_models_response()
                        .expect("bundled models")
                        .models
                        .into_iter()
                        .find(|model| model.slug == "gpt-5.5")
                        .expect("gpt-5.5 model")
                        .into();
                    Ok(vec![preset])
                }
                Some(CatalogOutcome::Empty) => Ok(Vec::new()),
                Some(CatalogOutcome::Error) | None => Err(codex_protocol::error::CodexErr::Stream(
                    format!("catalog failed for {account_id}"),
                )),
            }
        })
    }

    fn raw_model_catalog(
        &self,
        _refresh_strategy: RefreshStrategy,
        _http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ModelsResponse> {
        Box::pin(async { ModelsResponse::default() })
    }

    fn get_remote_models(&self) -> ModelsManagerFuture<'_, Vec<ModelInfo>> {
        Box::pin(async { Vec::new() })
    }

    fn try_get_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError> {
        Ok(Vec::new())
    }

    fn auth_manager(&self) -> Option<&AuthManager> {
        None
    }

    fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        Vec::new()
    }

    fn refresh_if_new_etag(
        &self,
        _etag: String,
        _http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        Box::pin(async {})
    }
}

impl BlockingModelsManager {
    fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            release: Semaphore::new(/*permits*/ 0),
        }
    }
}

impl ModelsManager for BlockingModelsManager {
    fn list_models_for_account<'a>(
        &'a self,
        _account_id: &'a str,
        _http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'a, CoreResult<Vec<ModelPreset>>> {
        Box::pin(async move {
            let active = self.active.fetch_add(/*val*/ 1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            self.started.fetch_add(/*val*/ 1, Ordering::SeqCst);
            self.release
                .acquire()
                .await
                .expect("release semaphore should remain open")
                .forget();
            self.active.fetch_sub(/*val*/ 1, Ordering::SeqCst);
            Ok(Vec::new())
        })
    }

    fn raw_model_catalog(
        &self,
        _refresh_strategy: RefreshStrategy,
        _http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ModelsResponse> {
        Box::pin(async { ModelsResponse::default() })
    }

    fn get_remote_models(&self) -> ModelsManagerFuture<'_, Vec<ModelInfo>> {
        Box::pin(async { Vec::new() })
    }

    fn try_get_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError> {
        Ok(Vec::new())
    }

    fn auth_manager(&self) -> Option<&AuthManager> {
        None
    }

    fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        Vec::new()
    }

    fn refresh_if_new_etag(
        &self,
        _etag: String,
        _http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        Box::pin(async {})
    }
}

fn stored_chatgpt_account(account_id: &str) -> AuthDotJson {
    AuthDotJson {
        auth_mode: Some(codex_protocol::auth::AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: Some(TokenData {
            id_token: IdTokenInfo {
                raw_jwt: "e30.e30.c2ln".to_string(),
                ..Default::default()
            },
            access_token: format!("access-{account_id}"),
            refresh_token: format!("refresh-{account_id}"),
            account_id: Some(account_id.to_string()),
        }),
        last_refresh: Some(Utc::now()),
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        accounts: Vec::new(),
    }
}

#[tokio::test]
async fn model_catalog_probes_are_bounded_and_concurrent() {
    let codex_home = tempdir().expect("temporary Codex home");
    for account_id in ["a", "b", "c", "d", "e", "f"] {
        save_auth(
            codex_home.path(),
            &stored_chatgpt_account(account_id),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::Direct,
        )
        .expect("store account");
    }
    let auth_manager = AuthManager::shared_from_auth_config(
        AuthConfig {
            codex_home: codex_home.path().to_path_buf(),
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::Direct,
            forced_login_method: None,
            chatgpt_base_url: None,
            forced_chatgpt_workspace_id: None,
            managed_auth_policy: ManagedAuthPolicy::default(),
            auth_route_config: codex_login::test_support::transport_default_auth_route_config(),
            chatgpt_account_selection: ChatgptAccountSelection::RoundRobin,
        },
        /*enable_codex_api_key_env*/ false,
    )
    .await
    .expect("build auth manager");
    let models_manager = Arc::new(BlockingModelsManager::new());
    let shared_models_manager: SharedModelsManager = models_manager.clone();
    let task = tokio::spawn({
        let auth_manager = Arc::clone(&auth_manager);
        async move {
            compatible_chatgpt_account_ids(
                &auth_manager,
                &shared_models_manager,
                "test-model",
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await
        }
    });

    timeout(Duration::from_secs(1), async {
        while models_manager.started.load(Ordering::SeqCst) < MODEL_CATALOG_CONCURRENCY {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first probe batch should start concurrently");
    assert_eq!(
        models_manager.started.load(Ordering::SeqCst),
        MODEL_CATALOG_CONCURRENCY
    );
    assert_eq!(
        models_manager.max_active.load(Ordering::SeqCst),
        MODEL_CATALOG_CONCURRENCY
    );

    models_manager.release.add_permits(/*n*/ 1);
    timeout(Duration::from_secs(1), async {
        while models_manager.started.load(Ordering::SeqCst) < 6 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("next probe should start when capacity is available");
    models_manager.release.add_permits(/*n*/ 5);

    assert_eq!(
        task.await
            .expect("catalog probe task")
            .expect("probe catalogs"),
        Vec::<String>::new()
    );
    assert_eq!(
        models_manager.max_active.load(Ordering::SeqCst),
        MODEL_CATALOG_CONCURRENCY
    );
}

#[tokio::test]
async fn model_catalog_probe_tolerates_partial_failure() {
    let codex_home = tempdir().expect("temporary Codex home");
    for account_id in ["failed", "compatible", "empty"] {
        save_auth(
            codex_home.path(),
            &stored_chatgpt_account(account_id),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::Direct,
        )
        .expect("store account");
    }
    let auth_manager = AuthManager::shared_from_auth_config(
        AuthConfig {
            codex_home: codex_home.path().to_path_buf(),
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::Direct,
            forced_login_method: None,
            chatgpt_base_url: None,
            forced_chatgpt_workspace_id: None,
            managed_auth_policy: ManagedAuthPolicy::default(),
            auth_route_config: codex_login::test_support::transport_default_auth_route_config(),
            chatgpt_account_selection: ChatgptAccountSelection::RoundRobin,
        },
        /*enable_codex_api_key_env*/ false,
    )
    .await
    .expect("build auth manager");
    let models_manager: SharedModelsManager = Arc::new(OutcomeModelsManager {
        outcomes: HashMap::from([
            ("failed".to_string(), CatalogOutcome::Error),
            ("compatible".to_string(), CatalogOutcome::Compatible),
            ("empty".to_string(), CatalogOutcome::Empty),
        ]),
    });

    assert_eq!(
        compatible_chatgpt_account_ids(
            &auth_manager,
            &models_manager,
            "gpt-5.5",
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        )
        .await
        .expect("partial failure should be tolerated"),
        vec!["compatible"]
    );
}

#[tokio::test]
async fn model_catalog_probe_reports_when_every_catalog_fails() {
    let codex_home = tempdir().expect("temporary Codex home");
    for account_id in ["a", "b"] {
        save_auth(
            codex_home.path(),
            &stored_chatgpt_account(account_id),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::Direct,
        )
        .expect("store account");
    }
    let auth_manager = AuthManager::shared_from_auth_config(
        AuthConfig {
            codex_home: codex_home.path().to_path_buf(),
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::Direct,
            forced_login_method: None,
            chatgpt_base_url: None,
            forced_chatgpt_workspace_id: None,
            managed_auth_policy: ManagedAuthPolicy::default(),
            auth_route_config: codex_login::test_support::transport_default_auth_route_config(),
            chatgpt_account_selection: ChatgptAccountSelection::RoundRobin,
        },
        /*enable_codex_api_key_env*/ false,
    )
    .await
    .expect("build auth manager");
    let models_manager: SharedModelsManager = Arc::new(OutcomeModelsManager {
        outcomes: HashMap::from([
            ("a".to_string(), CatalogOutcome::Error),
            ("b".to_string(), CatalogOutcome::Error),
        ]),
    });

    let error = compatible_chatgpt_account_ids(
        &auth_manager,
        &models_manager,
        "gpt-5.5",
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
    )
    .await
    .expect_err("all catalog failures should be reported");
    let message = error.to_string();
    assert!(message.contains("could not verify ChatGPT model availability for any account"));
    assert!(message.contains("a:"), "unexpected error: {message}");
    assert!(message.contains("b:"), "unexpected error: {message}");
}
