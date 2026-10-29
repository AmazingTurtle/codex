//! Status pages expose control identities without copying authored or returned bodies.

use super::control_output;
use super::output::bounded_page;
use super::output::preview;
use super::output::summary;
use crate::workflows::DynamicWorkflowService;
use crate::workflows::WorkflowRun;
use serde_json::Value;
use serde_json::json;

pub(super) fn content(
    service: &DynamicWorkflowService,
    run: &WorkflowRun,
    section: super::ReadSection,
    execution_id: Option<&str>,
    offset: usize,
) -> Result<Value, String> {
    if let Some(execution_id) = execution_id {
        let execution = service.read_execution(&run.id, execution_id)?;
        execution_content(
            &run.id,
            &execution.execution_id,
            section,
            &execution.source,
            &execution.input,
            offset,
        )
    } else {
        execution_content(
            &run.id,
            &run.execution_id,
            section,
            &run.source,
            &run.input,
            offset,
        )
    }
}

fn execution_content(
    run_id: &str,
    execution_id: &str,
    section: super::ReadSection,
    source: &str,
    input: &Value,
    offset: usize,
) -> Result<Value, String> {
    let (section, encoding, page) = match section {
        // Raw JavaScript can expand sixfold when JSON escapes control characters.
        // Match saved-source pages so the complete response retains room for its headers.
        super::ReadSection::Source => (
            "source",
            "javascript",
            super::output::content_page(source, offset, 384)?,
        ),
        super::ReadSection::Args => (
            "args",
            "json",
            super::output::content_page(&input.to_string(), offset, 768)?,
        ),
        super::ReadSection::Metadata | super::ReadSection::Result => {
            return Err("select source or args for an execution content page".into());
        }
    };
    Ok(
        json!({"run_id":run_id,"execution_id":execution_id,"section":section,"encoding":encoding,"page":page}),
    )
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;

pub(super) async fn metadata(
    service: &DynamicWorkflowService,
    run: &WorkflowRun,
    offset: usize,
) -> Result<Value, String> {
    let current = run.format_version == codex_dynamic_workflows::WORKFLOW_FORMAT_VERSION;
    let control = current.then(|| control_output::state(&run.steering.state));
    let mut steps = Vec::new();
    for step in run.steps.values().skip(offset).take(5) {
        let latest = step.attempts.last();
        let current_owned_turn_id = if current && let Some(attempt) = latest {
            service
                .active_owned_turn(&run.id, &step.logical_id, &attempt.id)
                .await?
        } else {
            None
        };
        steps.push(json!({
            "id":step.id,"logical_id":step.logical_id,"scope":step.scope,"key":step.key,
            "disposition":step.disposition,"attempts":step.attempts.len(),
            "current_owned_turn_id":current_owned_turn_id,"has_queued_override":step.effective_options.is_some(),
            "control_outcome":step.control_outcome.as_ref().map(|outcome|json!({"command_id":outcome.command_id,"revision_id":outcome.revision_id})),
            "latest":latest.map(|attempt|json!({
                "id":attempt.id,"child_thread_id":attempt.child_thread_id,"status":attempt.status,
                "owned_turn_ids":attempt.owned_turn_ids.iter().take(5).collect::<Vec<_>>(),"owned_turn_count":attempt.owned_turn_ids.len(),
                "final_turn_id":attempt.final_turn_id,"resolved_model":attempt.resolved_model,
                "reasoning_effort":attempt.reasoning_effort,"error":attempt.error.as_deref().map(|text|preview(text,128)),
            })),
        }));
    }
    let has_more = offset.saturating_add(steps.len()) < run.steps.len();
    bounded_page(
        json!({"run":summary(run),"control":control,"result_available":run.result.is_some()}),
        "steps",
        offset,
        steps,
        has_more,
    )
}
