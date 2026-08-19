//! Account navigation for `/usage`. Picker reads never update session rate limits.
use super::*;
use codex_app_server_protocol::AccountRateLimitsReadManyResponse;
use codex_app_server_protocol::RateLimitResetCreditsSummary;

const ROOT: &str = "usage-menu";
const ACCOUNT: &str = "usage-account";
const RANGES: &str = "usage-ranges";

#[derive(Default)]
pub(super) struct UsagePickerState {
    pending_accounts: Option<u64>,
    pub(super) account_selected_idx: Option<usize>,
    pub(super) account: Option<ChatgptAccountSummary>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum UsageReadPurpose {
    Account,
    ResetPicker,
    ResetConsume,
}

#[derive(Debug)]
pub(crate) enum UsagePickerEvent {
    LoadAccounts {
        request_id: u64,
    },
    AccountsLoaded {
        request_id: u64,
        result: Result<Vec<ChatgptAccountSummary>, String>,
    },
    OpenAccount(ChatgptAccountSummary),
    RefreshAccount,
    ReadAccount {
        request_id: u64,
        account_id: String,
        purpose: UsageReadPurpose,
    },
    AccountLoaded {
        request_id: u64,
        account_id: String,
        purpose: UsageReadPurpose,
        result: Result<AccountRateLimitsReadManyResponse, String>,
    },
    ShowRanges,
    ShowUsage {
        view: TokenActivityView,
        account_id: Option<String>,
    },
}

impl ChatWidget {
    pub(super) fn open_usage_menu(&mut self) {
        self.close_usage_picker();
        self.clear_pending_rate_limit_reset_hint();
        let request_id = self.take_next_rate_limit_reset_request_id();
        self.usage_picker.pending_accounts = Some(request_id);
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(ROOT),
            title: Some("Usage".to_string()),
            items: vec![SelectionItem {
                name: "Loading accounts…".to_string(),
                is_disabled: true,
                ..Default::default()
            }],
            ..Default::default()
        });
        self.app_event_tx
            .send(AppEvent::UsagePicker(UsagePickerEvent::LoadAccounts {
                request_id,
            }));
        self.request_redraw();
    }

    pub(crate) fn finish_usage_accounts(
        &mut self,
        request_id: u64,
        result: Result<Vec<ChatgptAccountSummary>, String>,
    ) {
        if self.usage_picker.pending_accounts != Some(request_id) {
            return;
        }
        self.usage_picker.pending_accounts = None;
        let mut items = vec![SelectionItem {
            name: "Show cumulative usage".to_string(),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::UsagePicker(UsagePickerEvent::ShowUsage {
                    view: TokenActivityView::Cumulative,
                    account_id: None,
                }))
            })],
            ..Default::default()
        }];
        let subtitle = match result {
            Ok(accounts) => {
                let empty = accounts.is_empty();
                for account in accounts {
                    let label = account
                        .email
                        .clone()
                        .unwrap_or_else(|| account.account_id.clone());
                    items.push(SelectionItem {
                        name: label.clone(),
                        description: Some(crate::status::plan_type_display_name(account.plan_type)),
                        is_current: account.is_active,
                        is_disabled: !account.is_eligible,
                        disabled_reason: (!account.is_eligible)
                            .then(|| "not allowed by the current configuration".to_string()),
                        search_value: Some(format!("{label} {}", account.account_id)),
                        actions: vec![Box::new(move |tx| {
                            tx.send(AppEvent::UsagePicker(UsagePickerEvent::OpenAccount(
                                account.clone(),
                            )))
                        })],
                        ..Default::default()
                    });
                }
                if empty {
                    "No stored accounts. Use /account to add one."
                } else {
                    "View usage or redeem a reset for an account."
                }
                .to_string()
            }
            Err(err) => {
                items.push(SelectionItem {
                    name: "Try again".to_string(),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::UsagePicker(UsagePickerEvent::LoadAccounts {
                            request_id,
                        }))
                    })],
                    ..Default::default()
                });
                // Keep this request current so retry can replace the same loading/error view.
                self.usage_picker.pending_accounts = Some(request_id);
                format!("Couldn't load accounts: {err}")
            }
        };
        self.bottom_pane.replace_selection_view_if_present(
            ROOT,
            SelectionViewParams {
                view_id: Some(ROOT),
                title: Some("Usage".to_string()),
                subtitle: Some(subtitle),
                items,
                is_searchable: true,
                search_placeholder: Some("Search accounts".to_string()),
                ..Default::default()
            },
        );
        self.request_redraw();
    }

    pub(crate) fn open_usage_account(&mut self, account: ChatgptAccountSummary) {
        if !account.is_eligible
            || self
                .bottom_pane
                .selected_index_for_active_view(ROOT)
                .is_none()
        {
            return;
        }
        self.usage_picker.account = Some(account);
        self.usage_picker.account_selected_idx = None;
        let params = self.usage_account_params(/*result*/ None);
        self.bottom_pane.show_selection_view(params);
        self.refresh_usage_account();
    }

    pub(crate) fn refresh_usage_account(&mut self) {
        if self
            .bottom_pane
            .selected_index_for_active_view(ACCOUNT)
            .is_none()
        {
            return;
        }
        self.usage_picker.account_selected_idx =
            self.bottom_pane.selected_index_for_active_view(ACCOUNT);
        let Some(account_id) = self.usage_reset_account_id() else {
            return;
        };
        let request_id = self.take_next_rate_limit_reset_request_id();
        self.pending_usage_menu_rate_limit_request_id = Some(request_id);
        let params = self.usage_account_params(/*result*/ None);
        self.bottom_pane
            .replace_selection_view_if_active(ACCOUNT, params);
        self.app_event_tx
            .send(AppEvent::UsagePicker(UsagePickerEvent::ReadAccount {
                request_id,
                account_id,
                purpose: UsageReadPurpose::Account,
            }));
        self.request_redraw();
    }

    fn usage_account_params(
        &self,
        result: Option<&Result<RateLimitResetCreditsSummary, String>>,
    ) -> SelectionViewParams {
        let title = self.usage_picker.account.as_ref().map(|account| {
            account
                .email
                .clone()
                .unwrap_or_else(|| account.account_id.clone())
        });
        let (suffix, enabled) = match result {
            Some(Ok(summary)) => (
                format!("{} available", summary.available_count.max(0)),
                summary.available_count > 0,
            ),
            Some(Err(_)) => ("availability unavailable".to_string(), false),
            None => ("loading…".to_string(), false),
        };
        let account_id = self.usage_reset_account_id();
        let reset_gate = std::sync::atomic::AtomicBool::new(true);
        let mut items = vec![
            SelectionItem {
                name: "Show usage for this account".to_string(),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::UsagePicker(UsagePickerEvent::ShowRanges))
                })],
                ..Default::default()
            },
            SelectionItem {
                name: format!("Redeem usage reset for this account ({suffix})"),
                is_disabled: !enabled,
                actions: vec![Box::new(move |tx| {
                    if reset_gate.swap(false, std::sync::atomic::Ordering::AcqRel) {
                        tx.send(AppEvent::OpenRateLimitResetCredits {
                            account_id: account_id.clone(),
                        });
                    }
                })],
                ..Default::default()
            },
        ];
        if matches!(result, Some(Err(_))) {
            items.push(SelectionItem {
                name: "Try again".to_string(),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::UsagePicker(UsagePickerEvent::RefreshAccount))
                })],
                ..Default::default()
            });
        }
        items.push(SelectionItem {
            name: "Back".to_string(),
            dismiss_on_select: true,
            ..Default::default()
        });
        SelectionViewParams {
            view_id: Some(ACCOUNT),
            title,
            items,
            initial_selected_idx: self.usage_picker.account_selected_idx,
            ..Default::default()
        }
    }

    pub(crate) fn finish_usage_account_read(
        &mut self,
        request_id: u64,
        account_id: String,
        purpose: UsageReadPurpose,
        result: Result<AccountRateLimitsReadManyResponse, String>,
    ) -> bool {
        if self.usage_reset_account_id().as_ref() != Some(&account_id) {
            return false;
        }
        let result = result.and_then(|response| {
            let entry = response
                .data
                .into_iter()
                .find(|entry| entry.account.account_id == account_id)
                .ok_or_else(|| "Account is no longer available.".to_string())?;
            entry
                .rate_limits
                .and_then(|limits| limits.rate_limit_reset_credits)
                .ok_or_else(|| {
                    entry
                        .error
                        .unwrap_or_else(|| "Reset availability unavailable.".to_string())
                })
        });
        match purpose {
            UsageReadPurpose::Account => {
                if self.pending_usage_menu_rate_limit_request_id != Some(request_id) {
                    return false;
                }
                self.pending_usage_menu_rate_limit_request_id = None;
                let params = self.usage_account_params(Some(&result));
                let replaced = self
                    .bottom_pane
                    .replace_selection_view_if_present(ACCOUNT, params);
                self.request_redraw();
                replaced
            }
            UsageReadPurpose::ResetPicker => {
                self.finish_rate_limit_reset_credits_refresh(request_id, Vec::new(), result)
            }
            UsageReadPurpose::ResetConsume => {
                self.finish_post_consume_reset_credits_refresh(request_id, Vec::new(), result)
            }
        }
    }

    pub(crate) fn show_usage_ranges(&mut self) {
        if self
            .bottom_pane
            .selected_index_for_active_view(ACCOUNT)
            .is_none()
        {
            return;
        }
        self.usage_picker.account_selected_idx =
            self.bottom_pane.selected_index_for_active_view(ACCOUNT);
        let Some(account_id) = self.usage_reset_account_id() else {
            return;
        };
        let mut items = [
            TokenActivityView::Daily,
            TokenActivityView::Weekly,
            TokenActivityView::Cumulative,
        ]
        .into_iter()
        .map(|view| {
            let account_id = account_id.clone();
            SelectionItem {
                name: view.label().to_string(),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::UsagePicker(UsagePickerEvent::ShowUsage {
                        view,
                        account_id: Some(account_id.clone()),
                    }))
                })],
                ..Default::default()
            }
        })
        .collect::<Vec<_>>();
        items.push(SelectionItem {
            name: "Back".to_string(),
            dismiss_on_select: true,
            ..Default::default()
        });
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(RANGES),
            title: Some("Usage time range".to_string()),
            items,
            ..Default::default()
        });
        self.request_redraw();
    }

    pub(crate) fn usage_reset_account_id(&self) -> Option<String> {
        self.usage_reset_account()
            .map(|account| account.account_id.clone())
    }

    /// Account-update notifications invalidate this identity along with pending reset requests.
    pub(crate) fn usage_reset_account(&self) -> Option<&ChatgptAccountSummary> {
        self.usage_picker.account.as_ref()
    }

    pub(crate) fn close_usage_picker(&mut self) {
        for id in [RANGES, ACCOUNT, ROOT] {
            self.bottom_pane.dismiss_view_by_id(id);
        }
        self.usage_picker = UsagePickerState::default();
        self.pending_usage_menu_rate_limit_request_id = None;
    }

    pub(super) fn decorate_account_reset_view(
        &self,
        mut params: SelectionViewParams,
    ) -> SelectionViewParams {
        if let Some(account) = &self.usage_picker.account {
            let label = account.email.as_deref().unwrap_or(&account.account_id);
            params.title = Some(format!("Usage limit resets · {label}"));
            for item in &mut params.items {
                if item.name == "Close" || item.name == "Cancel" {
                    item.name = "Back".to_string();
                    item.actions.push(Box::new(|tx| {
                        tx.send(AppEvent::UsagePicker(UsagePickerEvent::RefreshAccount))
                    }));
                }
            }
            if params.allow_cancel && !params.items.iter().any(|item| item.name == "Back") {
                params.items.push(SelectionItem {
                    name: "Back".to_string(),
                    dismiss_on_select: true,
                    actions: vec![Box::new(|tx| {
                        tx.send(AppEvent::UsagePicker(UsagePickerEvent::RefreshAccount))
                    })],
                    ..Default::default()
                });
            }
            params.on_cancel = Some(Box::new(|tx| {
                tx.send(AppEvent::UsagePicker(UsagePickerEvent::RefreshAccount))
            }));
        }
        params
    }
}
