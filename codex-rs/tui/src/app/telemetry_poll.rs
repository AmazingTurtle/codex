//! Best-effort all-account sampling while the TUI has active turns.
//!
//! The shared database claims each five-minute interval before issuing HTTP, so two TUIs
//! cannot multiply polling traffic. A failed or interrupted sample is retried next interval.

use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::AccountRateLimitsReadManyParams;
use codex_app_server_protocol::AccountRateLimitsReadManyResponse;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_rollout::StateDbHandle;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

const POLL_INTERVAL: Duration = Duration::from_secs(/*secs*/ 300);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 60);

pub(super) struct AccountLimitPoller {
    pub(super) next_attempt: Instant,
    pending: Option<JoinHandle<()>>,
}

impl Default for AccountLimitPoller {
    fn default() -> Self {
        Self {
            next_attempt: Instant::now(),
            pending: None,
        }
    }
}

impl AccountLimitPoller {
    pub(super) fn start_if_due(
        &mut self,
        state: StateDbHandle,
        request_handle: AppServerRequestHandle,
    ) {
        if Instant::now() < self.next_attempt {
            return;
        }
        self.next_attempt = Instant::now() + POLL_INTERVAL;
        if self
            .pending
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return;
        }
        self.pending = Some(tokio::spawn(async move {
            let claimed = state
                .try_claim_account_limit_poll(chrono::Utc::now(), chrono::Duration::minutes(5))
                .await;
            match claimed {
                Ok(true) => {}
                Ok(false) => return,
                Err(error) => {
                    tracing::warn!(%error, "could not claim account telemetry poll");
                    return;
                }
            }
            let request = request_handle.request_typed::<AccountRateLimitsReadManyResponse>(
                ClientRequest::AccountRateLimitsReadMany {
                    request_id: RequestId::String(format!("telemetry-poll-{}", Uuid::new_v4())),
                    params: AccountRateLimitsReadManyParams {
                        account_ids: None,
                        supports_luna_reserve: true,
                        exclude_reset_credit_details: true,
                    },
                },
            );
            match tokio::time::timeout(REQUEST_TIMEOUT, request).await {
                Ok(Ok(response)) => {
                    let failures = response
                        .data
                        .iter()
                        .filter(|entry| entry.error.is_some())
                        .count();
                    if failures > 0 {
                        tracing::warn!(failures, "some account telemetry reads failed");
                    }
                }
                Ok(Err(error)) => tracing::warn!(%error, "account telemetry poll failed"),
                Err(_) => tracing::warn!("account telemetry poll timed out"),
            }
        }));
    }
}

impl Drop for AccountLimitPoller {
    fn drop(&mut self) {
        if let Some(task) = &self.pending {
            task.abort();
        }
    }
}

#[cfg(test)]
#[path = "telemetry_poll_tests.rs"]
mod tests;
