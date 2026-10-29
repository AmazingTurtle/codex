//! Verify the public exporters, rather than the presence of an experimental inventory marker.
use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn variant<'a>(schema: &'a Value, name: &str) -> Result<&'a Value> {
    schema["oneOf"]
        .as_array()
        .context("exported ThreadItem variants")?
        .iter()
        .find(|arm| arm["properties"]["type"]["enum"] == json!([name]))
        .with_context(|| format!("exported {name} arm"))
}

#[test]
fn generated_workflow_presentation_is_experimental_and_keeps_native_message_fields() -> Result<()> {
    let output = tempfile::tempdir()?;
    let mut schemas = Vec::new();
    let mut typescript = Vec::new();
    for experimental in [false, true] {
        let root = output.path().join(if experimental {
            "experimental"
        } else {
            "stable"
        });
        let json_root = root.join("json");
        let ts_root = root.join("typescript");
        generate_json_with_experimental(&json_root, experimental)?;
        generate_ts_with_options(
            &ts_root,
            /*prettier*/ None,
            GenerateTsOptions {
                generate_indices: false,
                ensure_headers: false,
                run_prettier: false,
                experimental_api: experimental,
            },
        )?;
        let bundle: Value = serde_json::from_slice(&fs::read(
            json_root.join("codex_app_server_protocol.v2.schemas.json"),
        )?)?;
        schemas.push(bundle["definitions"]["ThreadItem"].clone());
        typescript.push(fs::read_to_string(ts_root.join("v2/ThreadItem.ts"))?);
    }
    let stable = &schemas[0];
    let experimental = &schemas[1];
    let stable_message = variant(stable, "agentMessage")?;
    let experimental_message = variant(experimental, "agentMessage")?;
    assert!(stable_message["properties"].get("workflow").is_none());
    assert!(experimental_message["properties"].get("workflow").is_some());
    let mut expected = experimental.clone();
    let arms = expected["oneOf"]
        .as_array_mut()
        .context("experimental variants")?;
    for arm in arms {
        if arm["properties"]["type"]["enum"] == json!(["agentMessage"]) {
            arm["properties"]
                .as_object_mut()
                .context("agent message properties")?
                .remove("workflow");
            if let Some(required) = arm["required"].as_array_mut() {
                required.retain(|name| name.as_str() != Some("workflow"));
            }
        }
    }
    assert_eq!(
        stable, &expected,
        "only the experimental field is removed from native item schemas"
    );
    assert!(
        !typescript[0].contains("workflow:")
            && !typescript[0].contains("WorkflowAgentMessageProvenance")
    );
    assert!(
        typescript[1].contains("workflow:")
            && typescript[1].contains("WorkflowAgentMessageProvenance")
    );
    for field in [
        "id: string",
        "text: string",
        "phase:",
        "memoryCitation:",
        "delivery:",
        "questions:",
    ] {
        assert!(
            typescript.iter().all(|source| source.contains(field)),
            "ordinary field {field} must survive both exports"
        );
    }
    for arm in experimental["oneOf"]
        .as_array()
        .context("all native item variants")?
    {
        let name = arm["properties"]["type"]["enum"][0]
            .as_str()
            .context("variant name")?;
        assert!(
            typescript[0].contains(&format!("\"type\": \"{name}\"")),
            "stable TypeScript lost {name}"
        );
    }
    Ok(())
}

#[test]
fn enum_schema_pruning_stays_at_the_registered_object_level() -> Result<()> {
    for keyword in ["oneOf", "anyOf", "allOf"] {
        let arm = json!({
            "type":"object","properties":{"workflow":{"type":"string"},"text":{"type":"string"},
                "payload":{"type":"object","properties":{"workflow":{"type":"string"}}}},
            "required":["workflow","text"],"default":{"workflow":"user value"}
        });
        let mut schema = json!({(keyword):[arm.clone(),arm.clone()]});
        remove_property_from_schema(&mut schema, "workflow");
        let mut expected_arm = arm;
        expected_arm["properties"]
            .as_object_mut()
            .context("known object properties")?
            .remove("workflow");
        expected_arm["required"] = json!(["text"]);
        assert_eq!(
            schema,
            json!({(keyword):[expected_arm.clone(),expected_arm]})
        );
    }
    Ok(())
}

#[test]
fn typescript_pruning_visits_all_union_arms_but_keeps_nested_user_fields() {
    let source = "export type Item = { type: 'one', workflow: string, text: string, payload: { workflow: string } } | { type: 'two', workflow: string | null, value: number } & ({ [key in string]?: { workflow: string } });\n";
    let filtered = filter_experimental_type_fields_ts_contents(
        source.into(),
        &HashSet::from(["workflow".into()]),
    );
    assert_eq!(
        filtered,
        "export type Item = {type: 'one', text: string, payload: { workflow: string }} | {type: 'two', value: number} & ({ [key in string]?: { workflow: string } });\n"
    );
}
