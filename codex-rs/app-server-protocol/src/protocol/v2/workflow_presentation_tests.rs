use super::*;
use crate::protocol::v2::ThreadItem;
use crate::protocol::v2::WorkflowAttempt;
use crate::protocol::v2::WorkflowPhaseProgress;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn historical_workflow_rows_keep_unknown_times_and_counts() {
    let attempt: WorkflowAttempt = serde_json::from_value(json!({
        "id":"old-attempt","childThreadId":"child","status":"succeeded",
        "usage":null,"error":null,
    }))
    .unwrap();
    assert_eq!(
        (
            attempt.updated_at,
            attempt.completed_at,
            attempt.metrics.model_turns,
            attempt.metrics.active_duration_ms
        ),
        (None, None, None, None)
    );
    let phase: WorkflowPhaseProgress = serde_json::from_value(
        json!({"title":"Review","status":"completed","stepsDiscovered":1,"stepsCompleted":1}),
    )
    .unwrap();
    assert_eq!(
        (
            phase.updated_at,
            phase.completed_at,
            phase.metrics.model_turns,
            phase.metrics.agents_used
        ),
        (None, None, None, None)
    );
    let encoded = serde_json::to_value(phase).unwrap();
    assert!(encoded["updatedAt"].is_null() && encoded["metrics"]["modelTurns"].is_null());
}

#[test]
fn presentation_roundtrip_keeps_native_text_and_typed_string_policy() {
    for format in [
        WorkflowAgentMessageFormat::Text,
        WorkflowAgentMessageFormat::Json,
    ] {
        let original = json!({"type":"agentMessage","id":"item","text":"{\"nested\":1}"});
        let mut item: ThreadItem = serde_json::from_value(original.clone()).unwrap();
        assert!(matches!(
            &item,
            ThreadItem::AgentMessage { workflow: None, .. }
        ));
        if let ThreadItem::AgentMessage { workflow, .. } = &mut item {
            *workflow = Some(WorkflowAgentMessageProvenance {
                run_id: "run".into(),
                execution_id: "execution".into(),
                attempt_id: "attempt".into(),
                child_thread_id: "child".into(),
                turn_id: "turn".into(),
                item_id: "item".into(),
                format,
            });
        }
        let wire = serde_json::to_value(&item).unwrap();
        assert_eq!(wire["text"], original["text"]);
        assert!(wire["workflow"].get("result").is_none());
        assert_eq!(serde_json::from_value::<ThreadItem>(wire).unwrap(), item);
    }
}
