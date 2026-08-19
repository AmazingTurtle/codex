use codex_http_client::HttpClientFactory;
use codex_login::AuthManager;
use codex_models_manager::manager::SharedModelsManager;
use futures::StreamExt;

const MODEL_CATALOG_CONCURRENCY: usize = 5;

pub(crate) async fn compatible_chatgpt_account_ids(
    auth_manager: &AuthManager,
    models_manager: &SharedModelsManager,
    model: &str,
    http_client_factory: HttpClientFactory,
) -> std::io::Result<Vec<String>> {
    compatible_chatgpt_account_ids_excluding(
        auth_manager,
        models_manager,
        model,
        http_client_factory,
        &[],
    )
    .await
}

pub(crate) async fn compatible_chatgpt_account_ids_excluding(
    auth_manager: &AuthManager,
    models_manager: &SharedModelsManager,
    model: &str,
    http_client_factory: HttpClientFactory,
    excluded_account_ids: &[String],
) -> std::io::Result<Vec<String>> {
    let accounts = auth_manager
        .chatgpt_accounts()?
        .into_iter()
        .filter(|account| {
            account.is_eligible && !excluded_account_ids.contains(&account.account_id)
        })
        .collect::<Vec<_>>();
    let mut compatible_account_ids = Vec::new();
    let mut successful_catalogs = 0;
    let mut catalog_errors = Vec::new();

    let mut catalogs = futures::stream::iter(accounts)
        .map(|account| {
            let models_manager = models_manager.clone();
            let http_client_factory = http_client_factory.clone();
            async move {
                let account_id = account.account_id;
                let result = models_manager
                    .list_models_for_account(&account_id, http_client_factory)
                    .await;
                (account_id, result)
            }
        })
        .buffered(MODEL_CATALOG_CONCURRENCY);
    while let Some((account_id, result)) = catalogs.next().await {
        match result {
            Ok(models) => {
                successful_catalogs += 1;
                if models.iter().any(|preset| preset.model == model) {
                    compatible_account_ids.push(account_id);
                }
            }
            Err(err) => catalog_errors.push(format!("{account_id}: {err}")),
        }
    }

    if successful_catalogs == 0 && !catalog_errors.is_empty() {
        return Err(std::io::Error::other(format!(
            "could not verify ChatGPT model availability for any account: {}",
            catalog_errors.join("; ")
        )));
    }

    Ok(compatible_account_ids)
}

#[cfg(test)]
#[path = "chatgpt_account_selection_tests.rs"]
mod tests;
