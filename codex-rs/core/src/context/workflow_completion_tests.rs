use super::*;
use pretty_assertions::assert_eq;

#[test]
fn terminal_notice_bounds_untrusted_text_and_preserves_valid_utf8() {
    for error in ["\0".repeat(4_000), "é".repeat(4_000)] {
        let completion = WorkflowCompletion::new(
            "invalid".repeat(1_000),
            "invalid".repeat(1_000),
            usize::MAX,
            usize::MAX,
            Some(error),
        );
        let rendered = completion.render().into_text().expect("text notice");
        assert!(rendered.len() < 3_200);
        let encoded = rendered
            .split('\n')
            .find(|line| line.starts_with('{'))
            .expect("summary JSON");
        let summary: serde_json::Value =
            serde_json::from_str(encoded).expect("valid bounded summary");
        assert_eq!(
            (summary["run_id"].as_str(), summary["status"].as_str()),
            (
                Some("00000000-0000-0000-0000-000000000000"),
                Some("unknown")
            )
        );
        assert!(summary["error"].as_str().unwrap().len() <= 256);
    }
}

#[test]
fn success_notice_keeps_result_behind_the_paged_read() {
    let id = Uuid::new_v4();
    let completion = WorkflowCompletion::new(
        id.to_string(),
        "succeeded".into(),
        2,
        2,
        /*error*/ None,
    );
    let rendered = completion.render().into_text().expect("text notice");
    assert!(rendered.contains(&format!("run_id:\"{id}\",section:\"result\",offset:0")));
    assert!(!rendered.contains("\"result\":"));
}
