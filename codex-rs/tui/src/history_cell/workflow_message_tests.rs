use super::*;
use pretty_assertions::assert_eq;

fn proof(
    turn: &str,
    item: &str,
    format: WorkflowAgentMessageFormat,
) -> WorkflowAgentMessageProvenance {
    WorkflowAgentMessageProvenance {
        run_id: "run".into(),
        execution_id: "execution".into(),
        attempt_id: "attempt".into(),
        child_thread_id: "worker".into(),
        turn_id: turn.into(),
        item_id: item.into(),
        format,
    }
}

#[test]
fn late_proof_formats_only_the_exact_owned_final_and_preserves_native_source() {
    let original = r#"{"checks":[{"name":"Build","status":"passed"}],"ok":true}"#;
    let cell = WorkflowMessageCell::new(
        NativeMessageIdentity {
            thread_id: "worker".into(),
            turn_id: "owned".into(),
            item_id: "final".into(),
        },
        AgentMarkdownCell::new(original.into(), std::path::Path::new("/workspace"))
            .with_copy_source(Some(original.into())),
    );
    let ordinary = cell.display_lines(70);
    assert!(!cell.apply(&proof(
        "later-human",
        "final",
        WorkflowAgentMessageFormat::Json
    )));
    assert!(!cell.apply(&proof("owned", "interim", WorkflowAgentMessageFormat::Json)));
    assert!(!cell.apply(&proof("owned", "final", WorkflowAgentMessageFormat::Text)));
    assert_eq!(cell.display_lines(70), ordinary);
    assert!(cell.apply(&proof("owned", "final", WorkflowAgentMessageFormat::Json)));
    assert!(
        !cell.apply(&proof("owned", "final", WorkflowAgentMessageFormat::Json)),
        "duplicate metadata must not churn the retained cell"
    );
    assert_eq!(
        (cell.copy_source(), cell.raw_lines()),
        (Some(original), raw_lines_from_source(original))
    );
    let visible = cell
        .display_lines(70)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        visible.contains("Build") && visible.contains("passed") && !visible.contains("{\"checks\"")
    );
    assert!(visible.starts_with("• Response\n"), "{visible}");
    assert!(!visible.contains("• •"), "{visible}");
    insta::assert_snapshot!("native_workflow_final_json", visible);
}

#[test]
fn proven_null_is_present_and_typed_string_is_not_recursively_parsed() {
    for (text, format, formatted) in [
        ("null", WorkflowAgentMessageFormat::Json, true),
        (
            "{\"message\":\"ordinary string\"}",
            WorkflowAgentMessageFormat::Text,
            false,
        ),
        (
            "## Plain **Markdown**",
            WorkflowAgentMessageFormat::Text,
            false,
        ),
    ] {
        let native = AgentMarkdownCell::new(text.into(), std::path::Path::new("/workspace"));
        let before = native.display_lines(60);
        let cell = WorkflowMessageCell::new(
            NativeMessageIdentity {
                thread_id: "worker".into(),
                turn_id: "owned".into(),
                item_id: "final".into(),
            },
            native,
        );
        assert_eq!(cell.apply(&proof("owned", "final", format)), formatted);
        if !formatted {
            assert_eq!(cell.display_lines(60), before);
        }
        assert_eq!(cell.copy_source(), Some(text));
    }
}
