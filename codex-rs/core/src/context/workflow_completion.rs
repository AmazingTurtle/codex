//! A bounded terminal notice; full workflow output stays behind paged reads.

use super::ContextualUserFragment;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::input_text;
use codex_utils_string::take_bytes_at_char_boundary;
use uuid::Uuid;

pub(crate) struct WorkflowCompletion {
    id: Uuid,
    status: &'static str,
    completed_steps: usize,
    total_steps: usize,
    error: Option<String>,
}

impl WorkflowCompletion {
    pub(crate) fn new(
        id: String,
        status: String,
        completed_steps: usize,
        total_steps: usize,
        error: Option<String>,
    ) -> Self {
        Self {
            id: Uuid::parse_str(&id).unwrap_or_default(),
            status: match status.as_str() {
                "succeeded" => "succeeded",
                "failed" => "failed",
                "stopped" => "stopped",
                "interrupted" => "interrupted",
                _ => "unknown",
            },
            completed_steps: completed_steps.min(total_steps),
            total_steps,
            error: error.map(|error| take_bytes_at_char_boundary(&error, 256).to_string()),
        }
    }
}

impl ContextualUserFragment for WorkflowCompletion {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("dynamic_workflows.completion".to_string())
    }

    fn role(&self) -> &'static str {
        "assistant"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<workflow_status>", "</workflow_status>")
    }

    fn body(&self) -> ContentItem {
        let summary = serde_json::json!({
            "run_id": self.id,
            "status": self.status,
            "completed_steps": self.completed_steps,
            "total_steps": self.total_steps,
            "error": self.error,
        });
        input_text(format!(
            "\n{summary}\nRead the final result with workflow_read({{run_id:\"{}\",section:\"result\",offset:0}}); follow next_offset for subsequent pages.\n",
            self.id,
        ))
    }
}

#[cfg(test)]
#[path = "workflow_completion_tests.rs"]
mod tests;
