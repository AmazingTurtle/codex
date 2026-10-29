use super::*;
use pretty_assertions::assert_eq;

#[test]
fn lifecycle_roundtrip_keeps_null_result_distinct_from_absence() {
    for result in [
        WorkflowResult::Absent,
        WorkflowResult::Value { value: Value::Null },
    ] {
        let event = WorkflowLifecycleEvent {
            id: "event".into(),
            owner_thread_id: "owner".into(),
            run_id: "run".into(),
            execution_id: "execution".into(),
            workflow_name: "Review".into(),
            completion_mode: WorkflowCompletionMode::Display,
            sequence: 2,
            created_at: 20,
            kind: WorkflowLifecycleKind::StepCompleted {
                step_id: "step".into(),
                attempt_id: "attempt".into(),
                child_thread_id: "child".into(),
                owned_turn_ids: vec!["turn".into()],
                final_turn_id: Some("turn".into()),
                label: "Inspect".into(),
                phase: None,
                status: WorkflowLifecycleStatus::Succeeded,
                result,
                error: None,
                resolved_model: None,
                reasoning_effort: None,
                active_duration_ms: Some(1200),
            },
        };
        let wire = serde_json::to_value(&event).expect("encode");
        assert!(wire["kind"].get("ownedTurnIds").is_some());
        assert!(wire["kind"].get("owned_turn_ids").is_none());
        assert_eq!(
            serde_json::from_value::<WorkflowLifecycleEvent>(wire).expect("decode"),
            event
        );
    }
    assert_ne!(
        serde_json::to_value(WorkflowResult::Absent).unwrap(),
        serde_json::to_value(WorkflowResult::Value { value: Value::Null }).unwrap()
    );
}
