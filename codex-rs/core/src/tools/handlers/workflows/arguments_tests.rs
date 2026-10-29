use super::Arguments;
use super::Handler;
use super::MAX_TOOL_DEFINITION_BYTES;
use super::Operation;
use codex_protocol::openai_models::ToolMessage;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn launch_arguments_preserve_explicit_null_and_legacy_input() {
    for (request, expected) in [
        (json!({"source": "script"}), json!({})),
        (json!({"source": "script", "args": null}), json!(null)),
        (
            json!({"source": "script", "args": {"task": "review"}}),
            json!({"task": "review"}),
        ),
        (
            json!({"source": "script", "args": "raw words"}),
            json!("raw words"),
        ),
        (json!({"source": "script", "input": null}), json!(null)),
        (json!({"source": "script", "input": [1, 2]}), json!([1, 2])),
    ] {
        let arguments: Arguments = serde_json::from_value(request).expect("valid launch arguments");
        assert_eq!(arguments.args.unwrap_or_else(|| json!({})), expected);
    }
    assert!(serde_json::from_value::<Arguments>(json!({"args": null, "input": {}})).is_err());
}

#[test]
fn catalog_expansion_falls_back_to_the_complete_bundled_definition() {
    let parameters = format!(
        "{{\"type\":\"object\",\"properties\":{{\"n\":{{\"type\":\"number\",\"enum\":[{}]}}}}}}",
        vec!["1e9"; 490].join(","),
    );
    assert!(parameters.len() <= 2048);
    let catalog = ToolMessage {
        description: Some("\0".repeat(1024)),
        parameters: Some(parameters),
    };
    let codex_tools::ToolSpec::Function(bundled) =
        Handler::new(Operation::Start, /*catalog*/ None).spec
    else {
        panic!("workflow launch is a function tool");
    };
    let expanded = codex_tools::ToolSpec::Function(codex_tools::with_catalog_tool_message(
        bundled,
        Some(&catalog),
    ));
    assert!(
        serde_json::to_vec(&expanded).unwrap().len() > MAX_TOOL_DEFINITION_BYTES,
        "the valid raw catalog must exceed the rendered budget to exercise fallback"
    );
    for operation in Operation::ALL {
        let bundled = Handler::new(operation, /*catalog*/ None).spec;
        let customized = Handler::new(operation, Some(&catalog)).spec;
        assert_eq!(customized, bundled);
        assert!(serde_json::to_vec(&customized).unwrap().len() <= MAX_TOOL_DEFINITION_BYTES);
    }
}

#[test]
fn a_bounded_catalog_override_retains_its_schema_and_required_arguments() {
    let catalog = ToolMessage {
        description: Some("Read the requested saved source.".into()),
        parameters: Some(json!({"type":"object","properties":{"saved_workflow_id":{"type":"string"}},"required":["saved_workflow_id"],"additionalProperties":false}).to_string()),
    };
    let spec = Handler::new(Operation::SavedRead, Some(&catalog)).spec;
    let rendered = serde_json::to_value(&spec).unwrap();
    assert_eq!(
        rendered["parameters"],
        serde_json::from_str::<serde_json::Value>(catalog.parameters.as_ref().unwrap()).unwrap()
    );
    assert_eq!(
        (rendered["name"].as_str(), rendered["description"].as_str()),
        (Some("saved_workflow_read"), catalog.description.as_deref())
    );
    assert!(serde_json::to_vec(&spec).unwrap().len() <= MAX_TOOL_DEFINITION_BYTES);
}
