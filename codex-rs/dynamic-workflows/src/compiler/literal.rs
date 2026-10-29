//! Decode the pure literal subset accepted in workflow metadata, without JavaScript evaluation.

use serde_json::Value;
use tree_sitter::Node;

mod string;

pub(super) fn value(node: Node<'_>, source: &str, depth: usize) -> Result<Value, String> {
    if depth > 16 {
        return Err("workflow metadata nesting exceeds 16 levels".into());
    }
    match node.kind() {
        "string" => Ok(Value::String(string::decode(node.utf8_text(source.as_bytes()).map_err(|error| error.to_string())?)?)),
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        "null" => Ok(Value::Null),
        "number" => serde_json::from_str(node.utf8_text(source.as_bytes()).map_err(|error| error.to_string())?).map_err(|error| error.to_string()),
        "object" => {
            let mut values = serde_json::Map::new();
            let mut cursor = node.walk();
            for property in node.named_children(&mut cursor).filter(|node| node.kind() != "comment") {
                if property.kind() != "pair" { return Err("workflow metadata properties must be literal key/value pairs; spreads, functions and shorthand are unavailable".into()); }
                let key = property.child_by_field_name("key").ok_or("workflow metadata property needs a key")?;
                let key = match key.kind() {
                    "property_identifier" => key.utf8_text(source.as_bytes()).map_err(|error| error.to_string())?.to_owned(),
                    "string" => string::decode(key.utf8_text(source.as_bytes()).map_err(|error| error.to_string())?)?,
                    _ => return Err("workflow metadata keys cannot be computed".into()),
                };
                if values.contains_key(&key) { return Err("workflow metadata cannot repeat a property".into()); }
                let item = property.child_by_field_name("value").ok_or("workflow metadata property needs a value")?;
                values.insert(key, value(item, source, depth + 1)?);
            }
            Ok(Value::Object(values))
        }
        "array" => {
            let mut values = Vec::new();
            let mut expecting_value = true;
            let mut cursor = node.walk();
            for item in node.children(&mut cursor) {
                match item.kind() {
                    "[" | "]" | "comment" => {}
                    "," if expecting_value => return Err("workflow metadata arrays cannot contain holes".into()),
                    "," => expecting_value = true,
                    _ if item.is_named() => {
                        values.push(value(item, source, depth + 1)?);
                        expecting_value = false;
                    }
                    _ => return Err("invalid workflow metadata array".into()),
                }
            }
            Ok(Value::Array(values))
        }
        _ => Err("workflow metadata must contain only literals; computed values, templates and functions are unavailable".into()),
    }
}
