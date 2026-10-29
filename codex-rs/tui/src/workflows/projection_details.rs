//! Overview metadata follows accepted attempt identity and authoritative accounting snapshots.
use super::*;
use codex_app_server_protocol::WorkflowAdmissionStatus;
use codex_app_server_protocol::WorkflowStep;
use codex_app_server_protocol::WorkflowStepDisposition;

fn model(step: &WorkflowStep, index: usize) -> String {
    let Some(attempt) = step.attempts.get(index) else {
        return "Unknown".into();
    };
    match (&attempt.resolved_model, &attempt.reasoning_effort) {
        (Some(model), Some(reasoning)) => format!("{model} ({reasoning})"),
        (Some(model), None) => format!("{model} (reasoning unknown)"),
        (None, Some(reasoning)) => format!("Unknown ({reasoning})"),
        (None, None) => "Unknown".into(),
    }
}

fn accepted_attempt(step: &WorkflowStep, index: usize) -> Option<String> {
    let attempt = step.attempts.get(index)?;
    if attempt.admission != WorkflowAdmissionStatus::Accepted {
        return None;
    }
    let number = step.attempts[..=index]
        .iter()
        .filter(|attempt| attempt.admission == WorkflowAdmissionStatus::Accepted)
        .count();
    let suffix = match (number % 100, number % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    Some(format!("{number}{suffix} attempt"))
}

pub(super) fn overview(step: &WorkflowStep, label: &str, index: Option<usize>) -> WorkflowOverview {
    let selected = index.and_then(|index| step.attempts.get(index));
    let historical = index.is_some_and(|index| index + 1 < step.attempts.len());
    let state = if historical {
        selected.map_or("Unknown", |attempt| super::status(attempt.status))
    } else {
        super::step_status(step)
    };
    let status = index
        .and_then(|index| accepted_attempt(step, index))
        .map_or_else(|| state.into(), |attempt| format!("{state} ({attempt})"));
    let options = step.effective_options.as_ref().unwrap_or(&step.options);
    let mut metadata = vec![
        ("Status".into(), status),
        (
            "Model".into(),
            index.map_or_else(|| "Unknown".into(), |index| model(step, index)),
        ),
    ];
    if let Some(agent_type) = options.get("agentType").and_then(serde_json::Value::as_str) {
        metadata.push(("Agent type".into(), agent_type.into()));
    }
    let skipped = step.disposition == WorkflowStepDisposition::Skipped;
    WorkflowOverview {
        title: if historical {
            format!("{label} · Attempt {}", index.unwrap_or(0) + 1)
        } else {
            label.into()
        },
        metadata,
        prompt: options
            .get("message")
            .or_else(|| options.get("prompt"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        metrics: Some(if historical {
            selected
                .map(|attempt| attempt.metrics.clone())
                .unwrap_or_default()
        } else {
            step.metrics.clone()
        }),
        result: if historical {
            None
        } else if skipped {
            step.control_outcome
                .as_ref()
                .map(|outcome| match &outcome.replacement {
                    codex_app_server_protocol::WorkflowResult::Absent => serde_json::Value::Null,
                    codex_app_server_protocol::WorkflowResult::Value { value } => value.clone(),
                })
        } else {
            step.result.clone()
        },
        result_label: (!historical && skipped).then(|| "Replacement".into()),
        error: selected.and_then(|attempt| attempt.error.clone()),
        ..Default::default()
    }
}
