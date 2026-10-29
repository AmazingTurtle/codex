use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn entry(item: serde_json::Value, completed_at_ms: Option<i64>) -> api::ThreadItemEntry {
    api::ThreadItemEntry {
        turn_id: "owned".into(),
        item: serde_json::from_value(item).unwrap(),
        started_at_ms: Some(10),
        completed_at_ms,
    }
}
fn command(
    status: &str,
    output: Option<&str>,
    completed_at_ms: Option<i64>,
) -> api::ThreadItemEntry {
    entry(
        json!({"type":"commandExecution","id":"real-tool","command":"printf actual","cwd":std::env::temp_dir(),"source":"agent","status":status,"commandActions":[],"aggregatedOutput":output,"exitCode":null,"durationMs":null}),
        completed_at_ms,
    )
}

#[test]
fn same_id_live_output_enriches_only_unsettled_durable_commands_and_retains_time() {
    let start = command("inProgress", None, /*completed_at_ms*/ None);
    let output = command(
        "inProgress",
        Some("actual native output"),
        /*completed_at_ms*/ None,
    );
    assert_eq!(
        merge(&[start], std::slice::from_ref(&output)),
        vec![output.clone()]
    );
    let done = command("completed", Some("settled canonical output"), Some(20));
    assert_eq!(merge(std::slice::from_ref(&done), &[output]), vec![done]);
}

#[test]
fn oversized_live_prefix_preserves_the_real_durable_page_and_cursor_with_a_limitation() {
    let task = entry(
        json!({"type":"userMessage","id":"actual-task","content":[{"type":"text","text":"The real durable native task","textElements":[]}]}),
        /*completed_at_ms*/ None,
    );
    let group:api::WorkflowTranscriptGroup=serde_json::from_value(json!({"runId":"run","executionId":"execution","stepId":"step","attemptId":"attempt","childThreadId":"child","label":"Worker","phase":null,"ownedTurnIds":["owned"],"resolvedModel":null,"reasoningEffort":null,"cwd":std::env::temp_dir(),"items":[task],"finalTurnId":null,"finalItemId":null,"finalResult":null})).unwrap();
    let mut response = api::WorkflowTranscriptListResponse {
        data: vec![group],
        next_cursor: Some("the unchanged durable cursor".into()),
        limitation: None,
    };
    let original = response.clone();
    let prefix = entry(
        json!({"type":"agentMessage","id":"actual-live-message","text":"x".repeat(9*1024*1024)}),
        /*completed_at_ms*/ None,
    );
    apply(
        &mut response,
        vec![Overlay {
            attempt_id: "attempt".into(),
            child_id: "child".into(),
            items: vec![prefix],
        }],
    )
    .unwrap();
    assert_eq!(
        (&response.data, &response.next_cursor),
        (&original.data, &original.next_cursor)
    );
    assert!(
        response
            .limitation
            .as_ref()
            .unwrap()
            .contains("bounded live preview")
    );
    assert!(serde_json::to_vec(&response).unwrap().len() < MAX_RESPONSE_BYTES);
}

#[test]
fn an_accepted_overlay_reserves_the_notice_before_rejecting_a_near_limit_overlay() {
    let task = entry(
        json!({"type":"userMessage","id":"actual-task","content":[{"type":"text","text":"Durable task","textElements":[]}]}),
        /*completed_at_ms*/ None,
    );
    let first:api::WorkflowTranscriptGroup=serde_json::from_value(json!({"runId":"run","executionId":"execution","stepId":"step","attemptId":"first","childThreadId":"child-one","label":"Worker","phase":null,"ownedTurnIds":["owned"],"resolvedModel":null,"reasoningEffort":null,"cwd":std::env::temp_dir(),"items":[task],"finalTurnId":null,"finalItemId":null,"finalResult":null})).unwrap();
    let mut second = first.clone();
    second.attempt_id = "second".into();
    second.child_thread_id = "child-two".into();
    let mut response = api::WorkflowTranscriptListResponse {
        data: vec![first, second],
        next_cursor: Some("durable cursor".into()),
        limitation: None,
    };
    let durable = response.clone();
    let small = entry(
        json!({"type":"agentMessage","id":"first-live","text":"A real live prefix"}),
        /*completed_at_ms*/ None,
    );
    let mut large = entry(
        json!({"type":"agentMessage","id":"second-live","text":""}),
        /*completed_at_ms*/ None,
    );
    let mut without_notice = response.clone();
    without_notice.data[0].items.push(small.clone());
    without_notice.data[1].items.push(large.clone());
    let overhead = serde_json::to_vec(&without_notice).unwrap().len();
    let api::ThreadItem::AgentMessage { text, .. } = &mut large.item else {
        unreachable!();
    };
    *text = "x".repeat(MAX_RESPONSE_BYTES - overhead - 32);
    without_notice.data[1].items.pop();
    without_notice.data[1].items.push(large.clone());
    assert_eq!(
        serde_json::to_vec(&without_notice).unwrap().len(),
        MAX_RESPONSE_BYTES - 32
    );
    apply(
        &mut response,
        vec![
            Overlay {
                attempt_id: "first".into(),
                child_id: "child-one".into(),
                items: vec![small.clone()],
            },
            Overlay {
                attempt_id: "second".into(),
                child_id: "child-two".into(),
                items: vec![large],
            },
        ],
    )
    .unwrap();
    let mut expected_first = durable.data[0].items.clone();
    expected_first.push(small);
    assert_eq!(
        (
            &response.data[0].items,
            &response.data[1].items,
            &response.next_cursor
        ),
        (
            &expected_first,
            &durable.data[1].items,
            &durable.next_cursor
        )
    );
    assert!(
        response
            .limitation
            .as_ref()
            .unwrap()
            .contains("bounded live preview")
    );
    assert!(serde_json::to_vec(&response).unwrap().len() <= MAX_RESPONSE_BYTES);
}
