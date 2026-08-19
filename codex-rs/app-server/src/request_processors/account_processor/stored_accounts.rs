//! Implements stored-account operations and batched account reads.

use super::*;

impl AccountRequestProcessor {
    pub(super) fn chatgpt_account_summary(
        account: codex_login::ChatgptAccount,
    ) -> ChatgptAccountSummary {
        ChatgptAccountSummary {
            account_id: account.account_id,
            email: account.email,
            plan_type: account.plan_type,
            is_active: account.is_active,
            is_eligible: account.is_eligible,
        }
    }

    pub(super) fn account_list_response(
        &self,
        params: AccountListParams,
    ) -> Result<AccountListResponse, JSONRPCErrorError> {
        let mut accounts = self
            .auth_manager
            .chatgpt_accounts()
            .map_err(|err| internal_error(format!("failed to list ChatGPT accounts: {err}")))?;
        accounts.sort_unstable_by(|left, right| left.account_id.cmp(&right.account_id));
        let total = accounts.len();
        let start = params
            .cursor
            .as_deref()
            .map(|cursor| accounts.partition_point(|account| account.account_id.as_str() <= cursor))
            .unwrap_or(0);
        let limit = params.limit.unwrap_or(total as u32).max(1) as usize;
        let end = start.saturating_add(limit).min(total);
        Ok(AccountListResponse {
            data: accounts[start..end]
                .iter()
                .cloned()
                .map(Self::chatgpt_account_summary)
                .collect(),
            next_cursor: (end < total).then(|| accounts[end - 1].account_id.clone()),
        })
    }

    pub(super) async fn account_switch_response(
        &self,
        params: AccountSwitchParams,
    ) -> Result<AccountSwitchResponse, JSONRPCErrorError> {
        let account_id = params.account_id;
        let accounts = self
            .auth_manager
            .chatgpt_accounts()
            .map_err(|err| internal_error(format!("failed to list ChatGPT accounts: {err}")))?;
        let Some(account) = accounts
            .iter()
            .find(|account| account.account_id == account_id)
        else {
            return Err(invalid_request(format!(
                "ChatGPT account `{account_id}` was not found"
            )));
        };
        if !account.is_eligible {
            return Err(invalid_request(format!(
                "ChatGPT account `{account_id}` is not allowed by the current configuration"
            )));
        }
        self.auth_manager
            .switch_chatgpt_account(&account_id)
            .await
            .map_err(|err| internal_error(format!("failed to switch ChatGPT account: {err}")))?;
        let account = self
            .auth_manager
            .chatgpt_accounts()
            .map_err(|err| internal_error(format!("failed to reload ChatGPT accounts: {err}")))?
            .into_iter()
            .find(|account| account.account_id == account_id)
            .ok_or_else(|| internal_error("active ChatGPT account disappeared after switching"))?;
        Ok(AccountSwitchResponse {
            account: Self::chatgpt_account_summary(account),
        })
    }

    pub(super) async fn account_remove_response(
        &self,
        params: AccountRemoveParams,
    ) -> Result<AccountRemoveResponse, JSONRPCErrorError> {
        let account_id = params.account_id;
        let removed = self
            .auth_manager
            .logout_chatgpt_account(&account_id)
            .await
            .map_err(|err| internal_error(format!("failed to remove ChatGPT account: {err}")))?;
        if !removed {
            return Err(invalid_request(format!(
                "ChatGPT account `{account_id}` was not found"
            )));
        }
        let active_account_id = self.auth_manager.auth_cached().and_then(|auth| match auth {
            CodexAuth::Chatgpt(_) => auth.get_account_id(),
            CodexAuth::ApiKey(_)
            | CodexAuth::ChatgptAuthTokens(_)
            | CodexAuth::Headers(_)
            | CodexAuth::AgentIdentity(_)
            | CodexAuth::PersonalAccessToken(_)
            | CodexAuth::BedrockApiKey(_)
            | CodexAuth::BedrockAccessKeys(_) => None,
        });
        Ok(AccountRemoveResponse { active_account_id })
    }

    pub(super) async fn get_account_rate_limits_many_response(
        &self,
        params: AccountRateLimitsReadManyParams,
    ) -> Result<AccountRateLimitsReadManyResponse, JSONRPCErrorError> {
        let accounts = self.selected_chatgpt_accounts(params.account_ids)?;
        let read_params = GetAccountRateLimitsParams {
            supports_luna_reserve: params.supports_luna_reserve,
            exclude_reset_credit_details: params.exclude_reset_credit_details,
        };
        let data = collect_account_reads(accounts, |account| {
            let read_params = read_params.clone();
            async move {
                let account_id = account.account_id.clone();
                let http_client_factory = self.auth_manager.http_client_factory();
                let result = match self
                    .auth_manager
                    .auth_for_chatgpt_account(&account_id)
                    .await
                {
                    Ok(Some(auth)) => {
                        self.get_account_rate_limits_response_for_auth(
                            &auth,
                            http_client_factory,
                            read_params,
                        )
                        .await
                    }
                    Ok(None) => Err(invalid_request(format!(
                        "ChatGPT account `{account_id}` was not found"
                    ))),
                    Err(err) => Err(internal_error(format!(
                        "failed to load ChatGPT account `{account_id}`: {err}"
                    ))),
                };
                let (rate_limits, error) = match result {
                    Ok(rate_limits) => (Some(rate_limits), None),
                    Err(err) => (None, Some(err.message)),
                };
                AccountRateLimitsReadResult {
                    account: Self::chatgpt_account_summary(account),
                    rate_limits: rate_limits.map(Into::into),
                    error,
                }
            }
        })
        .await;
        Ok(AccountRateLimitsReadManyResponse { data })
    }

    pub(super) async fn get_account_token_usage_many_response(
        &self,
        params: AccountReadManyParams,
    ) -> Result<AccountUsageReadManyResponse, JSONRPCErrorError> {
        let accounts = self.selected_chatgpt_accounts(params.account_ids)?;
        let data = collect_account_reads(accounts, |account| async move {
            let account_id = account.account_id.clone();
            let http_client_factory = self.auth_manager.http_client_factory();
            let result = match self
                .auth_manager
                .auth_for_chatgpt_account(&account_id)
                .await
            {
                Ok(Some(auth)) => {
                    self.get_account_token_usage_response_for_auth(
                        &auth,
                        http_client_factory,
                        /*thread_id*/ None,
                    )
                    .await
                }
                Ok(None) => Err(invalid_request(format!(
                    "ChatGPT account `{account_id}` was not found"
                ))),
                Err(err) => Err(internal_error(format!(
                    "failed to load ChatGPT account `{account_id}`: {err}"
                ))),
            };
            let (usage, error) = match result {
                Ok(usage) => (Some(usage), None),
                Err(err) => (None, Some(err.message)),
            };
            AccountUsageReadResult {
                account: Self::chatgpt_account_summary(account),
                usage: usage.map(Into::into),
                error,
            }
        })
        .await;
        Ok(AccountUsageReadManyResponse { data })
    }

    pub(super) fn selected_chatgpt_accounts(
        &self,
        account_ids: Option<Vec<String>>,
    ) -> Result<Vec<codex_login::ChatgptAccount>, JSONRPCErrorError> {
        let accounts = self
            .auth_manager
            .chatgpt_accounts()
            .map_err(|err| internal_error(format!("failed to list ChatGPT accounts: {err}")))?;
        let Some(account_ids) = account_ids else {
            return Ok(accounts
                .into_iter()
                .filter(|account| account.is_eligible)
                .collect());
        };
        let mut selected = Vec::with_capacity(account_ids.len());
        for account_id in account_ids {
            let Some(account) = accounts
                .iter()
                .find(|account| account.account_id == account_id)
            else {
                return Err(invalid_request(format!(
                    "ChatGPT account `{account_id}` was not found"
                )));
            };
            if !account.is_eligible {
                return Err(invalid_request(format!(
                    "ChatGPT account `{account_id}` is not allowed by the current configuration"
                )));
            }
            if !selected
                .iter()
                .any(|selected: &codex_login::ChatgptAccount| selected.account_id == account_id)
            {
                selected.push(account.clone());
            }
        }
        Ok(selected)
    }

    pub(super) async fn get_account_models_many_response(
        &self,
        params: AccountReadManyParams,
    ) -> Result<AccountModelsReadManyResponse, JSONRPCErrorError> {
        let accounts = self.selected_chatgpt_accounts(params.account_ids)?;
        let models_manager = self.thread_manager.get_models_manager();
        let data = collect_account_reads(accounts, |account| {
            let models_manager = models_manager.clone();
            async move {
                let account_id = account.account_id.clone();
                let result = models_manager
                    .list_models_for_account(&account_id, self.config.http_client_factory())
                    .await;
                let (models, error) = match result {
                    Ok(models) => (
                        Some(
                            models
                                .into_iter()
                                .map(crate::models::model_from_preset)
                                .collect(),
                        ),
                        None,
                    ),
                    Err(err) => (None, Some(err.to_string())),
                };
                AccountModelsReadResult {
                    account: Self::chatgpt_account_summary(account),
                    models,
                    error,
                }
            }
        })
        .await;
        Ok(AccountModelsReadManyResponse { data })
    }
}
