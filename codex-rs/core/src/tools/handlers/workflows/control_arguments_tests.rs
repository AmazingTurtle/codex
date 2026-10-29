use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn guidance_keeps_explicit_revision_attempt_and_owned_turn_guards() {
    let args = json!({"action":"guide","run_id":"run-1","command_id":"guide_1","expected_control_revision":7,
        "target":{"type":"step","logical_id":"root/key:review","expected_attempt_id":"attempt-2","expected_turn_id":"turn-3"},"message":"Focus on changed files"});
    let ControlCommand::Submit { run_id, request } = parse_control(&args.to_string()).unwrap()
    else {
        panic!("guidance must produce a guarded command");
    };
    assert_eq!(
        (run_id, request),
        (
            "run-1".into(),
            WorkflowControlRequest {
                command_id: "guide_1".into(),
                expected_control_revision: 7,
                target: WorkflowControlTarget::Step {
                    logical_id: "root/key:review".into(),
                    expected_attempt_id: Some("attempt-2".into()),
                    expected_turn_id: Some("turn-3".into())
                },
                operation: WorkflowControlOperation::Guide {
                    message: "Focus on changed files".into()
                },
            }
        )
    );
    for missing in ["command_id", "expected_control_revision"] {
        let mut invalid = args.clone();
        invalid.as_object_mut().unwrap().remove(missing);
        assert!(
            parse_control(&invalid.to_string()).is_err(),
            "a mutation must not infer {missing}"
        );
    }
    let mut unknown = args;
    unknown["expectedControlRevision"] = json!(7);
    assert!(
        parse_control(&unknown.to_string()).is_err(),
        "misspelled extra fields must not be silently accepted"
    );
}

#[test]
fn stage_arguments_and_skip_replacements_keep_absence_distinct_from_null() {
    for (field, expected) in [
        (None, WorkflowResult::Absent),
        (
            Some(Value::Null),
            WorkflowResult::Value { value: Value::Null },
        ),
        (
            Some(json!({"task":"check"})),
            WorkflowResult::Value {
                value: json!({"task":"check"}),
            },
        ),
    ] {
        let mut revision = json!({"action":"stage","run_id":"run","command_id":"stage_1","expected_control_revision":1,"source":"canonical source"});
        if let Some(value) = &field {
            revision["args"] = value.clone();
        }
        let RevisionCommand::Stage { request, .. } = parse_revision(&revision.to_string()).unwrap()
        else {
            panic!("stage request");
        };
        assert_eq!(request.input, expected);
        let mut skip = json!({"action":"skip","run_id":"run","command_id":"skip_1","expected_control_revision":2,"target":{"type":"future_step","scope":"root/parallel:1","key":"review"}});
        if let Some(value) = field {
            skip["replacement"] = value;
        }
        let ControlCommand::Submit { request, .. } = parse_control(&skip.to_string()).unwrap()
        else {
            panic!("skip request");
        };
        assert_eq!(
            request.operation,
            WorkflowControlOperation::Skip {
                replacement: expected
            }
        );
    }
}

#[test]
fn apply_requires_a_selected_revision_and_defaults_to_draining_accepted_work() {
    let base = json!({"action":"apply","run_id":"run","command_id":"apply_1","expected_control_revision":3,"revision_id":"revision-2"});
    let RevisionCommand::Apply { run_id, request } = parse_revision(&base.to_string()).unwrap()
    else {
        panic!("apply request");
    };
    assert_eq!(
        (run_id, request),
        (
            "run".into(),
            RevisionApplyRequest {
                command_id: "apply_1".into(),
                expected_control_revision: 3,
                revision_id: "revision-2".into(),
                mode: RevisionApplyMode::DrainAccepted
            }
        )
    );
    let mut interrupt = base.clone();
    interrupt["mode"] = json!("interrupt_and_apply");
    let RevisionCommand::Apply { request, .. } = parse_revision(&interrupt.to_string()).unwrap()
    else {
        panic!("explicit interrupt request");
    };
    assert_eq!(request.mode, RevisionApplyMode::InterruptAndApply);
    let mut invalid = base;
    invalid.as_object_mut().unwrap().remove("revision_id");
    assert!(parse_revision(&invalid.to_string()).is_err());
}

#[test]
fn mutation_parser_rejects_fanout_and_unpinned_running_targets() {
    for target in [
        json!({"type":"run"}),
        json!({"type":"phase","title":"Review"}),
        json!({"type":"step","logical_id":"review"}),
        json!({"type":"step","logical_id":"review","expected_attempt_id":"attempt"}),
        json!({"type":"future_step","scope":"root","key":"review"}),
    ] {
        for action in ["guide", "interrupt"] {
            let mut command = json!({"action":action,"run_id":"run","command_id":"command_1","expected_control_revision":1,"target":target});
            if action == "guide" {
                command["message"] = json!("Focus on changed files");
            }
            assert!(
                parse_control(&command.to_string()).is_err(),
                "running controls must retain an exact native target"
            );
        }
    }
}
