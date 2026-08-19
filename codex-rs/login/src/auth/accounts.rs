//! Account-scoped credential loading, switching, and removal.

use super::*;

impl AuthManager {
    pub(super) fn persisted_chatgpt_account_id(&self) -> std::io::Result<Option<String>> {
        let cached_account_id = self.auth_cached().and_then(|auth| match auth {
            CodexAuth::Chatgpt(_) => auth.get_account_id(),
            CodexAuth::ApiKey(_)
            | CodexAuth::ChatgptAuthTokens(_)
            | CodexAuth::Headers(_)
            | CodexAuth::AgentIdentity(_)
            | CodexAuth::PersonalAccessToken(_)
            | CodexAuth::BedrockApiKey(_)
            | CodexAuth::BedrockAccessKeys(_) => None,
        });
        let accounts = self.chatgpt_accounts()?;
        Ok(cached_account_id
            .and_then(|account_id| {
                accounts
                    .iter()
                    .any(|account| account.account_id == account_id && account.is_eligible)
                    .then_some(account_id)
            })
            .or_else(|| {
                accounts
                    .into_iter()
                    .find(|account| account.is_eligible)
                    .map(|account| account.account_id)
            }))
    }

    /// Returns display-safe metadata for all persisted ChatGPT accounts.
    pub fn chatgpt_accounts(&self) -> std::io::Result<Vec<ChatgptAccount>> {
        if self.has_external_auth() {
            return Ok(Vec::new());
        }
        let storage = create_auth_storage(
            self.codex_home.clone(),
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        );
        let Some(auth) = storage.load()? else {
            return Ok(Vec::new());
        };
        Ok(auth
            .chatgpt_accounts()
            .into_iter()
            .map(|mut account| {
                account.is_eligible = self.is_chatgpt_account_eligible(&account.account_id);
                account
            })
            .collect())
    }

    /// Loads refresh-capable authentication for one persisted ChatGPT account without making it
    /// globally active. Any refreshed tokens are written back only to that account's record.
    pub async fn auth_for_chatgpt_account(
        &self,
        account_id: &str,
    ) -> std::io::Result<Option<CodexAuth>> {
        let Some(auth) = self.reload_chatgpt_account_auth(account_id).await? else {
            return Ok(None);
        };
        let Some(reason) = Self::proactive_refresh_reason(&auth) else {
            return Ok(Some(auth));
        };
        if let Err(err) = self
            .refresh_chatgpt_account_from_authority(account_id, reason)
            .await
        {
            tracing::error!("Failed to refresh token: {err}");
            return Ok(Some(auth));
        }
        self.reload_chatgpt_account_auth(account_id).await
    }

    pub(super) async fn reload_chatgpt_account_auth(
        &self,
        account_id: &str,
    ) -> std::io::Result<Option<CodexAuth>> {
        let _refresh_guard = self
            .refresh_lock
            .acquire()
            .await
            .map_err(std::io::Error::other)?;
        self.load_chatgpt_account_auth(account_id).await
    }

    pub(super) async fn load_chatgpt_account_auth(
        &self,
        account_id: &str,
    ) -> std::io::Result<Option<CodexAuth>> {
        if self.has_external_auth() {
            return Ok(None);
        }
        if !self.is_chatgpt_account_eligible(account_id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "ChatGPT account `{account_id}` is not allowed by the current configuration"
                ),
            ));
        }
        let base_storage = create_auth_storage(
            self.codex_home.clone(),
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        );
        let account_storage: Arc<dyn AuthStorageBackend> = Arc::new(ChatgptAccountStorage {
            account_id: account_id.to_string(),
            storage: base_storage,
        });
        let Some(auth_dot_json) = account_storage.load()? else {
            return Ok(None);
        };
        let mut auth = CodexAuth::from_auth_dot_json(
            &self.codex_home,
            auth_dot_json,
            self.auth_credentials_store_mode,
            self.chatgpt_base_url.as_deref(),
            self.keyring_backend_kind,
            self.agent_identity_authapi_base_url.as_deref(),
            &self.auth_route_config,
        )
        .await?;
        match &mut auth {
            CodexAuth::Chatgpt(chatgpt_auth) => {
                chatgpt_auth.storage = account_storage.clone();
            }
            CodexAuth::ApiKey(_)
            | CodexAuth::ChatgptAuthTokens(_)
            | CodexAuth::Headers(_)
            | CodexAuth::AgentIdentity(_)
            | CodexAuth::PersonalAccessToken(_)
            | CodexAuth::BedrockApiKey(_)
            | CodexAuth::BedrockAccessKeys(_) => return Ok(None),
        }
        Ok(Some(auth))
    }

    /// Makes one persisted ChatGPT account active.
    pub async fn switch_chatgpt_account(&self, account_id: &str) -> std::io::Result<bool> {
        if self.has_external_auth() {
            return Err(std::io::Error::other(
                "externally managed authentication does not support account switching",
            ));
        }
        if !self.is_chatgpt_account_eligible(account_id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "ChatGPT account `{account_id}` is not allowed by the current configuration"
                ),
            ));
        }
        let _selection_permit = self
            .account_selection_lock
            .acquire()
            .await
            .map_err(std::io::Error::other)?;
        let _rotation_guard = self
            .refresh_lock
            .acquire()
            .await
            .map_err(std::io::Error::other)?;
        let storage = create_auth_storage(
            self.codex_home.clone(),
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        );
        let Some(mut current) = storage.load()? else {
            return Ok(false);
        };
        if current.resolved_mode() != AuthMode::Chatgpt {
            return Ok(false);
        }
        if current.chatgpt_account_id().as_deref() == Some(account_id) {
            self.record_manual_account_switch(account_id);
            self.active_account_change_tx
                .send_modify(|revision| *revision += 1);
            return Ok(false);
        }
        let Some(next_index) = current
            .accounts
            .iter()
            .position(|account| account.chatgpt_account_id().as_deref() == Some(account_id))
        else {
            return Ok(false);
        };
        let mut next = current.accounts.remove(next_index);
        let mut remaining = std::mem::take(&mut current.accounts);
        current.accounts.clear();
        remaining.push(current);
        next.accounts = remaining;
        storage.save(&next)?;
        self.reload().await;
        self.record_manual_account_switch(account_id);
        self.active_account_change_tx
            .send_modify(|revision| *revision += 1);
        Ok(true)
    }

    /// Revokes and removes one persisted ChatGPT account.
    pub async fn logout_chatgpt_account(&self, account_id: &str) -> std::io::Result<bool> {
        if self.has_external_auth() {
            return Err(std::io::Error::other(
                "externally managed authentication does not support account removal",
            ));
        }
        let _selection_permit = self
            .account_selection_lock
            .acquire()
            .await
            .map_err(std::io::Error::other)?;
        let _rotation_guard = self
            .refresh_lock
            .acquire()
            .await
            .map_err(std::io::Error::other)?;
        let active_before = self.auth_cached().and_then(|auth| auth.get_account_id());
        let allowed_workspaces = self.effective_chatgpt_workspaces();
        let promotion_policy = allowed_workspaces
            .as_deref()
            .map(AccountPromotionPolicy::AllowedWorkspaces)
            .unwrap_or(AccountPromotionPolicy::StoredOrder);
        let removed = logout_account_with_revoke_and_promotion(
            &self.codex_home,
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
            account_id,
            &self.auth_route_config,
            promotion_policy,
        )
        .await?;
        if removed {
            self.reload().await;
            let active_after = self.auth_cached().and_then(|auth| auth.get_account_id());
            if active_before != active_after {
                self.active_account_change_tx
                    .send_modify(|revision| *revision += 1);
            }
        }
        Ok(removed)
    }
}
