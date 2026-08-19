//! Multi-account rate-limit state and rendering for `/status all`.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::RwLock;

use chrono::DateTime;
use chrono::Local;
use codex_app_server_protocol::AccountRateLimitsReadManyResponse;
use ratatui::prelude::*;
use ratatui::style::Stylize;

use crate::app_server_session::app_server_rate_limit_snapshots;

use super::format::FieldFormatter;
use super::format::push_label;
use super::helpers::plan_type_display_name;
use super::rate_limit_rows::collect_rate_limit_labels;
use super::rate_limit_rows::render_rate_limit_lines;
use super::rate_limits::StatusRateLimitData;
use super::rate_limits::compose_rate_limit_data_many;
use super::rate_limits::rate_limit_snapshot_display_for_limit;

#[derive(Debug, Clone)]
pub(super) struct StatusAccountLimits {
    state: Arc<RwLock<Option<StatusAccountLimitsState>>>,
}

#[derive(Debug, Clone)]
enum StatusAccountLimitsState {
    Pending,
    Available(Vec<StatusAccountLimitDisplay>),
    Failed(String),
}

#[derive(Debug, Clone)]
struct StatusAccountLimitDisplay {
    label: String,
    plan: String,
    is_active: bool,
    rate_limits: Option<StatusRateLimitData>,
    error: Option<String>,
}

impl StatusAccountLimits {
    pub(super) fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(None)),
        }
    }

    pub(super) fn start_refresh(&self) {
        #[expect(clippy::expect_used)]
        let mut state = self
            .state
            .write()
            .expect("status history account-limit state poisoned");
        *state = Some(StatusAccountLimitsState::Pending);
    }

    pub(super) fn finish_refresh(
        &self,
        response: AccountRateLimitsReadManyResponse,
        captured_at: DateTime<Local>,
    ) {
        let mut accounts = response
            .data
            .into_iter()
            .map(|result| {
                let account = result.account;
                let label = account.email.unwrap_or_else(|| account.account_id.clone());
                let rate_limits = result.rate_limits.map(|response| {
                    let snapshots = app_server_rate_limit_snapshots(response.into())
                        .into_iter()
                        .map(|snapshot| {
                            let limit_id = snapshot
                                .limit_id
                                .clone()
                                .unwrap_or_else(|| "codex".to_string());
                            let limit_label = snapshot
                                .limit_name
                                .clone()
                                .unwrap_or_else(|| limit_id.clone());
                            rate_limit_snapshot_display_for_limit(
                                &snapshot,
                                limit_label,
                                captured_at,
                            )
                        })
                        .collect::<Vec<_>>();
                    compose_rate_limit_data_many(&snapshots, captured_at)
                });
                StatusAccountLimitDisplay {
                    label,
                    plan: plan_type_display_name(account.plan_type),
                    is_active: account.is_active,
                    rate_limits,
                    error: result.error,
                }
            })
            .collect::<Vec<_>>();
        accounts.sort_by_key(|account| !account.is_active);

        #[expect(clippy::expect_used)]
        let mut state = self
            .state
            .write()
            .expect("status history account-limit state poisoned");
        *state = Some(StatusAccountLimitsState::Available(accounts));
    }

    pub(super) fn fail_refresh(&self, error: String) {
        #[expect(clippy::expect_used)]
        let mut state = self
            .state
            .write()
            .expect("status history account-limit state poisoned");
        *state = Some(StatusAccountLimitsState::Failed(error));
    }

    pub(super) fn is_enabled(&self) -> bool {
        #[expect(clippy::expect_used)]
        self.state
            .read()
            .expect("status history account-limit state poisoned")
            .is_some()
    }

    pub(super) fn collect_labels(&self, seen: &mut BTreeSet<String>, labels: &mut Vec<String>) {
        #[expect(clippy::expect_used)]
        let state = self
            .state
            .read()
            .expect("status history account-limit state poisoned");
        let Some(state) = state.as_ref() else {
            return;
        };

        push_label(labels, seen, "Account Limits");
        if let StatusAccountLimitsState::Available(accounts) = state {
            for account in accounts {
                if let Some(rate_limits) = account.rate_limits.as_ref() {
                    collect_rate_limit_labels(rate_limits, seen, labels);
                } else {
                    push_label(labels, seen, "Limits");
                }
            }
        }
    }

    pub(super) fn lines(
        &self,
        available_inner_width: usize,
        formatter: &FieldFormatter,
    ) -> Vec<Line<'static>> {
        #[expect(clippy::expect_used)]
        let state = self
            .state
            .read()
            .expect("status history account-limit state poisoned");
        let Some(state) = state.as_ref() else {
            return Vec::new();
        };

        match state {
            StatusAccountLimitsState::Pending => {
                vec![formatter.line("Account Limits", vec!["loading…".dim()])]
            }
            StatusAccountLimitsState::Failed(error) => dimmed_field_lines(
                "Account Limits",
                format!("unavailable — {error}"),
                available_inner_width,
                formatter,
            ),
            StatusAccountLimitsState::Available(accounts) if accounts.is_empty() => {
                vec![formatter.line(
                    "Account Limits",
                    vec!["no eligible managed ChatGPT accounts".dim()],
                )]
            }
            StatusAccountLimitsState::Available(accounts) => {
                let mut lines = Vec::new();
                for (index, account) in accounts.iter().enumerate() {
                    if index > 0 {
                        lines.push(Line::default());
                    }

                    let active = if account.is_active { " · active" } else { "" };
                    let header = vec![
                        account.label.clone().bold(),
                        format!(" — {}{active}", account.plan).dim(),
                    ];
                    if index == 0 {
                        lines.push(formatter.line("Account Limits", header));
                    } else {
                        lines.push(
                            vec![Span::from(FieldFormatter::INDENT).dim()]
                                .into_iter()
                                .chain(header)
                                .collect::<Vec<_>>()
                                .into(),
                        );
                    }

                    if let Some(rate_limits) = account.rate_limits.as_ref() {
                        lines.extend(render_rate_limit_lines(
                            rate_limits,
                            /*refreshing_rate_limits*/ false,
                            available_inner_width,
                            formatter,
                        ));
                    } else {
                        let error = account
                            .error
                            .as_deref()
                            .unwrap_or("rate limits were not returned");
                        lines.extend(dimmed_field_lines(
                            "Limits",
                            format!("unavailable — {error}"),
                            available_inner_width,
                            formatter,
                        ));
                    }
                }
                lines
            }
        }
    }
}

fn dimmed_field_lines(
    label: &'static str,
    value: String,
    available_inner_width: usize,
    formatter: &FieldFormatter,
) -> Vec<Line<'static>> {
    let value_width = formatter.value_width(available_inner_width).max(1);
    let wrapped = textwrap::wrap(
        value.as_str(),
        textwrap::Options::new(value_width).break_words(false),
    );
    let mut wrapped = wrapped.into_iter();
    let Some(first) = wrapped.next() else {
        return vec![formatter.line(label, vec![Span::from(value).dim()])];
    };
    let mut lines = vec![formatter.line(label, vec![Span::from(first.into_owned()).dim()])];
    lines.extend(
        wrapped.map(|line| formatter.continuation(vec![Span::from(line.into_owned()).dim()])),
    );
    lines
}
