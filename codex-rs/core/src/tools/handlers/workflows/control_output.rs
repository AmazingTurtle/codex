//! Control receipts omit prompts and source; large input is never echoed into inference.
use super::output::preview;
use codex_dynamic_workflows::RevisionApplyMode;
use codex_dynamic_workflows::WorkflowControlCommand;
use codex_dynamic_workflows::WorkflowControlState;
use codex_dynamic_workflows::WorkflowRevision;
use serde_json::Value;
use serde_json::json;

pub(super) fn command(command: &WorkflowControlCommand) -> Value {
    json!({
        "id":command.id,"revision_id":command.revision_id,"control_revision":command.control_revision,"status":command.status,
        "created_at":command.created_at,"target_preview":preview(&serde_json::to_string(&command.target).unwrap_or_default(),256),
        "error":command.error.as_deref().map(|error|preview(error,256)),
    })
}

pub(super) fn revision(revision: &WorkflowRevision) -> Value {
    json!({
        "id":revision.id,"parent_id":revision.parent_id,"number":revision.number,"created_at":revision.created_at,
        "execution_ids":revision.execution_ids.iter().take(5).collect::<Vec<_>>(),"execution_count":revision.execution_ids.len(),
        "name":revision.metadata.name,"description":preview(&revision.metadata.description,256),
        "reusable_statements":revision.reusable_statements,"statement_count":revision.statement_count,
        "invalidation_reason":preview(&revision.invalidation_reason,512),
        "status":revision.status,"mode":revision.mode.map(|mode|match mode {
            RevisionApplyMode::DrainAccepted => "drain_accepted",
            RevisionApplyMode::InterruptAndApply => "interrupt_and_apply",
        }),"error":revision.error.as_deref().map(|error|preview(error,256)),
    })
}

pub(super) fn state(state: &WorkflowControlState) -> Value {
    json!({"control_revision":state.control_revision,"active_revision_id":state.active_revision_id,
        "pending_revision_id":state.pending_revision_id,"status":state.status})
}

pub(super) fn command_response(
    command: &WorkflowControlCommand,
    state: &WorkflowControlState,
) -> Value {
    json!({"command":self::command(command),"state":self::state(state)})
}

pub(super) fn revision_response(
    revision: &WorkflowRevision,
    state: &WorkflowControlState,
) -> Value {
    json!({"revision":self::revision(revision),"state":self::state(state)})
}

#[cfg(test)]
#[path = "control_output_tests.rs"]
mod tests;
