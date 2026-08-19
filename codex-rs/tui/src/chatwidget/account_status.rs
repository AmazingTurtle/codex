use super::*;
use chrono::Duration;
use chrono::NaiveDate;
use chrono::Utc;
use codex_app_server_protocol::AccountTokenUsageDailyBucket;

impl ChatWidget {
    pub(crate) fn add_account_status_output(
        &mut self,
        response: AccountRateLimitsReadManyResponse,
    ) {
        let mut lines = vec!["ChatGPT account status".bold().into()];
        for result in response.data {
            let label = result
                .account
                .email
                .clone()
                .unwrap_or_else(|| result.account.account_id.clone());
            let plan = crate::status::plan_type_display_name(result.account.plan_type);
            let active = if result.account.is_active {
                " · active"
            } else {
                ""
            };
            lines.push(
                vec![
                    "  ".into(),
                    label.bold(),
                    format!(" — {plan}{active}").dim(),
                ]
                .into(),
            );
            if let Some(rate_limits) = result.rate_limits {
                for window in [
                    ("Primary", rate_limits.rate_limits.primary),
                    ("Secondary", rate_limits.rate_limits.secondary),
                ] {
                    if let (name, Some(window)) = window {
                        let duration = window
                            .window_duration_mins
                            .map(|minutes| format!(" / {minutes}m window"))
                            .unwrap_or_default();
                        lines.push(
                            format!("    {name}: {}% used{duration}", window.used_percent).into(),
                        );
                    }
                }
            } else if let Some(error) = result.error {
                lines.push(vec!["    unavailable: ".dim(), error.dim()].into());
            }
        }
        self.add_plain_history_lines(lines);
    }

    pub(crate) fn add_account_usage_output(
        &mut self,
        view: TokenActivityView,
        response: AccountUsageReadManyResponse,
    ) {
        let mut lines = vec![
            format!("ChatGPT account usage · {}", view.as_str())
                .bold()
                .into(),
        ];
        let today = Utc::now().date_naive();
        let mut accumulated = 0_i64;
        let mut successful = 0_usize;
        for result in response.data {
            let label = result
                .account
                .email
                .clone()
                .unwrap_or_else(|| result.account.account_id.clone());
            if let Some(usage) = result.usage {
                let tokens = match view {
                    TokenActivityView::Daily => usage
                        .daily_usage_buckets
                        .as_deref()
                        .map(|buckets| tokens_in_date_range(buckets, today, today)),
                    TokenActivityView::Weekly => {
                        usage.daily_usage_buckets.as_deref().map(|buckets| {
                            tokens_in_date_range(buckets, today - Duration::days(6), today)
                        })
                    }
                    TokenActivityView::Cumulative => usage.summary.lifetime_tokens,
                };
                if let Some(tokens) = tokens {
                    accumulated = accumulated.saturating_add(tokens);
                    successful += 1;
                    lines.push(
                        vec![
                            "  ".into(),
                            label.bold(),
                            format!(" — {} tokens", format_tokens_compact(tokens)).into(),
                        ]
                        .into(),
                    );
                } else {
                    lines.push(vec!["  ".into(), label.bold(), " — unavailable".dim()].into());
                }
            } else {
                let error = result.error.unwrap_or_else(|| "unavailable".to_string());
                lines.push(vec!["  ".into(), label.bold(), " — ".into(), error.dim()].into());
            }
        }
        if successful > 1 {
            lines.push("".into());
            lines.push(
                vec![
                    "  Accumulated: ".bold(),
                    format!("{} tokens", format_tokens_compact(accumulated)).bold(),
                ]
                .into(),
            );
        }
        self.add_plain_history_lines(lines);
    }
}

pub(in crate::chatwidget) fn tokens_in_date_range(
    buckets: &[AccountTokenUsageDailyBucket],
    start: NaiveDate,
    end: NaiveDate,
) -> i64 {
    buckets
        .iter()
        .filter_map(|bucket| {
            let date = NaiveDate::parse_from_str(&bucket.start_date, "%Y-%m-%d").ok()?;
            (date >= start && date <= end).then_some(bucket.tokens.max(0))
        })
        .fold(0_i64, i64::saturating_add)
}
