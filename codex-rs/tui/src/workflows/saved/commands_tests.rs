use super::*;
use pretty_assertions::assert_eq;

fn row(id: &str, alias: &str, scope: WorkflowScope) -> SavedWorkflowRow {
    SavedWorkflowRow {
        id: id.into(),
        name: id.split_once(':').unwrap().1.into(),
        scope: Some(scope),
        metadata: Some(
            serde_json::from_value(
                serde_json::json!({"name":alias,"description":"Inspect source","phases":[]}),
            )
            .unwrap(),
        ),
        ..Default::default()
    }
}

#[test]
fn metadata_aliases_keep_project_precedence_and_scoped_entries_remain_visible() {
    let rows = vec![
        row("user:review", "source-review", WorkflowScope::User),
        row(
            "project:other-file",
            "source-review",
            WorkflowScope::Project,
        ),
    ];
    let candidates = candidates(&rows);
    assert_eq!(
        catalog(&candidates, &[]),
        vec![SavedCommand {
            id: "project:other-file".into(),
            name: "source-review".into(),
            description: "Inspect source".into()
        }]
    );
    assert!(
        command_notice(&rows[0], &candidates, &[])
            .unwrap()
            .contains("project workflow")
    );
    assert_eq!(command_notice(&rows[1], &candidates, &[]), None);
}

#[test]
fn collisions_and_legacy_entries_have_visible_fallback_without_overriding_native_commands() {
    let mut rows = vec![
        row("project:first", "source-review", WorkflowScope::Project),
        row("project:second", "Source-review", WorkflowScope::Project),
        row("project:status-file", "Status", WorkflowScope::Project),
        row("user:tier-file", "FAST", WorkflowScope::User),
        row("user:legacy", "legacy", WorkflowScope::User),
    ];
    rows[4].read_only = true;
    rows[4].read_only_reason = Some("Legacy workflow is read-only".into());
    let tiers = vec![ServiceTierCommand {
        id: "priority".into(),
        name: "fast".into(),
        description: "Fast inference".into(),
    }];
    let candidates = candidates(&rows);
    assert!(catalog(&candidates, &tiers).is_empty());
    for row in &rows[..4] {
        assert!(
            command_notice(row, &candidates, &tiers)
                .unwrap()
                .contains("/workflows")
        );
    }
    assert_eq!(
        command_notice(&rows[4], &candidates, &tiers).as_deref(),
        Some("Legacy workflow is read-only")
    );
}

#[test]
fn raw_arguments_preserve_json_values_and_text_without_coercing_absence_to_null() {
    assert_eq!(
        [
            input(None),
            input(Some("null")),
            input(Some("[1,2]")),
            input(Some("42")),
            input(Some("false")),
            input(Some("run cargo --release"))
        ],
        [
            serde_json::json!({}),
            serde_json::Value::Null,
            serde_json::json!([1, 2]),
            serde_json::json!(42),
            serde_json::json!(false),
            serde_json::json!("run cargo --release")
        ]
    );
}
