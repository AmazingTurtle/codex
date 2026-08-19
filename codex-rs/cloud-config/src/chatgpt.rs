//! ChatGPT eligibility, credentials, and account identity for managed configuration.

use crate::backend::BundleClient;
use crate::backend::BundleRequestError;
use crate::backend::RetryableFailureKind;
use crate::config_source::BundleSource;
use codex_config::CloudConfigBundle;
use codex_config::CloudConfigBundleLoadError;
use codex_config::CloudConfigBundleLoadErrorCode;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::RefreshTokenError;
use codex_login::UnauthorizedRecovery;
use codex_protocol::account::PlanType;
use sha2::Digest;
use sha2::Sha256;
use std::sync::Arc;

const CLOUD_CONFIG_BUNDLE_AUTH_RECOVERY_FAILED_MESSAGE: &str = concat!(
    "Your authentication session could not be refreshed automatically. ",
    "Please log out and sign in again."
);

pub(crate) struct ChatGptSource<C> {
    pub(crate) auth_manager: Arc<AuthManager>,
    /// Selects a stored account instead of the active one; its cache and token refreshes stay
    /// scoped to that account.
    pub(crate) chatgpt_account_id: Option<String>,
    pub(crate) client: Arc<C>,
}

impl<C> ChatGptSource<C> {
    async fn current_auth(&self) -> std::io::Result<Option<CodexAuth>> {
        match self.chatgpt_account_id.as_deref() {
            Some(account_id) => self.auth_manager.auth_for_chatgpt_account(account_id).await,
            None => Ok(self.auth_manager.auth().await),
        }
    }
}

impl<C: BundleClient> BundleSource for ChatGptSource<C> {
    type Auth = CodexAuth;
    /// Account-scoped recovery binds to the rejected auth, so it starts on the first 401.
    type FetchContext = Option<UnauthorizedRecovery>;
    fn begin_fetch(&self) -> Self::FetchContext {
        self.chatgpt_account_id
            .is_none()
            .then(|| self.auth_manager.unauthorized_recovery())
    }

    async fn auth(&self) -> Result<Option<CodexAuth>, CloudConfigBundleLoadError> {
        let auth = self.current_auth().await.map_err(|err| {
            CloudConfigBundleLoadError::new(
                CloudConfigBundleLoadErrorCode::Auth,
                /*status_code*/ None,
                format!("failed to load account-scoped authentication: {err}"),
            )
        })?;
        Ok(auth.filter(|auth| {
            auth.uses_codex_backend()
                && auth.account_plan_type().is_some_and(|plan| {
                    plan.is_business_like()
                        || plan.is_education_like()
                        || matches!(
                            plan,
                            PlanType::Enterprise | PlanType::Sci | PlanType::SciPlus
                        )
                })
        }))
    }

    fn identity(&self, auth: &CodexAuth) -> (Option<String>, Option<String>) {
        (auth.get_chatgpt_user_id(), auth.get_account_id())
    }

    async fn get_bundle(
        &self,
        auth: &mut CodexAuth,
        _context: &mut Option<UnauthorizedRecovery>,
    ) -> Result<CloudConfigBundle, BundleRequestError> {
        self.client.get_bundle(auth).await
    }

    async fn recover_auth(
        &self,
        auth: &mut CodexAuth,
        recovery: &mut Option<UnauthorizedRecovery>,
        status_code: Option<u16>,
        _recovery_attempt: usize,
    ) -> Result<bool, BundleRequestError> {
        let recovery = recovery.get_or_insert_with(|| {
            self.auth_manager
                .unauthorized_recovery_for_auth(auth.clone())
        });
        if !recovery.has_next() {
            return Err(BundleRequestError::auth_failure(
                status_code,
                CLOUD_CONFIG_BUNDLE_AUTH_RECOVERY_FAILED_MESSAGE,
                Some("auth_recovery_unavailable"),
            ));
        }
        match recovery.next().await {
            Ok(_) => {
                *auth = self
                    .current_auth()
                    .await
                    .map_err(|err| {
                        BundleRequestError::auth_failure(
                            status_code,
                            format!("failed to reload account-scoped authentication: {err}"),
                            Some("auth_recovery_missing_auth"),
                        )
                    })?
                    .ok_or_else(|| {
                        BundleRequestError::auth_failure(
                            status_code,
                            CLOUD_CONFIG_BUNDLE_AUTH_RECOVERY_FAILED_MESSAGE,
                            Some("auth_recovery_missing_auth"),
                        )
                    })?;
                Ok(true)
            }
            Err(RefreshTokenError::Transient(_)) => Err(BundleRequestError::Retryable(
                RetryableFailureKind::AuthRecovery { status_code },
            )),
            Err(RefreshTokenError::Permanent(failed)) => Err(BundleRequestError::auth_failure(
                status_code,
                failed.message,
                Some("auth_recovery_unrecoverable"),
            )),
            Err(RefreshTokenError::Policy(error)) => Err(BundleRequestError::auth_failure(
                status_code,
                error.to_string(),
                /*metric_reason*/ None,
            )),
        }
    }

    fn cache_namespace(&self) -> Option<String> {
        self.chatgpt_account_id.as_deref().map(|account_id| {
            format!(
                "chatgpt-account-{:x}",
                Sha256::digest(account_id.as_bytes())
            )
        })
    }
}
