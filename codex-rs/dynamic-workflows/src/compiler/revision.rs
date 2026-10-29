//! Conservative statement frontiers for replaying an immutable revision prefix.

use super::CompiledScript;
use super::compile_script;
use std::collections::HashSet;
use tree_sitter::Node;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionAnalysis {
    pub reusable_statements: usize,
    pub original_statements: usize,
    pub revised_statements: usize,
    pub invalidation_reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionProgram {
    pub body: String,
    pub statement_count: usize,
    pub frontier: usize,
    pub marker: String,
}

/// Compare exact authored statement text, then exclude ambiguous influence or escaping work.
pub fn analyze_revision(
    original: &str,
    revised: &str,
    args_changed: bool,
) -> Result<RevisionAnalysis, String> {
    let old = compile_script(original)?;
    let new = compile_script(revised)?;
    let old_tree = parse(&old.body)?;
    let new_tree = parse(&new.body)?;
    let old_statements = statements(old_tree.root_node());
    let new_statements = statements(new_tree.root_node());
    let mut prefix = old_statements
        .iter()
        .zip(&new_statements)
        .take_while(|(old_node, new_node)| {
            text(**old_node, &old.body) == text(**new_node, &new.body)
        })
        .count();
    let mut invalidation_reason =
        if prefix == old_statements.len() && prefix == new_statements.len() {
            "All executable statements are unchanged".to_string()
        } else if old_statements
            .get(prefix)
            .is_some_and(|node| atomic(*node, &old.body))
            || new_statements
                .get(prefix)
                .is_some_and(|node| atomic(*node, &new.body))
        {
            "A changed parallel, pipeline, or nested-workflow statement must rerun as a unit"
                .to_string()
        } else {
            format!("Executable statement {} changed", prefix + 1)
        };
    if args_changed {
        prefix = 0;
        invalidation_reason = "Workflow arguments changed".into();
    } else if old.metadata.phases != new.metadata.phases {
        prefix = 0;
        invalidation_reason = "Declared workflow phases changed".into();
    }
    let changed_bindings = old_statements[prefix..]
        .iter()
        .flat_map(|node| bindings(*node, &old.body))
        .chain(
            new_statements[prefix..]
                .iter()
                .flat_map(|node| bindings(*node, &new.body)),
        )
        .collect::<HashSet<_>>();
    if prefix != 0
        && (changed_bindings.iter().any(|name| name.contains('\\'))
            || old_statements[..prefix].iter().any(|node| {
                identifiers(*node, &old.body)
                    .iter()
                    .any(|name| name.contains('\\') || changed_bindings.contains(name))
            }))
    {
        prefix = 0;
        invalidation_reason =
            "Changed declarations or escaped identifiers can affect earlier statements".into();
    }
    for (index, node) in new_statements[..prefix].iter().enumerate() {
        if !replay_safe(*node, &new.body) {
            prefix = index;
            invalidation_reason =
                "Earlier work may escape its statement or use an ambiguous dynamic call".into();
            break;
        }
    }
    Ok(RevisionAnalysis {
        reusable_statements: prefix,
        original_statements: old_statements.len(),
        revised_statements: new_statements.len(),
        invalidation_reason,
    })
}

/// Yield checkpoints from an inaccessible async generator, without exposing a callable capability.
/// The bootstrap owns the iterator and validates each yielded statement index before host gating.
pub fn instrument_revision_script(
    compiled: &CompiledScript,
    frontier: usize,
) -> Result<RevisionProgram, String> {
    super::revision_program::prepare(compiled, frontier)
}

fn parse(source: &str) -> Result<tree_sitter::Tree, String> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .map_err(|error| error.to_string())?;
    let tree = parser
        .parse(source, /*old_tree*/ None)
        .ok_or("failed to parse workflow statements")?;
    if tree.root_node().has_error() {
        return Err("invalid workflow statement syntax".into());
    }
    Ok(tree)
}

fn statements(root: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter(|node| node.kind() != "comment")
        .collect()
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn atomic(node: Node<'_>, source: &str) -> bool {
    descendants(node).iter().any(|node| {
        node.kind() == "call_expression"
            && node
                .child_by_field_name("function")
                .is_some_and(|function| {
                    matches!(
                        text(function, source),
                        "workflow" | "workflow.parallel" | "workflow.pipeline"
                    )
                })
    })
}

fn descendants(root: Node<'_>) -> Vec<Node<'_>> {
    let mut result = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        result.push(node);
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    result
}

fn identifiers(node: Node<'_>, source: &str) -> HashSet<String> {
    descendants(node)
        .into_iter()
        .filter(|node| {
            matches!(
                node.kind(),
                "identifier"
                    | "shorthand_property_identifier"
                    | "shorthand_property_identifier_pattern"
            )
        })
        .map(|node| text(node, source).to_string())
        .collect()
}

fn bindings(node: Node<'_>, source: &str) -> HashSet<String> {
    let mut found = declaration_bindings(node, source);
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        if current != node
            && matches!(
                current.kind(),
                "function_expression"
                    | "function_declaration"
                    | "generator_function"
                    | "generator_function_declaration"
                    | "arrow_function"
                    | "method_definition"
            )
        {
            continue;
        }
        if current != node && current.kind() == "variable_declaration" {
            found.extend(declaration_bindings(current, source));
        }
        let mut cursor = current.walk();
        pending.extend(current.named_children(&mut cursor));
    }
    found
}

fn declaration_bindings(node: Node<'_>, source: &str) -> HashSet<String> {
    match node.kind() {
        "lexical_declaration" | "variable_declaration" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .filter_map(|declaration| declaration.child_by_field_name("name"))
                .flat_map(|name| identifiers(name, source))
                .collect()
        }
        "function_declaration" | "generator_function_declaration" | "class_declaration" => node
            .child_by_field_name("name")
            .map(|name| HashSet::from([text(name, source).to_string()]))
            .unwrap_or_default(),
        _ => HashSet::new(),
    }
}

fn awaited(mut node: Node<'_>) -> bool {
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "await_expression" | "return_statement" => return true,
            "parenthesized_expression" => node = parent,
            "arrow_function" if parent.child_by_field_name("body") == Some(node) => return true,
            _ => return false,
        }
    }
    false
}

fn replay_safe(statement: Node<'_>, source: &str) -> bool {
    for node in descendants(statement) {
        match node.kind() {
            "call_expression" | "new_expression" => {
                let Some(function) = node
                    .child_by_field_name("function")
                    .or_else(|| node.child_by_field_name("constructor"))
                else {
                    return false;
                };
                let called = text(function, source);
                let workflow = matches!(
                    called,
                    "workflow"
                        | "workflow.agent"
                        | "workflow.parallel"
                        | "workflow.pipeline"
                        | "workflow.phase"
                        | "workflow.log"
                );
                if workflow && !awaited(node) {
                    return false;
                }
                if !workflow
                    && !matches!(
                        called,
                        "workflow.budget.spent"
                            | "workflow.budget.remaining"
                            | "JSON.stringify"
                            | "JSON.parse"
                    )
                {
                    return false;
                }
            }
            "member_expression" if text(node, source).starts_with("workflow.") => {
                let field = text(node, source);
                if matches!(
                    field,
                    "workflow.agent"
                        | "workflow.parallel"
                        | "workflow.pipeline"
                        | "workflow.phase"
                        | "workflow.log"
                ) && !node.parent().is_some_and(|parent| {
                    parent.kind() == "call_expression"
                        && parent.child_by_field_name("function") == Some(node)
                }) {
                    return false;
                }
            }
            "identifier" if text(node, source) == "workflow" => {
                if !node.parent().is_some_and(|parent| {
                    (parent.kind() == "member_expression"
                        && parent.child_by_field_name("object") == Some(node))
                        || (parent.kind() == "call_expression"
                            && parent.child_by_field_name("function") == Some(node))
                }) {
                    return false;
                }
            }
            "identifier"
                if matches!(
                    text(node, source),
                    "eval" | "Function" | "AsyncFunction" | "globalThis"
                ) =>
            {
                return false;
            }
            "assignment_expression" | "augmented_assignment_expression" | "update_expression"
                if node
                    .child_by_field_name("left")
                    .or_else(|| node.child_by_field_name("argument"))
                    .is_some_and(|target| {
                        matches!(target.kind(), "member_expression" | "subscript_expression")
                    }) =>
            {
                return false;
            }
            "with_statement" | "yield_expression" | "class_declaration" | "method_definition" => {
                return false;
            }
            _ => {}
        }
    }
    true
}

#[cfg(test)]
#[path = "revision_tests.rs"]
mod tests;
