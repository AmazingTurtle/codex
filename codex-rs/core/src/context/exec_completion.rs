use codex_protocol::models::ContentItemKind;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::truncate_text;

use super::ContextualUserFragment;

const CALL_ID_TOKENS: usize = 64;
const COMMAND_TOKENS: usize = 512;
const OUTPUT_TOKENS: usize = 7_200;

/// Model-visible completion of a unified exec process that previously yielded.
pub(crate) struct ExecCompletion {
    body: String,
}

impl ExecCompletion {
    pub(crate) fn new(
        call_id: &str,
        process_id: i32,
        command: &[String],
        exit_code: i32,
        output: &str,
    ) -> Self {
        let call_id = truncate_text(call_id, TruncationPolicy::Tokens(CALL_ID_TOKENS));
        let command = truncate_text(&command.join(" "), TruncationPolicy::Tokens(COMMAND_TOKENS));
        let output = truncate_text(output, TruncationPolicy::Tokens(OUTPUT_TOKENS));
        Self {
            body: format!(
                "<exec-command-completed call-id=\"{call_id}\" process-id=\"{process_id}\" exit-code=\"{exit_code}\">\n<command>{command}</command>\n<output>\n{output}\n</output>\n</exec-command-completed>"
            ),
        }
    }
}

impl ContextualUserFragment for ExecCompletion {
    fn role(&self) -> &'static str {
        "user"
    }

    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("exec.completion".to_string())
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn body(&self) -> String {
        self.body.clone()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }
}

#[cfg(test)]
#[path = "exec_completion_tests.rs"]
mod tests;
