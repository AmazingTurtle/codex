//! Real native cells, their disclosure and immutable prefixes survive live and paged projections.
use super::*;
use pretty_assertions::assert_eq;

fn entry(
    turn: &str,
    item: serde_json::Value,
    completed: Option<i64>,
) -> codex_app_server_protocol::ThreadItemEntry {
    codex_app_server_protocol::ThreadItemEntry {
        turn_id: turn.into(),
        item: serde_json::from_value(item).unwrap(),
        started_at_ms: Some(1),
        completed_at_ms: completed,
    }
}

#[tokio::test]
async fn ordered_tools_and_live_native_cells_share_compact_rendering_and_keep_prefix_arcs()
-> Result<()> {
    let (app, _events, _ops) = make_test_app_with_channels().await;
    let origin: codex_app_server_protocol::WorkflowTranscriptGroup = serde_json::from_value(
        json!({
            "runId":"run","executionId":"execution","stepId":"step","attemptId":"attempt","childThreadId":ThreadId::new(),"label":"Reviewer","phase":"Review","ownedTurnIds":["owned"],"resolvedModel":"actual-model","reasoningEffort":"high","cwd":app.config.cwd,
            "items":[],"finalTurnId":null,"finalItemId":null,"finalResult":null
        }),
    )?;
    let mut group = crate::workflows::TranscriptGroup {
        origin,
        header: Arc::new(crate::history_cell::PlainHistoryCell::new(Vec::new())),
        items: Vec::new(),
        projection: Default::default(),
        projected: Vec::new(),
        outcome: None,
    };
    let command = |id: &str, command: &str, status: &str, output: &str| json!({"type":"commandExecution","id":id,"command":command,"cwd":app.config.cwd,"status":status,"source":"agent","commandActions":[],"aggregatedOutput":output,"exitCode":if status=="completed" {Some(0)}else{None},"durationMs":1});
    group.items.push(crate::workflows::TranscriptItem {entry:entry("owned",json!({"type":"userMessage","id":"task","content":[{"type":"text","text":"Actual complete task","textElements":[]}]}),None)});
    group.items.push(crate::workflows::TranscriptItem {
        entry: entry(
            "owned",
            command("tool", "printf ACTUAL_TOOL", "inProgress", "ACTUAL_TOOL\n"),
            None,
        ),
    });
    group.project(&app.config, app.local_settings.tui.tool_call_display);
    let prompt = Arc::clone(&group.projected[0]);
    assert!(
        group.projected[1]
            .as_any()
            .is::<crate::thread_transcript::LiveCommandCell>(),
        "an active native tool must retain the incremental native ExecCell"
    );
    let live = group.projected[1]
        .display_lines(80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(live.contains("ACTUAL_TOOL"));
    group.items[1].entry = entry(
        "owned",
        command(
            "tool",
            "printf ACTUAL_TOOL",
            "completed",
            &(1..15)
                .map(|index| format!("line {index}\n"))
                .collect::<String>(),
        ),
        Some(2),
    );
    group.project(&app.config, app.local_settings.tui.tool_call_display);
    assert!(
        Arc::ptr_eq(&prompt, &group.projected[0]),
        "settling a tool must preserve the preceding task's anchor"
    );
    let compact = group.projected[1]
        .display_lines(80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(compact.contains("Ran printf ACTUAL_TOOL") && compact.contains("line 1"));
    assert!(
        !compact.contains("line 14"),
        "normal compact output must retain native disclosure instead of expanding all output"
    );
    let retained = Arc::clone(&group.projected[1]);
    group.project(&app.config, app.local_settings.tui.tool_call_display);
    assert!(
        Arc::ptr_eq(&retained, &group.projected[1]),
        "unchanged refresh must preserve native group identity"
    );
    group.origin.final_turn_id = Some("owned".into());
    group.origin.final_item_id = Some("accepted".into());
    group.origin.final_result = Some(codex_app_server_protocol::WorkflowTranscriptResult::Value {
        value: json!({"ok":true}),
    });
    let mut stale = group.origin.clone();
    stale.final_turn_id = None;
    stale.final_item_id = None;
    stale.final_result = None;
    stale.owned_turn_ids.clear();
    group.reconcile_origin(&mut stale);
    assert_eq!(
        stale, group.origin,
        "a delayed snapshot cannot remove the accepted outcome or proven turn range"
    );
    let mut context_only = group.origin.clone();
    context_only.final_item_id = None;
    context_only.final_result = None;
    let pending = crate::workflows::TranscriptGroup {
        origin: context_only,
        header: Arc::clone(&group.header),
        items: Vec::new(),
        projection: Default::default(),
        projected: Vec::new(),
        outcome: None,
    };
    let mut accepted = group.origin.clone();
    pending.reconcile_origin(&mut accepted);
    assert_eq!(
        accepted, group.origin,
        "earlier native final-turn context must still accept the later validated result"
    );
    insta::assert_snapshot!("workflow_native_compact_tool", compact);
    let read = |id: &str, path: &str| {
        entry(
            "owned",
            json!({"type":"commandExecution","id":id,"command":format!("cat {path}"),"cwd":app.config.cwd,"status":"completed","source":"agent","commandActions":[{"type":"read","command":format!("cat {path}"),"name":path,"path":path}],"aggregatedOutput":"file content","exitCode":0,"durationMs":1}),
            Some(2),
        )
    };
    group.items = vec![
        crate::workflows::TranscriptItem {
            entry: read("read-one", "one.rs"),
        },
        crate::workflows::TranscriptItem {
            entry: entry(
                "owned",
                json!({"type":"reasoning","id":"hidden","summary":["Checking the other source"],"content":[]}),
                Some(2),
            ),
        },
        crate::workflows::TranscriptItem {
            entry: read("read-two", "two.rs"),
        },
    ];
    group.project(&app.config, codex_config::types::ToolCallDisplay::Summary);
    assert_eq!(
        group.projected.len(),
        1,
        "ordered native exploration grouping spans hidden reasoning"
    );
    let native = group.projected[0]
        .as_any()
        .downcast_ref::<crate::exec_cell::ExecCell>()
        .unwrap();
    assert_eq!(native.iter_calls().count(), 2);
    let mut second = read("read-three", "three.rs");
    second.turn_id = "later-owned".into();
    group.origin.owned_turn_ids.push("later-owned".into());
    group
        .items
        .push(crate::workflows::TranscriptItem { entry: second });
    group.project(&app.config, codex_config::types::ToolCallDisplay::Summary);
    assert_eq!(
        group.projected.len(),
        2,
        "native activity never merges across turn ownership boundaries"
    );
    Ok(())
}
