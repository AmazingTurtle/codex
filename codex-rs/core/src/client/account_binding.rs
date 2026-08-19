//! Maintains model-compatible account ownership for one model client.

use super::*;

impl ModelClient {
    pub(super) async fn rotate_chatgpt_account_after_limit(
        &self,
        auth_manager: &AuthManager,
        account_id: &str,
        attempted_account_ids: &[String],
        model_info: &ModelInfo,
    ) -> Result<Option<ChatgptAccountBinding>> {
        let eligible_account_ids = if let Some(models_manager) = self.state.models_manager.get() {
            match crate::chatgpt_account_selection::compatible_chatgpt_account_ids_excluding(
                auth_manager,
                models_manager,
                &model_info.slug,
                self.http_client_factory.clone(),
                attempted_account_ids,
            )
            .await
            {
                Ok(eligible) => Some(eligible),
                Err(fetch_err) => {
                    warn!(
                        error = %fetch_err,
                        "could not verify model availability for failover accounts"
                    );
                    Some(Vec::new())
                }
            }
        } else {
            None
        };
        Ok(auth_manager
            .rotate_chatgpt_account_after_limit(
                account_id,
                attempted_account_ids,
                eligible_account_ids.as_deref(),
            )
            .await?)
    }

    pub(super) async fn current_client_setup_for_model(
        &self,
        model_info: &ModelInfo,
    ) -> Result<CurrentClientSetup> {
        let unavailable_account_id = match self.current_client_setup(ClientRouting::Workspace).await
        {
            Ok(setup) => return Ok(setup),
            Err(err) => {
                let Some(unavailable) = (match err.details() {
                    CodexErrorDetails::Io(io_error) => io_error.get_ref().and_then(|source| {
                        source.downcast_ref::<PinnedChatgptAccountUnavailable>()
                    }),
                    _ => None,
                }) else {
                    return Err(err);
                };
                unavailable.account_id().to_string()
            }
        };
        let auth_manager = self.state.provider.auth_manager().ok_or_else(|| {
            CodexErr::Io(std::io::Error::other(
                "cannot replace a removed session account without authentication",
            ))
        })?;
        let models_manager = self.state.models_manager.get().ok_or_else(|| {
            CodexErr::Io(std::io::Error::other(
                "cannot replace a removed session account without model discovery",
            ))
        })?;
        let eligible_account_ids =
            crate::chatgpt_account_selection::compatible_chatgpt_account_ids(
                &auth_manager,
                models_manager,
                &model_info.slug,
                self.http_client_factory.clone(),
            )
            .await?;
        let binding = auth_manager
            .replace_removed_chatgpt_account_for_session(
                &unavailable_account_id,
                &eligible_account_ids,
            )
            .await?
            .ok_or_else(|| {
                CodexErr::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "no remaining ChatGPT account supports model `{}`",
                        model_info.slug
                    ),
                ))
            })?;
        self.set_chatgpt_account_binding(binding);
        self.current_client_setup(ClientRouting::Workspace).await
    }

    pub(super) fn current_chatgpt_account_binding(&self) -> Option<ChatgptAccountBinding> {
        let mut binding = self
            .state
            .chatgpt_account_binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(manual) = self
            .state
            .provider
            .auth_manager()
            .and_then(|manager| manager.manual_chatgpt_account_selection())
            && binding.as_ref().is_none_or(|current| {
                manual.manual_switch_revision > current.manual_switch_revision
            })
        {
            *binding = Some(manual);
        }
        binding.clone()
    }

    pub(super) fn set_chatgpt_account_binding(&self, binding: ChatgptAccountBinding) {
        *self
            .state
            .chatgpt_account_binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(binding);
    }
}
