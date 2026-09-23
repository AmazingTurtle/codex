use super::ExecCompletion;
use crate::context::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;
use codex_utils_output_truncation::approx_token_count;
use pretty_assertions::assert_eq;

#[test]
fn completion_identifies_process_and_bounds_model_input() {
    let completion = ExecCompletion::new(
        &"call".repeat(1_000),
        42,
        &["command".repeat(1_000)],
        7,
        &"output".repeat(10_000),
    );
    let body = completion.body();

    assert_eq!(
        completion.content_kind(),
        ContentItemKind("exec.completion".to_string())
    );
    assert_eq!(completion.role(), "user");
    assert!(body.contains("process-id=\"42\" exit-code=\"7\""));
    assert!(body.ends_with("</exec-command-completed>"));
    assert!(approx_token_count(&body) <= 8_000);
}
