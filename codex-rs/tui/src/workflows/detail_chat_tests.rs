use super::*;
use crate::test_support::PathBufExt;
use crate::test_support::test_path_buf;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;

fn screen(buffer: &Buffer) -> String {
    buffer
        .content()
        .chunks(usize::from(buffer.area.width))
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_owned()
}

#[test]
fn chat_uses_native_compact_tool_disclosure_without_stealing_table_enter() {
    let cwd = test_path_buf("/workspace").abs();
    let item:codex_app_server_protocol::ThreadItem=serde_json::from_value(serde_json::json!({"type":"commandExecution","id":"real-tool","command":"cargo check","cwd":cwd,"status":"completed","source":"agent","commandActions":[],"aggregatedOutput":(1..15).map(|index|format!("line {index}\n")).collect::<String>(),"exitCode":0,"durationMs":12})).unwrap();
    let cells = crate::thread_transcript::thread_items_to_transcript_cells(
        /*thread_id*/ None,
        &cwd,
        [item],
        crate::thread_transcript::RawReasoningVisibility::Hidden,
        /*config*/ None,
    );
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 70, /*height*/ 24,
    );
    let mut viewport = DetailViewport::default();
    let mut buffer = Buffer::empty(area);
    viewport.render_cells_revision(&cells, "scope", /*revision*/ 1, area, &mut buffer);
    let compact = screen(&buffer);
    assert!(
        compact.contains("Ran cargo check")
            && compact.contains("line 14")
            && compact.contains("+ 11 lines")
            && !compact.contains("line 2"),
        "{compact}"
    );
    let config = serde_json::from_value(serde_json::json!({
        "global": {"focus_activity": "f6"}
    }))
    .unwrap();
    let keymap = crate::keymap::RuntimeKeymap::from_config(&config).unwrap();
    assert!(
        !viewport.handle_disclosure_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &keymap)
    );
    assert!(
        !viewport.handle_disclosure_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE), &keymap)
    );
    assert!(
        viewport.handle_disclosure_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE), &keymap)
    );
    buffer.reset();
    viewport.render_cells_revision(&cells, "scope", /*revision*/ 1, area, &mut buffer);
    let expanded = screen(&buffer);
    assert!(
        expanded.contains("line 2") && expanded.contains("line 14"),
        "{expanded}"
    );
    assert!(
        viewport.handle_disclosure_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE), &keymap)
    );
    buffer.reset();
    viewport.render_cells_revision(&cells, "scope", /*revision*/ 1, area, &mut buffer);
    assert_eq!(
        screen(&buffer),
        compact,
        "the native tool returns to the same compact presentation"
    );
    insta::assert_snapshot!(
        "workflow_native_tool_disclosure",
        format!("Compact\n{compact}\n\nExpanded\n{expanded}")
    );
}
