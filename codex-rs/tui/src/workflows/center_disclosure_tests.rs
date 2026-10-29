//! The center uses the configured native activity toggle without consuming row/view shortcuts.
use super::*;
use pretty_assertions::assert_eq;

#[test]
fn configured_tool_details_toggle_native_cells_and_preserve_selected_worker() {
    let state = fixture();
    state.lock().unwrap().child = Some(0);
    let tool:codex_app_server_protocol::ThreadItem=serde_json::from_value(serde_json::json!({
        "type":"commandExecution","id":"real-command","command":"python audit.py","cwd":"/tmp/project","source":"agent","status":"completed",
        "commandActions":[{"type":"unknown","command":"python audit.py"}],"aggregatedOutput":(1..=12).map(|index|format!("tool output line {index}")).collect::<Vec<_>>().join("\n"),"exitCode":0,"durationMs":24
    })).unwrap();
    let cells = crate::thread_transcript::thread_items_to_transcript_cells(
        None,
        &crate::test_support::test_path_buf("/tmp/project")
            .try_into()
            .unwrap(),
        [tool],
        crate::thread_transcript::RawReasoningVisibility::Hidden,
        None,
    );
    let target = {
        let mut state = state.lock().unwrap();
        state.center.detail_tab = DetailTab::Transcript;
        state.selected_transcript_target().unwrap()
    };
    state.lock().unwrap().transcripts.insert(
        target.cache_key(),
        WorkflowTranscript {
            cells,
            ..Default::default()
        },
    );
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let config = serde_json::from_value(serde_json::json!({
        "global": {"focus_activity": "f8"}
    }))
    .unwrap();
    let keymap = crate::keymap::RuntimeKeymap::from_config(&config).unwrap();
    let mut center =
        WorkflowCenter::new(state.clone(), AppEventSender::new(tx)).with_runtime_keymap(keymap);
    let selected = state.lock().unwrap().selected_node_key();
    let compact = render(&center, 120, 30);
    assert!(
        compact.contains("Ran python audit.py")
            && compact.contains("tool output line 12")
            && compact.contains("+ 9 lines")
            && !compact.contains("tool output line 2"),
        "{compact}"
    );
    assert!(compact.contains("f8 tools"), "{compact}");
    center.handle_key_event(KeyCode::F(8).into());
    let expanded = render(&center, 120, 30);
    assert!(
        expanded.contains("tool output line 2") && expanded.contains("tool output line 12"),
        "{expanded}"
    );
    assert_eq!(state.lock().unwrap().selected_node_key(), selected);
    assert!(
        events.try_recv().is_err(),
        "tool disclosure reads the retained native cells"
    );
    insta::assert_snapshot!("workflow_configured_native_tool_details", expanded);
    center.handle_key_event(KeyCode::F(8).into());
    assert_eq!(render(&center, 120, 30), compact);
    center.handle_key_event(KeyCode::Right.into());
    assert_eq!(state.lock().unwrap().center.detail_tab, DetailTab::Source);
    center.handle_key_event(KeyCode::Enter.into());
    assert!(
        matches!(events.try_recv(),Ok(AppEvent::Workflow(WorkflowEvent::SelectAgent(id))) if id==ThreadId::from_u128(42))
    );
}
