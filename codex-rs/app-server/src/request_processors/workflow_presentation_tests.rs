use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn typed_final_matching_never_reinterprets_a_string_as_structured_output() {
    for (text, value, expected) in [
        (r#"{"a":1}"#, json!({"a":1}), true),
        (r#"{"a":1}"#, json!(r#"{"a":1}"#), true),
        (r#""{\"a\":1}""#, json!(r#"{"a":1}"#), true),
        ("null", serde_json::Value::Null, true),
        (r#"{"a":1}"#, json!({"a":2}), false),
        ("plain Markdown", json!({"a":1}), false),
    ] {
        assert_eq!(matches_result(text, &value), expected, "{text}");
    }
}

#[test]
fn final_proof_matches_both_native_item_and_turn_without_replacing_text() {
    let proof = api::WorkflowAgentMessageProvenance {
        run_id: "run".into(),
        execution_id: "execution".into(),
        attempt_id: "attempt".into(),
        child_thread_id: "child".into(),
        turn_id: "owned".into(),
        item_id: "native-final".into(),
        format: api::WorkflowAgentMessageFormat::Json,
    };
    let original: api::ThreadItem = serde_json::from_value(
        json!({"type":"agentMessage","id":"native-final","text":"{\"a\":1}"}),
    )
    .unwrap();
    let mut later = original.clone();
    annotate(&proof, "later-human", &mut later);
    assert_eq!(later, original);
    let mut annotated = original.clone();
    annotate(&proof, "owned", &mut annotated);
    let api::ThreadItem::AgentMessage { workflow, .. } = &mut annotated else {
        panic!("native message")
    };
    assert_eq!(workflow, &Some(proof));
    *workflow = None;
    assert_eq!(annotated, original);
}
