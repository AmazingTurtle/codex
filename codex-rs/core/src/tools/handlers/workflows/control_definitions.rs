//! Compact trusted tool schemas; the final wire definition has the same hard cap as existing tools.
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use serde_json::Value;
use serde_json::json;

pub(super) fn specs() -> Result<[ToolSpec; 2], String> {
    let target = json!({"type":"object","properties":{
        "type":{"type":"string","enum":["run","phase","step","future_step"]},
        "title":{"type":"string"},"logical_id":{"type":"string"},
        "expected_attempt_id":{"type":"string"},"expected_turn_id":{"type":"string"},
        "scope":{"type":"string"},"key":{"type":"string"}
    },"required":["type"],"additionalProperties":false});
    Ok([
        spec(
            "workflow_control",
            "Read state/history. Mutate only a run started by this agent, under its current permissions. Mutations require command_id and observed expected_control_revision. Guide/interrupt require step expected_attempt_id and expected_turn_id. Edit/skip require step or future_step(scope,key). Run/phase mutations are rejected; labels never identify work.",
            json!({
                "type":"object","properties":{
                    "action":{"type":"string","enum":["read","guide","edit_queued","skip","interrupt"]},"run_id":{"type":"string"},
                    "offset":{"type":"integer","minimum":0},"command_id":{"type":"string"},"expected_control_revision":{"type":"integer","minimum":0},
                    "target":target,"message":{"type":"string"},"prompt":{"type":"string"},
                    "replacement":{"description":"Skip result; omitted defaults to null. Must satisfy the step schema."}
                },"required":["action","run_id"],"additionalProperties":false
            }),
        )?,
        spec(
            "workflow_revision",
            "List revisions, stage canonical source, apply it, or resume a ready revision. Mutate only a run started by this agent, under its current permissions. Every mutation requires command_id and expected_control_revision. Apply defaults to drain_accepted; interrupt_and_apply cancels owned work. Revisions share the run's original limits.",
            json!({
                "type":"object","properties":{
                    "action":{"type":"string","enum":["list","stage","apply","resume"]},"run_id":{"type":"string"},"offset":{"type":"integer","minimum":0},
                    "command_id":{"type":"string"},"expected_control_revision":{"type":"integer","minimum":0},"source":{"type":"string"},
                    "args":{"description":"Stage only: omitted reuses current args; explicit null stays null."},"revision_id":{"type":"string"},
                    "mode":{"type":"string","enum":["drain_accepted","interrupt_and_apply"]}
                },"required":["action","run_id"],"additionalProperties":false
            }),
        )?,
    ])
}

fn spec(name: &str, description: &str, parameters: Value) -> Result<ToolSpec, String> {
    let spec = ToolSpec::Function(ResponsesApiTool {
        name: name.into(),
        description: description.into(),
        strict: false,
        defer_loading: None,
        parameters: serde_json::from_value(parameters).map_err(|error| error.to_string())?,
        output_schema: None,
    });
    if serde_json::to_vec(&spec)
        .map_err(|error| error.to_string())?
        .len()
        > super::MAX_TOOL_DEFINITION_BYTES
    {
        return Err("workflow control definition exceeds its model context budget".into());
    }
    Ok(spec)
}
