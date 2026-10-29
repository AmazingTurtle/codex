//! Rendering is separate so ordinary unchanged scripts retain their existing execution path.

use super::CompiledScript;
use serde_json::Value;

pub fn render_revision_script(
    compiled: &CompiledScript,
    args: &Value,
    budget: &Value,
    frontier: usize,
) -> Result<String, String> {
    let program = super::instrument_revision_script(compiled, frontier)?;
    let json_literal = |value: &Value| {
        serde_json::to_string(&value.to_string()).map_err(|error| error.to_string())
    };
    Ok(format!(
        "{}\n{}\nawait __workflowStart(JSON.parse({}), JSON.parse({}), JSON.parse({}), {}, {});",
        include_str!("../bootstrap_revision.js"),
        include_str!("../bootstrap.js"),
        json_literal(
            &serde_json::to_value(&compiled.metadata).map_err(|error| error.to_string())?
        )?,
        json_literal(args)?,
        json_literal(budget)?,
        serde_json::to_string(&program.body).map_err(|error| error.to_string())?,
        serde_json::json!({"statementCount":program.statement_count,"frontier":program.frontier,"marker":program.marker}),
    ))
}
