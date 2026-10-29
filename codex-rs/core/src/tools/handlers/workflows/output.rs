//! JSON projections budget their serialized bytes before entering model context.

use crate::workflows::WorkflowRun;
use codex_dynamic_workflows::SavedWorkflow;
use codex_dynamic_workflows::WorkflowSummary;
use serde_json::Value;
use serde_json::json;

pub(super) const MAX_OUTPUT_BYTES: usize = 3200;

pub(super) fn summary(run: &WorkflowRun) -> Value {
    json!({"id":run.id,"execution_id":run.execution_id,"format_version":run.format_version,"read_only":run.format_version != codex_dynamic_workflows::WORKFLOW_FORMAT_VERSION,"status":run.status,"phase":run.phase.as_deref().map(|text|preview(text,128)),"steps":run.steps.len(),"accepted_starts":run.accepted_starts,"limits":run.limits,"metrics":run.metrics,"error":run.error.as_deref().map(|text|preview(text,128))})
}

pub(super) fn run_summary(run: &WorkflowSummary) -> Value {
    json!({"id":run.id,"execution_id":run.execution_id,"format_version":run.format_version,"read_only":run.format_version != codex_dynamic_workflows::WORKFLOW_FORMAT_VERSION,"status":run.status,"phase":run.phase.as_deref().map(|text|preview(text,128)),"steps":run.step_count,"accepted_starts":run.accepted_starts,"limits":run.limits,"metrics":run.metrics})
}

pub(super) fn saved_summary(saved: &SavedWorkflow) -> Value {
    json!({
        "id":saved.id,"name":saved.name,"scope":saved.scope,
        "format_version":saved.format_version,"read_only":saved.read_only,
        "read_only_reason":saved.read_only_reason.as_deref().map(|text|preview(text,128)),
        "metadata":saved.metadata.as_ref().map(|metadata|json!({"name":metadata.name,"description":preview(&metadata.description,256)})),
    })
}

// Preview budgets include JSON escaping, while identifiers remain exact in their paged fields.
pub(super) fn preview(text: &str, max_bytes: usize) -> String {
    let mut end = text.floor_char_boundary(max_bytes.min(text.len()));
    loop {
        let preview = if end == text.len() {
            text.to_string()
        } else {
            format!("{}…", &text[..end])
        };
        if json!(&preview).to_string().len() <= max_bytes {
            return preview;
        }
        if end == 0 {
            return String::new();
        }
        end = text.floor_char_boundary(end.saturating_sub(1));
    }
}

pub(super) fn bounded_page(
    mut page: Value,
    field: &str,
    offset: usize,
    entries: Vec<Value>,
    has_more: bool,
) -> Result<Value, String> {
    page[field] = json!([]);
    page["next_offset"] = Value::Null;
    let total = entries.len();
    for (included, entry) in entries.into_iter().enumerate() {
        let Some(values) = page[field].as_array_mut() else {
            return Err("invalid workflow page".into());
        };
        values.push(entry);
        let next = offset.saturating_add(included + 1);
        page["next_offset"] = if has_more || included + 1 < total {
            json!(next)
        } else {
            Value::Null
        };
        if page.to_string().len() > MAX_OUTPUT_BYTES {
            if let Some(values) = page[field].as_array_mut() {
                values.pop();
            }
            if included == 0 {
                return Err("workflow page entry exceeds its output budget".into());
            }
            page["next_offset"] = json!(offset.saturating_add(included));
            break;
        }
    }
    if page.to_string().len() > MAX_OUTPUT_BYTES {
        return Err("workflow page exceeds its output budget".into());
    }
    Ok(page)
}

pub(super) fn content_page(text: &str, offset: usize, max_bytes: usize) -> Result<Value, String> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return Err("offset must be a UTF-8 byte boundary within the content".into());
    }
    let end = text.floor_char_boundary(offset.saturating_add(max_bytes).min(text.len()));
    Ok(
        json!({"content":&text[offset..end],"offset":offset,"next_offset":(end < text.len()).then_some(end),"total_bytes":text.len()}),
    )
}
