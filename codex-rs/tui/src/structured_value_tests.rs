use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn text(lines: Vec<Line<'static>>) -> String {
    lines
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn compact_json_reuses_native_markdown_without_interpreting_nested_string_markup() {
    let value = json!({"report":{"checks":[{"detail":{"message":"**Ready** for release","warnings":["One warning","Two warnings"]},"name":"Build"}],"owner":{"name":"日本語","notes":"First paragraph\n\nSecond paragraph"}},"summary":"Audit complete"});
    let wide = text(render_value(&value, 90));
    assert!(
        wide.contains("**Ready** for release")
            && wide.contains("One warning")
            && wide.contains("Two warnings"),
        "{wide}"
    );
    assert!(
        wide.contains("First paragraph ⏎  ⏎ Second paragraph"),
        "{wide}"
    );
    assert!(
        !wide.contains("# Result") && !wide.contains("continues"),
        "{wide}"
    );
    insta::assert_snapshot!("structured_nested_wide", wide);
}

#[test]
fn compact_json_reuses_native_markdown_without_interpreting_nested_string_markup_narrow() {
    let value = json!({"report":{"checks":[{"detail":{"message":"**Ready** for release","warnings":["One warning","Two warnings"]},"name":"Build"}],"owner":{"name":"日本語","notes":"First paragraph\n\nSecond paragraph"}},"summary":"Audit complete"});
    insta::assert_snapshot!("structured_nested_narrow", text(render_value(&value, 28)));
}

#[test]
fn unicode_field_continuations_use_display_cells_and_values_are_never_ellipsized() {
    let lines = render_field("研究🦀", "alpha beta gamma delta epsilon", 20);
    let rendered = text(lines.clone());
    assert_eq!(lines[0].to_string(), "研究🦀: alpha beta");
    assert!(lines[1].to_string().starts_with("        "), "{rendered}");
    let value = json!({"研究🦀 very long field":{"子":"日本語🙂 end-marker"}});
    for width in [1, 2, 8, 20, 42] {
        let output = text(render_value(&value, width));
        assert!(!output.contains('…'), "width {width}: {output}");
        for ch in ['研', '究', '🦀', '子', '日', '本', '語', '🙂'] {
            assert!(output.contains(ch), "width {width} lost {ch}: {output}");
        }
    }
    insta::assert_snapshot!("structured_unicode_hanging", rendered);
}

#[test]
fn object_array_identity_and_matrices_render_at_wide_and_narrow_widths() {
    let value = json!({"results":[{"language":"Python","value":610},{"language":"Rust","value":610},{"language":"JavaScript","value":610}],"matrix":[[1,2],[3,4],[]]});
    let wide = text(render_value(&value, 80));
    for name in ["Python", "Rust", "JavaScript"] {
        assert_eq!(wide.matches(name).count(), 1);
    }
    assert_eq!(wide.matches("610").count(), 3);
    for (value, expected) in [
        (json!({}), "{}"),
        (json!([]), "[]"),
        (Value::Null, "null"),
        (json!(false), "false"),
        (json!(0), "0"),
    ] {
        assert!(text(render_value(&value, 20)).contains(expected));
    }
    insta::assert_snapshot!("structured_records_wide", wide);
}

#[test]
fn object_array_identity_and_matrices_render_at_wide_and_narrow_widths_narrow() {
    let value = json!({"results":[{"language":"Python","value":610},{"language":"Rust","value":610},{"language":"JavaScript","value":610}],"matrix":[[1,2],[3,4],[]]});
    insta::assert_snapshot!("structured_records_narrow", text(render_value(&value, 18)));
}

#[test]
fn top_level_strings_remain_ordinary_markdown_without_recursive_json_detection() {
    for source in [
        "**Ready**\n\n- Build passed",
        "{\"looks\":\"like JSON\"}",
        "",
        "```rust\nlet value = 1;\n```",
    ] {
        assert_eq!(
            render_value(&Value::String(source.into()), 42),
            markdown(source, 42)
        );
    }
    let source = "**Ready**\n\n- Build passed";
    assert!(!text(render_value(&json!(source), 42)).contains("**Ready**"));
    assert!(text(render_value(&json!({"note":source}), 80)).contains("**Ready**"));
}

#[test]
fn depth_fallback_keeps_complete_relative_paths_and_control_values() {
    let value = json!([{"a":{"b":{"c":{"~/":{"":"evidence\u{001b}[31m"}}}}}]);
    let rendered = text(render_value(&value, 80));
    assert!(
        rendered.contains("relative JSON Pointer paths"),
        "{rendered}"
    );
    assert!(
        rendered.contains("/~0~1/") && rendered.contains("evidence\\u001b[31m"),
        "{rendered}"
    );
    assert!(!rendered.contains('\u{001b}'));
    insta::assert_snapshot!("structured_relative_paths", rendered);
}
