use super::*;
use chrono::Utc;
use codex_state::AccountLimitObservation;

impl AccountRequestProcessor {
    pub(super) async fn record_account_rate_limits(
        &self,
        auth: &CodexAuth,
        response: &GetAccountRateLimitsResponse,
    ) {
        let Some(state_db) = self.state_db.as_ref() else {
            return;
        };
        let Some(account_id) = auth
            .get_account_id()
            .or_else(|| response.account_id.clone())
        else {
            tracing::warn!("not recording account rate limits without an account id");
            return;
        };
        let captured_at = Utc::now();
        let snapshots = response
            .rate_limits_by_limit_id
            .as_ref()
            .map(|snapshots| {
                snapshots
                    .iter()
                    .map(|(limit_id, snapshot)| (limit_id.clone(), snapshot))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![("codex".to_string(), &response.rate_limits)]);
        let mut observations = Vec::new();
        for (map_limit_id, snapshot) in snapshots {
            let limit_id = snapshot.limit_id.as_deref().unwrap_or(&map_limit_id);
            for window in [snapshot.primary.as_ref(), snapshot.secondary.as_ref()]
                .into_iter()
                .flatten()
            {
                observations.push(AccountLimitObservation {
                    captured_at,
                    account_id: account_id.clone(),
                    limit_id: limit_id.to_string(),
                    window_seconds: window
                        .window_duration_mins
                        .and_then(|minutes| u64::try_from(minutes).ok())
                        .and_then(|minutes| minutes.checked_mul(60)),
                    used_percent: Some(f64::from(window.used_percent)),
                    resets_at: window
                        .resets_at
                        .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0)),
                    source: "account/rateLimits/read".to_string(),
                });
            }
        }
        if let Err(err) = state_db
            .record_account_limit_observations(&observations)
            .await
        {
            tracing::warn!("failed to record account rate limits: {err}");
        }
    }
}
