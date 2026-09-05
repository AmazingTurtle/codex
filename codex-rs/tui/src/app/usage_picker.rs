//! Asynchronous account reads for the usage picker, isolated from session limits.
use super::*;
use crate::chatwidget::UsagePickerEvent;
use crate::chatwidget::UsageReadPurpose;
use codex_app_server_protocol::AccountListParams;
use codex_app_server_protocol::AccountListResponse;
use codex_app_server_protocol::AccountRateLimitsReadManyResponse;
use codex_app_server_protocol::AccountReadManyParams;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;

impl App {
    pub(super) async fn handle_usage_picker_event(
        &mut self,
        event: UsagePickerEvent,
        app_server: &mut AppServerSession,
    ) {
        match event {
            UsagePickerEvent::LoadAccounts { request_id } => {
                let handle = app_server.request_handle();
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let result =
                        tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 15), async {
                            let mut accounts = Vec::new();
                            let mut cursor = None;
                            loop {
                                let response: AccountListResponse = handle
                                    .request_typed(ClientRequest::AccountList {
                                        request_id: RequestId::String(format!(
                                            "usage-accounts-{}",
                                            uuid::Uuid::new_v4()
                                        )),
                                        params: AccountListParams {
                                            cursor,
                                            limit: None,
                                        },
                                    })
                                    .await?;
                                accounts.extend(response.data);
                                cursor = response.next_cursor;
                                if cursor.is_none() {
                                    break;
                                }
                            }
                            anyhow::Ok(accounts)
                        })
                        .await
                        .map_err(|err| err.to_string())
                        .and_then(|result| result.map_err(|err| err.to_string()));
                    tx.send(AppEvent::UsagePicker(UsagePickerEvent::AccountsLoaded {
                        request_id,
                        result,
                    }));
                });
            }
            UsagePickerEvent::AccountsLoaded { request_id, result } => {
                self.chat_widget.finish_usage_accounts(request_id, result)
            }
            UsagePickerEvent::OpenAccount(account) => self.chat_widget.open_usage_account(account),
            UsagePickerEvent::RefreshAccount => self.chat_widget.refresh_usage_account(),
            UsagePickerEvent::ReadAccount {
                request_id,
                account_id,
                purpose,
            } => {
                self.read_usage_picker_account(app_server, request_id, account_id, purpose);
            }
            UsagePickerEvent::AccountLoaded {
                request_id,
                account_id,
                purpose,
                result,
            } => {
                self.chat_widget
                    .finish_usage_account_read(request_id, account_id, purpose, result);
            }

            UsagePickerEvent::ShowRanges => self.chat_widget.show_usage_ranges(),
            UsagePickerEvent::ShowUsage { view, account_id } => {
                let target =
                    account_id.map(
                        |account_id| crate::chatwidget::TokenActivityTarget::Stored {
                            label: self
                                .chat_widget
                                .usage_reset_account()
                                .filter(|account| account.account_id == account_id)
                                .and_then(|account| account.email.clone())
                                .unwrap_or_else(|| account_id.clone()),
                            account_id,
                        },
                    );
                self.chat_widget.close_usage_picker();
                if let Some(target) = target {
                    self.chat_widget.add_token_activity_output(view, target);
                } else {
                    self.show_account_usage(view, /*selector*/ None, app_server)
                        .await;
                }
            }
        }
    }

    pub(super) async fn show_account_usage(
        &mut self,
        view: crate::chatwidget::TokenActivityView,
        selector: Option<String>,
        app_server: &mut AppServerSession,
    ) {
        use crate::chatwidget::TokenActivityTarget;
        use crate::chatwidget::TokenActivityView;
        if let Some(selector) = selector {
            let result = app_server
                .list_chatgpt_accounts()
                .await
                .map_err(|err| err.to_string())
                .and_then(|response| {
                    let account_id = super::event_dispatch::resolve_chatgpt_account_selector(
                        &response.data,
                        &selector,
                    )?;
                    let label = response
                        .data
                        .iter()
                        .find(|account| account.account_id == account_id)
                        .and_then(|account| account.email.clone())
                        .unwrap_or_else(|| account_id.clone());
                    Ok(TokenActivityTarget::Stored { account_id, label })
                });
            match result {
                Ok(target) => self.chat_widget.add_token_activity_output(view, target),
                Err(err) => self.chat_widget.add_error_message(err),
            }
        } else if view != TokenActivityView::Cumulative {
            self.chat_widget
                .add_token_activity_output(view, TokenActivityTarget::Active);
        } else {
            match app_server
                .read_chatgpt_account_usage(/*account_ids*/ None)
                .await
            {
                Ok(response) if response.data.is_empty() => self
                    .chat_widget
                    .add_token_activity_output(view, TokenActivityTarget::Active),
                Ok(response) => self.chat_widget.add_account_usage_output(view, response),
                Err(err) => self
                    .chat_widget
                    .add_error_message(format!("Failed to load account usage: {err}")),
            }
        }
    }

    pub(super) fn read_usage_picker_account(
        &self,
        app_server: &AppServerSession,
        request_id: u64,
        account_id: String,
        purpose: UsageReadPurpose,
    ) {
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(/*secs*/ 15),
                handle.request_typed::<AccountRateLimitsReadManyResponse>(
                    ClientRequest::AccountRateLimitsReadMany {
                        request_id: RequestId::String(format!(
                            "usage-account-{}",
                            uuid::Uuid::new_v4()
                        )),
                        params: AccountReadManyParams {
                            account_ids: Some(vec![account_id.clone()]),
                        },
                    },
                ),
            )
            .await
            .map_err(|err| err.to_string())
            .and_then(|result| result.map_err(|err| err.to_string()));
            tx.send(AppEvent::UsagePicker(UsagePickerEvent::AccountLoaded {
                request_id,
                account_id,
                purpose,
                result,
            }));
        });
    }
}
