use super::super::ReadSection;
use super::super::output::MAX_OUTPUT_BYTES;
use super::execution_content;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn raw_source_pages_reconstruct_control_rich_javascript_within_the_serialized_budget() {
    let source = format!(
        "workflow.meta({{name:'source-pages',description:'Read exact JavaScript'}});\n/*{}{}*/\nreturn workflow.args;",
        "\u{0001}".repeat(1700),
        "λ日本語".repeat(150)
    );
    codex_dynamic_workflows::compile_script(&source)
        .expect("control characters inside this JavaScript comment are valid");
    let mut reconstructed = String::new();
    let mut offset = 0;
    let mut pages = 0;
    loop {
        let response = execution_content(
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
            ReadSection::Source,
            &source,
            &json!({}),
            offset,
        )
        .unwrap();
        assert!(
            serde_json::to_vec(&response).unwrap().len() <= MAX_OUTPUT_BYTES,
            "the real response envelope must fit, including JSON escaping"
        );
        assert_eq!(response["page"]["offset"], offset);
        reconstructed.push_str(response["page"]["content"].as_str().unwrap());
        pages += 1;
        assert!(
            pages < 32,
            "source pagination must make bounded forward progress"
        );
        let Some(next) = response["page"]["next_offset"].as_u64() else {
            break;
        };
        let next = usize::try_from(next).unwrap();
        assert!(next > offset && source.is_char_boundary(next));
        offset = next;
    }
    assert!(pages > 2);
    assert_eq!(reconstructed, source);
}
