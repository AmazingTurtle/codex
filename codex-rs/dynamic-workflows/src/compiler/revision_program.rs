//! Private prefix checkpoints and synchronous, non-authoritative suffix provenance.

use super::CompiledScript;
use super::RevisionProgram;

pub(super) fn prepare(
    compiled: &CompiledScript,
    frontier: usize,
) -> Result<RevisionProgram, String> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .map_err(|error| error.to_string())?;
    let tree = parser
        .parse(&compiled.body, /*old_tree*/ None)
        .ok_or("failed to parse workflow statements")?;
    let root = tree.root_node();
    if root.has_error() {
        return Err("invalid workflow statement syntax".into());
    }
    let mut cursor = root.walk();
    let nodes = root
        .named_children(&mut cursor)
        .filter(|node| node.kind() != "comment")
        .collect::<Vec<_>>();
    if frontier > nodes.len() {
        return Err("revision frontier exceeds its statement count".into());
    }
    let mut identifiers = std::collections::HashSet::new();
    let mut pending = vec![(root, false)];
    while let Some((node, inside_function)) = pending.pop() {
        if node.kind() == "yield_expression" && !inside_function {
            return Err("workflow source cannot yield revision checkpoints".into());
        }
        if node.kind().contains("identifier") {
            identifiers.insert(identifier(&compiled.body[node.byte_range()])?);
        }
        let inside_function = inside_function
            || matches!(
                node.kind(),
                "function_expression"
                    | "function_declaration"
                    | "generator_function"
                    | "generator_function_declaration"
                    | "arrow_function"
                    | "method_definition"
            );
        let mut cursor = node.walk();
        pending.extend(
            node.named_children(&mut cursor)
                .map(|child| (child, inside_function)),
        );
    }
    let mut marker = "__workflowStatement".to_string();
    let mut candidate = 0;
    while compiled.body.contains(&marker) || identifiers.contains(&marker) {
        candidate += 1;
        marker = format!("__workflowStatement_{candidate}");
    }
    let mut body = String::with_capacity(compiled.body.len() + nodes.len() * 32);
    let mut offset = 0;
    for (index, node) in nodes.iter().enumerate() {
        body.push_str(&compiled.body[offset..node.start_byte()]);
        if index <= frontier {
            body.push_str(&format!("yield {index};\n"));
        }
        if index >= frontier {
            body.push_str(&format!("{marker}({index});\n"));
        }
        body.push_str(&compiled.body[node.byte_range()]);
        offset = node.end_byte();
    }
    body.push_str(&compiled.body[offset..]);
    if frontier == nodes.len() {
        body.push_str(&format!("\nyield {frontier};"));
    }
    Ok(RevisionProgram {
        body,
        statement_count: nodes.len(),
        marker,
        frontier,
    })
}

fn identifier(source: &str) -> Result<String, String> {
    let mut output = String::with_capacity(source.len());
    let mut chars = source.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        if chars.next() != Some('u') {
            return Err("unsupported workflow identifier escape".into());
        }
        let first = chars
            .next()
            .ok_or("incomplete workflow identifier escape")?;
        let digits = if first == '{' {
            let mut digits = String::new();
            loop {
                let character = chars
                    .next()
                    .ok_or("incomplete workflow identifier escape")?;
                if character == '}' {
                    break;
                }
                digits.push(character);
            }
            digits
        } else {
            let mut digits = String::from(first);
            for _ in 0..3 {
                digits.push(
                    chars
                        .next()
                        .ok_or("incomplete workflow identifier escape")?,
                );
            }
            digits
        };
        let code =
            u32::from_str_radix(&digits, 16).map_err(|_| "invalid workflow identifier escape")?;
        output.push(char::from_u32(code).ok_or("invalid workflow identifier character")?);
    }
    Ok(output)
}
