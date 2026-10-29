//! Shared compact presentation of authoritative workflow phase and usage snapshots.
use super::WorkflowRow;
use codex_app_server_protocol::WorkflowPhaseStatus;

pub(super) fn phase_counts(row: &WorkflowRow) -> Option<(usize, usize)> {
    let metadata = row.metadata.as_ref()?;
    let total = metadata.phases.len();
    if total == 0 {
        return None;
    }
    let finished = metadata
        .phases
        .iter()
        .filter(|phase| {
            row.phase_progress
                .iter()
                .find(|progress| progress.title == phase.title)
                .is_some_and(|progress| {
                    matches!(
                        progress.status,
                        WorkflowPhaseStatus::Completed | WorkflowPhaseStatus::Skipped
                    )
                })
        })
        .count();
    Some((finished, total))
}

pub(super) fn progress(row: &WorkflowRow, compact: bool) -> String {
    match phase_counts(row) {
        Some((finished, total)) => {
            if compact {
                format!("{finished}/{total}p")
            } else {
                format!("{finished}/{total} phases")
            }
        }
        None if row.discovered == 0 => "No agent steps".into(),
        None => {
            let steps = row.completed.map_or_else(
                || format!("{} steps", row.discovered),
                |completed| format!("{completed}/{} steps", row.discovered),
            );
            if compact {
                steps
            } else {
                format!("indeterminate · {steps}")
            }
        }
    }
}

pub(super) fn bar(row: &WorkflowRow) -> String {
    let Some((finished, total)) = phase_counts(row) else {
        return "[········]".into();
    };
    let filled = finished.saturating_mul(8) / total;
    format!("[{}{}]", "■".repeat(filled), "□".repeat(8 - filled))
}

pub(super) fn usage(row: &WorkflowRow, compact: bool) -> String {
    let metrics = row.metrics.as_ref();
    let (input, output) = metrics
        .and_then(|metrics| metrics.usage.as_ref())
        .map_or_else(
            || ("?".into(), "?".into()),
            |usage| {
                let prefix = if metrics.is_some_and(|metrics| metrics.usage_complete) {
                    ""
                } else {
                    "≥"
                };
                let token = |count| {
                    format!(
                        "{prefix}{}",
                        crate::status::format_tokens_compact(
                            i64::try_from(count).unwrap_or(i64::MAX)
                        )
                    )
                };
                (token(usage.input_tokens), token(usage.output_tokens))
            },
        );
    if compact {
        format!("I {input}/O {output}")
    } else {
        format!("in {input} · out {output}")
    }
}

pub(super) fn elapsed(row: &WorkflowRow) -> Option<String> {
    row.metrics.as_ref()?.active_duration_ms.map(|duration| {
        codex_utils_elapsed::format_duration(std::time::Duration::from_millis(duration))
    })
}

pub(super) fn grouped_count(value: u64) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push('.');
        }
        result.push(digit);
    }
    result
}

pub(super) fn overview_metrics(
    metrics: &codex_app_server_protocol::WorkflowMetrics,
) -> Vec<(String, String)> {
    let prefix = if metrics.usage_complete { "" } else { "≥" };
    let count = |value: Option<u64>| {
        value.map_or_else(
            || "?".into(),
            |value| format!("{prefix}{}", grouped_count(value)),
        )
    };
    let turns = count(metrics.model_turns);
    let agents = metrics
        .agents_used
        .filter(|value| *value != 1)
        .map_or_else(String::new, |value| {
            format!(" across {} agents", grouped_count(u64::from(value)))
        });
    let tokens = format!(
        "{} in, {} out ({turns} model turns{agents})",
        count(metrics.usage.as_ref().map(|usage| usage.input_tokens)),
        count(metrics.usage.as_ref().map(|usage| usage.output_tokens))
    );
    let mut fields = vec![("Tokens".into(), tokens)];
    if let Some(usage) = &metrics.usage
        && usage.cached_input_tokens > 0
    {
        fields.push((
            "Cached input".into(),
            format!(
                "{} (included in input)",
                count(Some(usage.cached_input_tokens))
            ),
        ));
    }
    if let Some(duration) = metrics.active_duration_ms {
        fields.push((
            "Active time".into(),
            codex_utils_elapsed::format_duration(std::time::Duration::from_millis(duration)),
        ));
    }
    if let Some(reason) = &metrics.usage_incomplete_reason {
        fields.push(("Usage coverage".into(), reason.clone()));
    }
    fields
}
