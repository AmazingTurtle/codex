use super::*;
impl ChatWidget {
    pub(crate) fn show_account_manager(&mut self, results: Vec<AccountModelsReadResult>) {
        let has_accounts = !results.is_empty();
        let current_model = self.current_model().to_string();
        let mut items = results
            .into_iter()
            .map(|result| {
                let account = result.account;
                let account_id = account.account_id.clone();
                let name = account
                    .email
                    .clone()
                    .unwrap_or_else(|| account.account_id.clone());
                let plan = crate::status::plan_type_display_name(account.plan_type);
                let model_available = result
                    .models
                    .as_ref()
                    .is_some_and(|models| models.iter().any(|model| model.model == current_model));
                let capability = match result.models.as_ref() {
                    Some(models) if model_available => {
                        format!("{plan} · current model available · {} models", models.len())
                    }
                    Some(models) => {
                        format!(
                            "{plan} · current model unavailable · {} models",
                            models.len()
                        )
                    }
                    None => format!(
                        "{plan} · model availability unknown ({})",
                        result
                            .error
                            .unwrap_or_else(|| "backend unavailable".to_string())
                    ),
                };
                let action: SelectionAction = if model_available || result.models.is_none() {
                    Box::new(move |tx| {
                        tx.send(AppEvent::SwitchChatgptAccount {
                            account_id: account_id.clone(),
                        });
                    })
                } else {
                    let fallback_account = account.clone();
                    let fallback_models = result.models.unwrap_or_default();
                    let requested_model = current_model.clone();
                    Box::new(move |tx| {
                        tx.send(AppEvent::OpenAccountModelFallback {
                            account: fallback_account.clone(),
                            models: fallback_models.clone(),
                            requested_model: requested_model.clone(),
                        });
                    })
                };
                SelectionItem {
                    name,
                    description: Some(capability),
                    is_current: account.is_active,
                    is_disabled: account.is_active || !account.is_eligible,
                    disabled_reason: (!account.is_eligible)
                        .then_some("not allowed by the current configuration".to_string()),
                    actions: vec![action],
                    dismiss_on_select: true,
                    search_value: Some(format!(
                        "{} {}",
                        account.email.as_deref().unwrap_or_default(),
                        account.account_id
                    )),
                    ..Default::default()
                }
            })
            .collect::<Vec<_>>();
        items.push(SelectionItem {
            name: "Add account in browser…".to_string(),
            description: Some("Sign in to another ChatGPT account with OAuth".to_string()),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::AddChatgptAccount {
                    method: ChatgptLoginMethod::Browser,
                });
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
        items.push(SelectionItem {
            name: "Add account with device code…".to_string(),
            description: Some("Sign in without a local browser callback".to_string()),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::AddChatgptAccount {
                    method: ChatgptLoginMethod::DeviceCode,
                });
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
        if has_accounts {
            items.push(SelectionItem {
                name: "Remove an account…".to_string(),
                description: Some("Revoke and remove one stored ChatGPT login".to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::OpenRemoveAccountManager);
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("ChatGPT accounts".to_string()),
            subtitle: Some(if has_accounts {
                "Select the account Codex should use".to_string()
            } else {
                "Add a ChatGPT account to use with Codex".to_string()
            }),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: has_accounts,
            search_placeholder: has_accounts.then(|| "Search accounts".to_string()),
            ..Default::default()
        });
        self.request_redraw();
    }

    pub(crate) fn show_remove_account_picker(&mut self, accounts: Vec<ChatgptAccountSummary>) {
        let items = accounts
            .into_iter()
            .map(|account| {
                let label = account
                    .email
                    .clone()
                    .unwrap_or_else(|| account.account_id.clone());
                SelectionItem {
                    name: label,
                    description: Some(account.account_id.clone()),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::ConfirmRemoveChatgptAccount {
                            account: account.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Remove a ChatGPT account".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Search accounts".to_string()),
            ..Default::default()
        });
        self.request_redraw();
    }

    pub(crate) fn show_account_model_fallback(
        &mut self,
        account: ChatgptAccountSummary,
        models: Vec<ApiModel>,
        requested_model: String,
    ) {
        let label = account
            .email
            .clone()
            .unwrap_or_else(|| account.account_id.clone());
        let account_id = account.account_id;
        let items = models
            .into_iter()
            .filter(|model| !model.hidden)
            .map(|model| {
                let account_id = account_id.clone();
                let model_id = model.model.clone();
                let effort = model.default_reasoning_effort;
                SelectionItem {
                    name: model.display_name,
                    description: Some(model.description),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::ApplyChatgptAccountModelFallback {
                            account_id: account_id.clone(),
                            model: model_id.clone(),
                            effort: effort.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            self.add_error_message(format!(
                "{label} cannot use {requested_model}, and no alternative models were returned."
            ));
            return;
        }
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(format!("{requested_model} is unavailable for {label}")),
            subtitle: Some(
                "Choose a model to switch the account and remember the replacement.".to_string(),
            ),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Search compatible models".to_string()),
            ..Default::default()
        });
        self.request_redraw();
    }

    pub(crate) fn show_remove_account_confirmation(&mut self, account: ChatgptAccountSummary) {
        let account_id = account.account_id.clone();
        let label = account.email.unwrap_or_else(|| account.account_id.clone());
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(format!("Remove {label}?")),
            subtitle: Some(
                "This revokes the login and removes its stored credentials.".to_string(),
            ),
            footer_hint: Some(standard_popup_hint_line()),
            items: vec![
                SelectionItem {
                    name: "Keep account".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Remove account".to_string(),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::RemoveChatgptAccount {
                            account_id: account_id.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        self.request_redraw();
    }
}
