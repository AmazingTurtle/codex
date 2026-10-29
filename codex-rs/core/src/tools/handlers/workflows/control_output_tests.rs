use super::*;
use codex_dynamic_workflows::ScriptMetadata;
use codex_dynamic_workflows::WorkflowControlOperation;
use codex_dynamic_workflows::WorkflowControlStatus;
use codex_dynamic_workflows::WorkflowControlTarget;
use codex_dynamic_workflows::WorkflowRevisionStatus;
use pretty_assertions::assert_eq;

#[test]
fn command_receipt_bounds_untrusted_text_without_echoing_its_prompt() {
    let command = WorkflowControlCommand {
        id: "guide_1".into(),
        revision_id: "revision-1".into(),
        control_revision: 7,
        created_at: 1,
        target: WorkflowControlTarget::Step {
            logical_id: "\0".repeat(2048),
            expected_attempt_id: Some("attempt-1".into()),
            expected_turn_id: Some("turn-1".into()),
        },
        operation: WorkflowControlOperation::Guide {
            message: "PRIVATE_GUIDANCE".repeat(400),
        },
        status: WorkflowControlStatus::Rejected,
        error: Some("\0".repeat(8192)),
    };
    let state = WorkflowControlState::default();
    let output = command_response(&command, &state);
    assert_eq!(
        (
            output["command"]["id"].as_str(),
            output["command"]["revision_id"].as_str(),
            output["command"]["control_revision"].as_u64()
        ),
        (Some("guide_1"), Some("revision-1"), Some(7))
    );
    let encoded = output.to_string();
    assert!(encoded.len() <= super::super::output::MAX_OUTPUT_BYTES);
    assert!(!encoded.contains("PRIVATE_GUIDANCE"));
    assert!(output["command"]["target_preview"].as_str().unwrap().len() <= 256);
    assert_eq!(
        output["state"],
        serde_json::json!({"control_revision":0,"active_revision_id":"","pending_revision_id":null,"status":"active"})
    );
}

#[test]
fn revision_receipt_omits_source_arguments_and_bounds_escaped_reason() {
    let revision = WorkflowRevision {
        id: "revision-2".into(),
        parent_id: Some("revision-1".into()),
        number: 2,
        created_at: 3,
        execution_ids: vec!["execution-1".into()],
        source: "PRIVATE_SOURCE".repeat(1000),
        input: serde_json::json!({"secret":"PRIVATE_ARGUMENTS"}),
        source_hash: "hash".into(),
        metadata: ScriptMetadata {
            name: "review".into(),
            description: "description".into(),
            phases: Vec::new(),
        },
        reusable_statements: 1,
        invalidation_reason: "\0".repeat(1024),
        statement_count: 3,
        status: WorkflowRevisionStatus::Draft,
        mode: Some(RevisionApplyMode::DrainAccepted),
        error: None,
    };
    let state = WorkflowControlState {
        control_revision: 7,
        active_revision_id: "revision-1".into(),
        pending_revision_id: Some("revision-2".into()),
        status: WorkflowRevisionStatus::Fencing,
    };
    let output = revision_response(&revision, &state);
    assert_eq!(
        output["state"],
        serde_json::json!({"control_revision":7,"active_revision_id":"revision-1","pending_revision_id":"revision-2","status":"fencing"})
    );
    assert_eq!(output["revision"]["mode"], "drain_accepted");
    let encoded = output.to_string();
    assert!(encoded.len() <= super::super::output::MAX_OUTPUT_BYTES);
    assert!(!encoded.contains("PRIVATE_SOURCE"));
    assert!(!encoded.contains("PRIVATE_ARGUMENTS"));
    assert!(
        serde_json::to_string(&output["revision"]["invalidation_reason"])
            .unwrap()
            .len()
            <= 512
    );
}
