//! Selects, reserves, and rotates persisted ChatGPT account bindings.

use super::*;

impl AuthManager {
    pub fn active_account_change_receiver(&self) -> watch::Receiver<u64> {
        self.active_account_change_tx.subscribe()
    }

    pub fn manual_chatgpt_account_selection(&self) -> Option<ChatgptAccountBinding> {
        let selection = self.manual_account_selection.read().ok()?;
        Some(ChatgptAccountBinding {
            account_id: selection.account_id.clone()?,
            manual_switch_revision: selection.revision,
        })
    }

    /// Selects and reserves the ChatGPT account for one newly constructed session.
    pub async fn select_chatgpt_account_for_session(
        &self,
    ) -> std::io::Result<Option<ChatgptAccountBinding>> {
        let managed_account_id = self.persisted_chatgpt_account_id()?;
        if managed_account_id.is_some()
            && self.chatgpt_account_selection == ChatgptAccountSelection::RoundRobin
        {
            let _selection_permit = self
                .account_selection_lock
                .acquire()
                .await
                .map_err(std::io::Error::other)?;
            if let Some(binding) = self
                .rotate_chatgpt_account_locked(
                    /*expected_account_id*/ None,
                    &[],
                    /*eligible_account_ids*/ None,
                )
                .await?
            {
                return Ok(Some(binding));
            }
        }
        Ok(managed_account_id.map(|account_id| self.binding_for_account_id(account_id)))
    }

    /// Selects and reserves a model-compatible ChatGPT account for a newly constructed session.
    pub async fn select_compatible_chatgpt_account_for_session(
        &self,
        eligible_account_ids: &[String],
    ) -> std::io::Result<Option<ChatgptAccountBinding>> {
        let managed_account_id = self.persisted_chatgpt_account_id()?;
        if managed_account_id.is_some()
            && self.chatgpt_account_selection == ChatgptAccountSelection::RoundRobin
        {
            let _selection_permit = self
                .account_selection_lock
                .acquire()
                .await
                .map_err(std::io::Error::other)?;
            return self
                .rotate_chatgpt_account_locked(
                    /*expected_account_id*/ None,
                    &[],
                    Some(eligible_account_ids),
                )
                .await;
        }
        Ok(managed_account_id
            .filter(|account_id| eligible_account_ids.contains(account_id))
            .map(|account_id| self.binding_for_account_id(account_id)))
    }

    /// Reserves the next compatible round-robin account without changing persisted auth state.
    pub async fn reserve_compatible_chatgpt_account_for_session(
        self: &Arc<Self>,
        eligible_account_ids: &[String],
    ) -> std::io::Result<Option<ChatgptAccountReservation>> {
        let selection_permit = Arc::clone(&self.account_selection_lock)
            .acquire_owned()
            .await
            .map_err(std::io::Error::other)?;
        if self.chatgpt_account_selection != ChatgptAccountSelection::RoundRobin
            || self.persisted_chatgpt_account_id()?.is_none()
        {
            return Ok(None);
        }
        let Some((accounts, next_index)) = self.next_chatgpt_account(
            /*expected_account_id*/ None,
            &[],
            Some(eligible_account_ids),
        )?
        else {
            return Ok(None);
        };
        let account_id = accounts[next_index]
            .chatgpt_account_id()
            .ok_or_else(|| std::io::Error::other("selected ChatGPT account has no account id"))?;
        Ok(Some(ChatgptAccountReservation {
            manager: Arc::clone(self),
            binding: self.binding_for_account_id(account_id),
            _selection_permit: selection_permit,
        }))
    }

    /// Moves away from the account that reached a usage limit, excluding
    /// accounts already attempted by the current request.
    pub async fn rotate_chatgpt_account_after_limit(
        &self,
        expected_account_id: &str,
        attempted_account_ids: &[String],
        eligible_account_ids: Option<&[String]>,
    ) -> std::io::Result<Option<ChatgptAccountBinding>> {
        let _selection_permit = self
            .account_selection_lock
            .acquire()
            .await
            .map_err(std::io::Error::other)?;
        self.rotate_chatgpt_account_locked(
            Some(expected_account_id),
            attempted_account_ids,
            eligible_account_ids,
        )
        .await
    }

    /// Replaces a session binding whose persisted account was removed.
    pub async fn replace_removed_chatgpt_account_for_session(
        &self,
        removed_account_id: &str,
        eligible_account_ids: &[String],
    ) -> std::io::Result<Option<ChatgptAccountBinding>> {
        let _selection_permit = self
            .account_selection_lock
            .acquire()
            .await
            .map_err(std::io::Error::other)?;
        self.rotate_chatgpt_account_locked(
            Some(removed_account_id),
            &[],
            Some(eligible_account_ids),
        )
        .await
    }

    pub(super) async fn rotate_chatgpt_account_locked(
        &self,
        expected_account_id: Option<&str>,
        excluded_account_ids: &[String],
        eligible_account_ids: Option<&[String]>,
    ) -> std::io::Result<Option<ChatgptAccountBinding>> {
        let _rotation_guard = self
            .refresh_lock
            .acquire()
            .await
            .map_err(std::io::Error::other)?;
        let Some((mut accounts, next_index)) = self.next_chatgpt_account(
            expected_account_id,
            excluded_account_ids,
            eligible_account_ids,
        )?
        else {
            return Ok(None);
        };
        if next_index == 0 {
            let account_id = accounts[0].chatgpt_account_id().ok_or_else(|| {
                std::io::Error::other("selected ChatGPT account has no account id")
            })?;
            return Ok(Some(self.binding_for_account_id(account_id)));
        }
        accounts.rotate_left(next_index);
        let mut next = accounts.remove(0);
        let account_id = next
            .chatgpt_account_id()
            .ok_or_else(|| std::io::Error::other("selected ChatGPT account has no account id"))?;
        next.accounts = accounts;
        let storage = create_auth_storage(
            self.codex_home.clone(),
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        );
        storage.save(&next)?;
        self.reload().await;
        self.active_account_change_tx
            .send_modify(|revision| *revision += 1);
        Ok(Some(self.binding_for_account_id(account_id)))
    }

    pub(super) fn next_chatgpt_account(
        &self,
        expected_account_id: Option<&str>,
        excluded_account_ids: &[String],
        eligible_account_ids: Option<&[String]>,
    ) -> std::io::Result<Option<(Vec<AuthDotJson>, usize)>> {
        let storage = create_auth_storage(
            self.codex_home.clone(),
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        );
        let Some(mut current) = storage.load()? else {
            return Ok(None);
        };
        if current.resolved_mode() != AuthMode::Chatgpt {
            return Ok(None);
        }

        let mut accounts = Vec::with_capacity(current.accounts.len() + 1);
        let remaining = std::mem::take(&mut current.accounts);
        accounts.push(current);
        accounts.extend(remaining);
        let expected_index = expected_account_id.and_then(|expected| {
            accounts
                .iter()
                .position(|account| account.chatgpt_account_id().as_deref() == Some(expected))
        });
        let is_candidate = |index: &usize| {
            accounts[*index]
                .chatgpt_account_id()
                .is_some_and(|account_id| {
                    !excluded_account_ids.contains(&account_id)
                        && eligible_account_ids
                            .is_none_or(|eligible| eligible.contains(&account_id))
                        && self.is_chatgpt_account_eligible(&account_id)
                })
        };
        let next_index = match (expected_account_id, expected_index) {
            (_, Some(expected_index)) => (1..=accounts.len())
                .map(|offset| (expected_index + offset) % accounts.len())
                .find(is_candidate),
            (Some(_), None) => (0..accounts.len()).find(is_candidate),
            (None, None) => (1..=accounts.len())
                .map(|offset| offset % accounts.len())
                .find(is_candidate),
        };
        Ok(next_index.map(|next_index| (accounts, next_index)))
    }

    pub(super) fn binding_for_account_id(&self, account_id: String) -> ChatgptAccountBinding {
        let manual_switch_revision = self
            .manual_account_selection
            .read()
            .map(|selection| selection.revision)
            .unwrap_or_default();
        ChatgptAccountBinding {
            account_id,
            manual_switch_revision,
        }
    }

    pub(super) fn record_manual_account_switch(&self, account_id: &str) {
        if let Ok(mut selection) = self.manual_account_selection.write() {
            selection.revision = selection.revision.wrapping_add(1);
            selection.account_id = Some(account_id.to_string());
        }
    }
}
