use super::*;
use codex_protocol::AgentPath;
use pretty_assertions::assert_eq;
use serde_json::Value;

fn request(label: &str) -> AgentRequest {
    AgentRequest {
        run_id: "6f9f40c0-cdf0-4480-9d68-fd21e47968f2".into(),
        revision_id: "revision".into(),
        run_sequence: 7,
        owner_thread_id: "owner".into(),
        step_id: "opaque-callsite-id".into(),
        label: label.into(),
        validation_feedback: None,
        step_sequence: 1,
        attempt_id: "1f2bf9ee-5ae0-433c-a244-e982a52bc441".into(),
        attempt_number: 1,
        options: Value::Null,
    }
}

#[test]
fn readable_labels_preserve_unique_ordinals_when_sanitization_collides() {
    let mut first = request("AI Infrastructure");
    let mut second = request("AI/Infrastructure");
    second.step_sequence = 2;
    assert_eq!(
        task_name(&first),
        Ok("workflow_r7_ai_infrastructure_s1_a1".into())
    );
    assert_eq!(
        task_name(&second),
        Ok("workflow_r7_ai_infrastructure_s2_a1".into())
    );
    first.attempt_number = 2;
    assert_eq!(
        task_name(&first),
        Ok("workflow_r7_ai_infrastructure_s1_a2".into())
    );
    assert_eq!(
        task_name(&request("λ!?")),
        Ok("workflow_r7_step_s1_a1".into())
    );
}

#[test]
fn maximum_worker_name_is_native_valid_and_bounded_without_identity_truncation() {
    let mut request = request(&"a".repeat(128));
    request.run_sequence = u64::MAX;
    request.step_sequence = 1024;
    request.attempt_number = 16;
    let name = task_name(&request).unwrap();
    assert_eq!(name.len(), 64);
    assert!(AgentPath::root().join(&name).is_ok());
    assert!(name.contains(&u64::MAX.to_string()));
    request.attempt_number = 17;
    assert!(task_name(&request).is_err());
}

#[test]
fn legacy_records_derive_the_exact_original_name() {
    let mut request = request("readable step");
    request.run_sequence = 0;
    request.step_sequence = 0;
    assert_eq!(
        task_name(&request),
        Ok("workflow_6f9f40c0cdf044809d68fd21e47968f2_1f2bf9ee5ae0433ca244e982a52bc441".into())
    );
}
