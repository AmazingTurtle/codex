//! Ordered lossless pieces of one native workflow task, never independent tasks.

use super::ContextualUserFragment;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::input_text;

pub(crate) struct WorkflowTaskPart<'a> {
    pub(crate) payload: &'a str,
    pub(crate) index: usize,
    pub(crate) total: usize,
    pub(crate) author: &'a str,
    pub(crate) recipient: &'a str,
}

impl WorkflowTaskPart<'_> {
    /// Persisted on the original full message; fixes the partition algorithm version.
    pub(crate) const SOURCE_KIND: &str = "dynamic_workflows.task.v1";
    pub(crate) const PART_KIND: &str = "dynamic_workflows.task_part.v1";
}

impl ContextualUserFragment for WorkflowTaskPart<'_> {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind(Self::PART_KIND.to_string())
    }

    fn role(&self) -> &'static str {
        "assistant"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> ContentItem {
        input_text(format!(
            "Workflow task part {}/{}.\nTask name: {}\nSender: {}\nRead every part in order as one task.\n\n{}",
            self.index, self.total, self.recipient, self.author, self.payload,
        ))
    }
}
