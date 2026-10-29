//! Local JSON Schema validation; workflow schemas cannot retrieve external resources.
use serde_json::Value;

pub fn validate_schema(schema: &Value) -> Result<(), String> {
    crate::types::bounded_value(schema)?;
    if schema == &Value::Bool(false) {
        return Err("workflow schema contradiction: false rejects every result".into());
    }
    inspect(schema)?;
    jsonschema::options()
        .build(schema)
        .map_err(|error| format!("invalid workflow schema: {error}"))?;
    Ok(())
}

fn inspect(value: &Value) -> Result<(), String> {
    match value {
        Value::Object(fields) => {
            for key in ["$ref", "$dynamicRef"] {
                if let Some(reference) = fields.get(key).and_then(Value::as_str)
                    && !reference.starts_with('#')
                {
                    return Err("workflow schemas cannot reference external resources".into());
                }
            }
            if fields.get("type").and_then(Value::as_str) == Some("object")
                && fields.get("additionalProperties") == Some(&Value::Bool(false))
                && fields
                    .get("patternProperties")
                    .and_then(Value::as_object)
                    .is_none_or(serde_json::Map::is_empty)
                && let Some(required) = fields.get("required").and_then(Value::as_array)
            {
                let properties = fields.get("properties").and_then(Value::as_object);
                for name in required.iter().filter_map(Value::as_str) {
                    if properties.is_none_or(|properties| !properties.contains_key(name)) {
                        return Err(format!(
                            "workflow schema contradiction: required property {name} is forbidden by additionalProperties"
                        ));
                    }
                }
            }
            for (key, child) in fields {
                match key.as_str() {
                    "properties" | "patternProperties" | "$defs" | "definitions"
                    | "dependentSchemas" => {
                        if let Some(schemas) = child.as_object() {
                            for schema in schemas.values() {
                                inspect(schema)?;
                            }
                        }
                    }
                    "items"
                    | "additionalProperties"
                    | "unevaluatedProperties"
                    | "additionalItems"
                    | "propertyNames"
                    | "contains"
                    | "not"
                    | "if"
                    | "then"
                    | "else"
                    | "contentSchema"
                    | "allOf"
                    | "anyOf"
                    | "oneOf"
                    | "prefixItems" => inspect(child)?,
                    _ => {}
                }
            }
        }
        Value::Array(values) => {
            for child in values {
                inspect(child)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

pub(crate) fn validate_output(schema: &Value, result: &Value) -> Result<(), String> {
    let validator = jsonschema::options()
        .build(schema)
        .map_err(|error| error.to_string())?;
    validator
        .validate(result)
        .map_err(|error| format!("structured output validation failed: {error}"))
}
