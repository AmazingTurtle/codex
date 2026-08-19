//! Reads account status without blocking terminal input. Batched limits publish completed rows
//! while slow accounts settle; request IDs keep results tied to their originating status cards.
//! Identity refreshes also reject obsolete account-update and reconnect results.

use super::App;
use super::background_requests::RATE_LIMIT_RESET_REQUEST_TIMEOUT;
use super::event_dispatch::resolve_chatgpt_account_selector;
use crate::app_event::AppEvent;
use crate::app_event::ChatgptAccountStatusRequest;
use crate::app_server_session::AppServerSession;
use codex_app_server_protocol::Account;
use codex_app_server_protocol::AccountListParams;
use codex_app_server_protocol::AccountListResponse;
use codex_app_server_protocol::AccountRateLimitsReadManyParams;
use codex_app_server_protocol::AccountRateLimitsReadManyResponse;
use codex_app_server_protocol::AccountRateLimitsReadResult;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::GetAccountParams;
use codex_app_server_protocol::GetAccountResponse;
use codex_app_server_protocol::RequestId;
use futures::StreamExt;
use std::time::Duration;
use uuid::Uuid;

// Keep account fan-out aligned with app-server's batched account reads.
const ACCOUNT_STATUS_READ_CONCURRENCY: usize = 5;

impl App {
    pub(super) fn read_account_status(
        &self,
        app_server: &AppServerSession,
        request: ChatgptAccountStatusRequest,
    ) {
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let accounts = tokio::time::timeout(RATE_LIMIT_RESET_REQUEST_TIMEOUT, async {
                let mut accounts = Vec::new();
                let mut cursor = None;
                loop {
                    let response: AccountListResponse = handle
                        .request_typed(ClientRequest::AccountList {
                            request_id: RequestId::String(format!(
                                "status-list-{}",
                                Uuid::new_v4()
                            )),
                            params: AccountListParams {
                                cursor,
                                limit: None,
                            },
                        })
                        .await
                        .map_err(|err| err.to_string())?;
                    accounts.extend(response.data);
                    cursor = response.next_cursor;
                    if cursor.is_none() {
                        break;
                    }
                }
                match &request {
                    ChatgptAccountStatusRequest::AllAccountsStatusCard { .. } => {
                        accounts.retain(|account| account.is_eligible);
                    }
                    ChatgptAccountStatusRequest::SelectedAccount { selector, .. } => {
                        let id = resolve_chatgpt_account_selector(&accounts, selector)?;
                        accounts.retain(|account| account.account_id == id);
                    }
                }
                Ok::<_, String>(accounts)
            })
            .await
            .map_err(|_| "account list timed out".to_string())
            .and_then(std::convert::identity);
            let accounts = match accounts {
                Ok(accounts) => accounts,
                Err(error) => {
                    tx.send(AppEvent::ChatgptAccountStatusLoaded {
                        request,
                        result: Err(error),
                        pending_accounts: 0,
                    });
                    return;
                }
            };
            let mut pending_accounts = accounts.len();
            let mut response = AccountRateLimitsReadManyResponse { data: Vec::new() };
            if matches!(
                request,
                ChatgptAccountStatusRequest::AllAccountsStatusCard { .. }
            ) || pending_accounts == 0
            {
                tx.send(AppEvent::ChatgptAccountStatusLoaded {
                    request: request.clone(),
                    result: Ok(response.clone()),
                    pending_accounts,
                });
            }
            // Match app-server's account-read concurrency without head-of-line blocking:
            // each completed account becomes visible even if an earlier account stalls.
            let mut reads = futures::stream::iter(accounts)
                .map(|account| {
                    let handle = handle.clone();
                    async move {
                        let result = tokio::time::timeout(
                            RATE_LIMIT_RESET_REQUEST_TIMEOUT,
                            handle.request_typed::<AccountRateLimitsReadManyResponse>(
                                ClientRequest::AccountRateLimitsReadMany {
                                    request_id: RequestId::String(format!(
                                        "status-limits-{}",
                                        Uuid::new_v4()
                                    )),
                                    params: AccountRateLimitsReadManyParams {
                                        account_ids: Some(vec![account.account_id.clone()]),
                                        supports_luna_reserve: true,
                                        exclude_reset_credit_details: false,
                                    },
                                },
                            ),
                        )
                        .await
                        .map_err(|_| "account limits timed out".to_string())
                        .and_then(|result| result.map_err(|err| err.to_string()))
                        .and_then(|response| {
                            response
                                .data
                                .into_iter()
                                .find(|result| result.account.account_id == account.account_id)
                                .ok_or_else(|| "account limits were not returned".to_string())
                        });
                        result.unwrap_or_else(|error| AccountRateLimitsReadResult {
                            account,
                            rate_limits: None,
                            error: Some(error),
                        })
                    }
                })
                .buffer_unordered(ACCOUNT_STATUS_READ_CONCURRENCY);
            while let Some(result) = reads.next().await {
                response.data.push(result);
                pending_accounts -= 1;
                tx.send(AppEvent::ChatgptAccountStatusLoaded {
                    request: request.clone(),
                    result: Ok(response.clone()),
                    pending_accounts,
                });
            }
        });
    }

    pub(super) fn refresh_account_email(&mut self, app_server: &AppServerSession) {
        let request_id = Uuid::new_v4();
        self.account_email_request_id = Some(request_id);
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(/*secs*/ 30),
                request_handle.request_typed::<GetAccountResponse>(ClientRequest::GetAccount {
                    request_id: RequestId::String(format!("account-status-{request_id}")),
                    params: GetAccountParams {
                        refresh_token: false,
                    },
                }),
            )
            .await;
            match result {
                Ok(Ok(response)) => {
                    if let Some(Account::Chatgpt { email, .. }) = response.account {
                        app_event_tx.send(AppEvent::AccountEmailLoaded { request_id, email });
                    }
                }
                Ok(Err(_)) | Err(_) => {
                    tracing::warn!("failed to read account identity after account update");
                }
            }
        });
    }
}

#[cfg(test)]
#[path = "account_status_tests.rs"]
mod tests;
