//! Literal JSON spelling and the supplied JavaScript layout heuristics.
use serde_json::Map;
use serde_json::Value;

pub(super) fn leaf(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => true,
    }
}

fn source_len(text: &str) -> usize {
    text.encode_utf16().count()
}

pub(super) fn escape(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' | '\n' => {
                if ch == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                output.push_str(" ⏎ ");
            }
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '<' | '>' | '|' | '#' | '!' | '+'
            | '~' | '-' => {
                output.push('\\');
                output.push(ch);
            }
            // Unlike the JS CLI, a terminal display must not execute embedded controls.
            ch if ch.is_control() => {
                use std::fmt::Write;
                let _ = write!(output, "\\\\u{:04x}", u32::from(ch));
            }
            _ => output.push(ch),
        }
    }
    output
}

pub(super) fn label(key: &str) -> String {
    if key.is_empty() {
        "`\"\"`".into()
    } else {
        escape(key)
    }
}

pub(super) fn value(value: &Value) -> String {
    match value {
        Value::Array(_) => "`[]`".into(),
        Value::Object(_) => "`{}`".into(),
        Value::String(text) if text.is_empty() => "`\"\"`".into(),
        Value::String(text) => escape(text),
        Value::Null | Value::Bool(_) | Value::Number(_) => format!("`{value}`"),
    }
}

pub(super) fn small(value: &Value, short: usize) -> bool {
    leaf(value)
        && match value {
            Value::String(text) => source_len(text),
            _ => source_len(&value.to_string()),
        } <= short
}

/// Object.entries puts canonical array-index keys first, then other keys in insertion order.
pub(super) fn entries(fields: &Map<String, Value>) -> Vec<(&str, &Value)> {
    fn index(key: &str) -> Option<u32> {
        key.parse::<u32>()
            .ok()
            .filter(|value| *value != u32::MAX && value.to_string() == key)
    }
    let mut entries = fields
        .iter()
        .map(|(key, value)| (key.as_str(), value))
        .collect::<Vec<_>>();
    entries.sort_by_key(|(key, _)| index(key).map_or((1, 0), |index| (0, index)));
    entries
}

pub(super) fn pack(items: impl IntoIterator<Item = String>, width: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    for item in items {
        if let Some(last) = rows.last_mut()
            && source_len(last)
                .saturating_add(source_len(&item))
                .saturating_add(3)
                <= width
        {
            last.push_str(" · ");
            last.push_str(&item);
        } else {
            rows.push(item);
        }
    }
    rows
}
