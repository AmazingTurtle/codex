//! Readable native paths retain durable ordinals separately from opaque identity.

use codex_dynamic_workflows::AgentRequest;

pub(super) fn task_name(request: &AgentRequest) -> Result<String, String> {
    if request.run_sequence == 0 {
        return Ok(legacy_task_name(request));
    }
    if !(1..=codex_dynamic_workflows::MAX_STEPS as u16).contains(&request.step_sequence)
        || !(1..=16).contains(&request.attempt_number)
    {
        return Err("invalid workflow worker ordinal".into());
    }
    let suffix = format!("_s{}_a{}", request.step_sequence, request.attempt_number);
    let prefix = format!("workflow_r{}_", request.run_sequence);
    let label_limit = 24.min(64 - prefix.len() - suffix.len());
    let mut label = String::new();
    for character in request.label.chars() {
        let character = character.to_ascii_lowercase();
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            label.push(character);
        } else if !label.is_empty() && !label.ends_with('_') {
            label.push('_');
        }
        if label.len() == label_limit {
            break;
        }
    }
    let label = label.trim_end_matches('_');
    let label = if label.is_empty() { "step" } else { label };
    Ok(format!("{prefix}{label}{suffix}"))
}

pub(super) fn legacy_task_name(request: &AgentRequest) -> String {
    format!(
        "{}{}_{}",
        super::CHILD_NAME_PREFIX,
        request.run_id.replace('-', ""),
        request.attempt_id.replace('-', "")
    )
}

#[cfg(test)]
#[path = "worker_name_tests.rs"]
mod tests;
