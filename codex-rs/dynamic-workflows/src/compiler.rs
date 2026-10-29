//! Static workflow metadata extraction. Parsing never executes the submitted program.

mod literal;
mod revision;
mod revision_program;
mod revision_render;

pub use revision::RevisionAnalysis;
pub use revision::RevisionProgram;
pub use revision::analyze_revision;
pub use revision::instrument_revision_script;
pub use revision_render::render_revision_script;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use tree_sitter::Node;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptMetadata {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub phases: Vec<ScriptPhase>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptPhase {
    pub title: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledScript {
    pub metadata: ScriptMetadata,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParsedScript {
    V2(CompiledScript),
    LegacyReadOnly,
}

pub fn compile_script(source: &str) -> Result<CompiledScript, String> {
    match parse_script(source)? {
        ParsedScript::V2(script) => Ok(script),
        ParsedScript::LegacyReadOnly => Err("workflow source must begin with workflow.meta({name, description, phases}); legacy scripts are read-only".into()),
    }
}

pub fn parse_script(source: &str) -> Result<ParsedScript, String> {
    crate::types::validate_source(source)?;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .map_err(|error| error.to_string())?;
    let tree = parser
        .parse(source, /*old_tree*/ None)
        .ok_or("failed to parse workflow source")?;
    let root = tree.root_node();
    if root.has_error() {
        return Err("invalid workflow JavaScript syntax".into());
    }
    let mut cursor = root.walk();
    let first = root
        .named_children(&mut cursor)
        .find(|node| node.kind() != "comment")
        .ok_or("workflow source is empty")?;
    let mut calls = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if matches!(
            node.kind(),
            "import_statement" | "export_statement" | "import"
        ) {
            return Err("workflow scripts cannot import modules or use ESM exports".into());
        }
        if is_meta_call(node, source) {
            calls.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    if calls.is_empty() {
        return Ok(ParsedScript::LegacyReadOnly);
    }
    if calls.len() != 1 || first.kind() != "expression_statement" {
        return Err("workflow.meta must be the first statement and occur exactly once".into());
    }
    let call = first
        .named_child(0)
        .ok_or("workflow.meta must be the first statement")?;
    if !is_meta_call(call, source) {
        return Err("workflow.meta must be the first statement".into());
    }
    let function = call
        .child_by_field_name("function")
        .ok_or("invalid workflow.meta call")?;
    if function.kind() != "member_expression" {
        return Err("workflow.meta must use direct, non-computed property access".into());
    }
    if call.child_by_field_name("optional_chain").is_some()
        || function.child_by_field_name("optional_chain").is_some()
    {
        return Err("workflow.meta cannot use optional chaining".into());
    }
    let arguments = call
        .child_by_field_name("arguments")
        .ok_or("workflow.meta requires one object literal")?;
    let mut cursor = arguments.walk();
    let arguments: Vec<_> = arguments
        .named_children(&mut cursor)
        .filter(|node| node.kind() != "comment")
        .collect();
    if arguments.len() != 1 || arguments[0].kind() != "object" {
        return Err("workflow.meta requires one pure object literal".into());
    }
    let metadata: ScriptMetadata =
        serde_json::from_value(literal::value(arguments[0], source, /*depth*/ 0)?)
            .map_err(|error| format!("invalid workflow metadata: {error}"))?;
    if metadata.name.is_empty()
        || metadata.name.len() > 80
        || !metadata
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("workflow metadata name must contain 1..=80 ASCII letters, digits, underscores or hyphens".into());
    }
    if metadata.description.trim().is_empty() || metadata.description.len() > 1024 {
        return Err("workflow metadata description must contain 1..=1024 UTF-8 bytes".into());
    }
    let mut titles = HashSet::new();
    if metadata.phases.len() > 64 {
        return Err("workflow metadata exceeds 64 phases".into());
    }
    for phase in &metadata.phases {
        if phase.title.trim().is_empty() || phase.title.len() > 128 || !titles.insert(&phase.title)
        {
            return Err(
                "workflow phase titles must be unique and contain 1..=128 UTF-8 bytes".into(),
            );
        }
    }
    // Preserve source positions for errors and call-site identities after removing metadata.
    let mut body = source.to_owned();
    body.replace_range(
        first.byte_range(),
        &source[first.byte_range()]
            .bytes()
            .map(|character| {
                if character == b'\n' || character == b'\r' {
                    char::from(character)
                } else {
                    ' '
                }
            })
            .collect::<String>(),
    );
    Ok(ParsedScript::V2(CompiledScript { metadata, body }))
}

fn is_meta_call(node: Node<'_>, source: &str) -> bool {
    if node.kind() != "call_expression" {
        return false;
    }
    let Some(function) = node.child_by_field_name("function") else {
        return false;
    };
    if !matches!(
        function.kind(),
        "member_expression" | "subscript_expression"
    ) {
        return false;
    }
    let object = function.child_by_field_name("object");
    let property = function
        .child_by_field_name("property")
        .or_else(|| function.child_by_field_name("index"));
    object.is_some_and(|node| {
        node.kind() == "identifier" && node.utf8_text(source.as_bytes()) == Ok("workflow")
    }) && property.is_some_and(|node| {
        node.utf8_text(source.as_bytes()) == Ok("meta")
            || literal::value(node, source, /*depth*/ 0)
                .is_ok_and(|value| value == Value::String("meta".into()))
    })
}

/// Render one isolated workflow program. Only the public workflow object enters user scope.
pub fn render_script(
    compiled: &CompiledScript,
    args: &Value,
    budget: &Value,
) -> Result<String, String> {
    let json_literal = |value: &Value| {
        serde_json::to_string(&value.to_string()).map_err(|error| error.to_string())
    };
    Ok(format!(
        "{}\nawait __workflowStart(JSON.parse({}), JSON.parse({}), JSON.parse({}), {});",
        include_str!("bootstrap.js"),
        json_literal(
            &serde_json::to_value(&compiled.metadata).map_err(|error| error.to_string())?
        )?,
        json_literal(args)?,
        json_literal(budget)?,
        serde_json::to_string(&compiled.body).map_err(|error| error.to_string())?
    ))
}

#[cfg(test)]
#[path = "compiler_tests.rs"]
mod tests;
