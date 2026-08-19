//! Shared rendering for rate-limit rows in status cards.

use std::collections::BTreeSet;

use ratatui::prelude::*;
use ratatui::style::Stylize;

use crate::line_truncation::line_width;

use super::format::FieldFormatter;
use super::format::push_label;
use super::rate_limits::StatusRateLimitData;
use super::rate_limits::StatusRateLimitRow;
use super::rate_limits::StatusRateLimitValue;
use super::rate_limits::format_status_limit_summary;
use super::rate_limits::render_status_limit_progress_bar;

pub(super) fn render_rate_limit_lines(
    rate_limits: &StatusRateLimitData,
    refreshing_rate_limits: bool,
    available_inner_width: usize,
    formatter: &FieldFormatter,
) -> Vec<Line<'static>> {
    match rate_limits {
        StatusRateLimitData::Available(rows) => {
            if rows.is_empty() {
                return vec![formatter.line(
                    "Limits",
                    vec![Span::from("not available for this account").dim()],
                )];
            }

            render_rate_limit_rows(rows, available_inner_width, formatter)
        }
        StatusRateLimitData::Stale(rows) => {
            let mut lines = render_rate_limit_rows(rows, available_inner_width, formatter);
            lines.push(formatter.line(
                "Warning",
                vec![Span::from(if refreshing_rate_limits {
                    "limits may be stale - run /status again shortly."
                } else {
                    "limits may be stale - start new turn to refresh."
                })
                .dim()],
            ));
            lines
        }
        StatusRateLimitData::Unavailable => {
            vec![formatter.line(
                "Limits",
                vec![Span::from("not available for this account").dim()],
            )]
        }
        StatusRateLimitData::Missing => {
            vec![formatter.line(
                "Limits",
                vec![Span::from(if refreshing_rate_limits {
                    "refresh requested; run /status again shortly."
                } else {
                    "data not available yet"
                })
                .dim()],
            )]
        }
    }
}

pub(super) fn collect_rate_limit_labels(
    rate_limits: &StatusRateLimitData,
    seen: &mut BTreeSet<String>,
    labels: &mut Vec<String>,
) {
    match rate_limits {
        StatusRateLimitData::Available(rows) => {
            if rows.is_empty() {
                push_label(labels, seen, "Limits");
            } else {
                for row in rows {
                    push_label(labels, seen, row.label.as_str());
                }
            }
        }
        StatusRateLimitData::Stale(rows) => {
            for row in rows {
                push_label(labels, seen, row.label.as_str());
            }
            push_label(labels, seen, "Warning");
        }
        StatusRateLimitData::Unavailable | StatusRateLimitData::Missing => {
            push_label(labels, seen, "Limits");
        }
    }
}

fn render_rate_limit_rows(
    rows: &[StatusRateLimitRow],
    available_inner_width: usize,
    formatter: &FieldFormatter,
) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(rows.len().saturating_mul(2));

    for row in rows {
        match &row.value {
            StatusRateLimitValue::Window {
                percent_used,
                resets_at,
                details,
            } => {
                let percent_remaining = (100.0 - percent_used).clamp(0.0, 100.0);
                let summary = format_status_limit_summary(percent_remaining);
                let full_value_spans = vec![
                    Span::from(render_status_limit_progress_bar(percent_remaining)),
                    Span::from(" "),
                    Span::from(summary.clone()),
                ];
                // On narrow terminals, keep the percentage visible rather than
                // letting the fixed-width progress bar crowd out the reset time.
                let value_spans = if line_width(&Line::from(full_value_spans.clone()))
                    <= formatter.value_width(available_inner_width)
                {
                    full_value_spans
                } else {
                    vec![Span::from(summary)]
                };
                let base_spans = formatter.full_spans(row.label.as_str(), value_spans);
                let base_line = Line::from(base_spans.clone());

                if let Some(resets_at) = resets_at.as_ref() {
                    let resets_span = Span::from(format!("(resets {resets_at})")).dim();
                    let mut inline_spans = base_spans.clone();
                    inline_spans.push(Span::from(" ").dim());
                    inline_spans.push(resets_span);

                    if line_width(&Line::from(inline_spans.clone())) <= available_inner_width {
                        lines.push(Line::from(inline_spans));
                    } else {
                        lines.push(base_line);
                        let reset_text = format!("(resets {resets_at})");
                        let reset_width = formatter.value_width(available_inner_width).max(1);
                        let wrap_options = textwrap::Options::new(reset_width).break_words(false);
                        lines.extend(
                            textwrap::wrap(reset_text.as_str(), wrap_options)
                                .into_iter()
                                .map(|wrapped| {
                                    formatter
                                        .continuation(vec![Span::from(wrapped.into_owned()).dim()])
                                }),
                        );
                    }
                } else {
                    lines.push(base_line);
                }
                if let Some(details) = details {
                    let detail_width = formatter.value_width(available_inner_width).max(1);
                    let wrap_options = textwrap::Options::new(detail_width).break_words(false);
                    lines.extend(
                        textwrap::wrap(details.as_str(), wrap_options)
                            .into_iter()
                            .map(|wrapped| {
                                formatter.continuation(vec![Span::from(wrapped.into_owned()).dim()])
                            }),
                    );
                }
            }
            StatusRateLimitValue::Text(text) => {
                let spans =
                    formatter.full_spans(row.label.as_str(), vec![Span::from(text.clone())]);
                lines.push(Line::from(spans));
            }
        }
    }

    lines
}
