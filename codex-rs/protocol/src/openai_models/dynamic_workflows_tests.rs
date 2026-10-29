use crate::openai_models::ModelMessages;
use pretty_assertions::assert_eq;

#[test]
fn workflow_overrides_preserve_sparse_catalog_compatibility() {
    for value in [
        serde_json::json!({}),
        serde_json::json!({"tools": null}),
        serde_json::json!({"tools": {"dynamic_workflows": null}}),
        serde_json::json!({"tools": {"dynamic_workflows": {}}}),
        serde_json::json!({"tools": {"dynamic_workflows": {
            "guidance": "", "launch": {"description": "Launch wording"},
            "control": {"description": "Control wording"}
        }}}),
    ] {
        let messages: ModelMessages =
            serde_json::from_value(value.clone()).expect("compatible catalog");
        let serialized = serde_json::to_value(messages).expect("serialize catalog");
        let expected = value
            .pointer("/tools/dynamic_workflows")
            .filter(|value| !value.is_null());
        assert_eq!(serialized.pointer("/tools/dynamic_workflows"), expected);
    }
}
