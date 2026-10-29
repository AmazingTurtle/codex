use super::*;
use codex_app_server_protocol::WorkflowCompletionMode;
use codex_app_server_protocol::WorkflowMetrics;
use pretty_assertions::assert_eq;
use serde_json::json;

fn completed(result: WorkflowResult) -> WorkflowLifecycleEvent {
    WorkflowLifecycleEvent {
        id: "event-1".into(),
        owner_thread_id: "owner".into(),
        run_id: "run-1".into(),
        execution_id: "execution-1".into(),
        workflow_name: "review".into(),
        completion_mode: WorkflowCompletionMode::Display,
        sequence: 2,
        created_at: 1,
        kind: WorkflowLifecycleKind::Completed {
            name: "review".into(),
            status: WorkflowStatus::Succeeded,
            result,
            error: None,
            metrics: WorkflowMetrics::default(),
        },
    }
}

#[test]
fn final_result_keeps_explicit_null_distinct_from_absence() {
    let absent = WorkflowLifecycleHistoryCell::new(completed(WorkflowResult::Absent));
    let null =
        WorkflowLifecycleHistoryCell::new(completed(WorkflowResult::Value { value: json!(null) }));
    let text = |cell: &WorkflowLifecycleHistoryCell| {
        cell.raw_lines()
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(text(&absent), vec!["• Completed workflow review"]);
    assert_eq!(text(&null), vec!["• Completed workflow review", "null"]);
}

#[test]
fn final_result_uses_shared_structured_rendering_at_narrow_width() {
    assert_shared_structured_result(/*width*/ 28);
}

#[test]
fn final_result_uses_shared_structured_rendering_at_wide_width() {
    assert_shared_structured_result(/*width*/ 88);
}

fn assert_shared_structured_result(width: u16) {
    let cell = WorkflowLifecycleHistoryCell::new(completed(WorkflowResult::Value {
        value: json!({
            "summary":"Ready for **review**", "findings":[{"file":"src/main.rs","status":"checked"}],
            "nested":{"items":["Read the change", "Verify the result"]},
        }),
    }));
    let lines = cell.display_lines(width);
    assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
    insta::assert_snapshot!(
        format!("workflow_final_result_{width}"),
        lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn stopped_workflow_reports_its_status_without_claiming_completion() {
    let mut event = completed(WorkflowResult::Absent);
    if let WorkflowLifecycleKind::Completed { status, error, .. } = &mut event.kind {
        *status = WorkflowStatus::Stopped;
        *error = Some("Stopped by the user".into());
    }
    let cell = WorkflowLifecycleHistoryCell::new(event);
    assert_eq!(
        cell.raw_lines()
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>(),
        vec!["• Stopped workflow review", "Stopped by the user"]
    );
}

#[test]
fn skipped_step_reports_no_fictitious_agent_completion() {
    let mut event = completed(WorkflowResult::Absent);
    event.kind = WorkflowLifecycleKind::StepSkipped {
        step_id: "step-review".into(),
        logical_id: "root/review".into(),
        label: "Review".into(),
        phase: Some("Review".into()),
        command_id: "skip-1".into(),
        result: WorkflowResult::Value {
            value: json!({"replacement":"Used without starting an agent"}),
        },
    };
    let cell = WorkflowLifecycleHistoryCell::new(event);
    assert_eq!(
        cell.raw_lines()
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>(),
        vec!["• Skipped task Review in workflow review"]
    );
    insta::assert_snapshot!(
        "workflow_skipped_task",
        cell.display_lines(60)
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn lifecycle_names_use_agent_accent_without_inheriting_bold_at_narrow_width() {
    assert_lifecycle_name_styles(28);
}

#[test]
fn lifecycle_names_use_agent_accent_without_inheriting_bold_at_wide_width() {
    assert_lifecycle_name_styles(88);
}

fn assert_lifecycle_name_styles(width: u16) {
    use ratatui::style::Modifier;
    use ratatui::style::Style;
    let workflow = "review-change";
    let task = "Review 日本語";
    let task_event = |status| WorkflowLifecycleKind::StepCompleted {
        step_id: "step-1".into(),
        attempt_id: "attempt-1".into(),
        child_thread_id: "child-1".into(),
        owned_turn_ids: vec!["turn-1".into()],
        final_turn_id: Some("turn-1".into()),
        label: task.into(),
        phase: None,
        status,
        result: WorkflowResult::Absent,
        error: None,
        resolved_model: None,
        reasoning_effort: None,
        active_duration_ms: Some(220_000),
    };
    let events = vec![
        (
            "started",
            WorkflowLifecycleKind::Started {
                name: workflow.into(),
                description: "Review a change".into(),
            },
            vec![workflow],
        ),
        (
            "task completed",
            task_event(WorkflowStatus::Succeeded),
            vec![task, workflow],
        ),
        (
            "task failed",
            task_event(WorkflowStatus::Failed),
            vec![task, workflow],
        ),
        (
            "task stopped",
            task_event(WorkflowStatus::Stopped),
            vec![task, workflow],
        ),
        (
            "task skipped",
            WorkflowLifecycleKind::StepSkipped {
                step_id: "step-1".into(),
                logical_id: "root/review".into(),
                label: task.into(),
                phase: None,
                command_id: "skip-1".into(),
                result: WorkflowResult::Absent,
            },
            vec![task, workflow],
        ),
        (
            "completed",
            WorkflowLifecycleKind::Completed {
                name: workflow.into(),
                status: WorkflowStatus::Succeeded,
                result: WorkflowResult::Absent,
                error: None,
                metrics: WorkflowMetrics::default(),
            },
            vec![workflow],
        ),
        (
            "interrupted",
            WorkflowLifecycleKind::Interrupted {
                name: workflow.into(),
                reason: String::new(),
                native_settled: true,
                metrics: WorkflowMetrics::default(),
            },
            vec![workflow],
        ),
        (
            "result",
            WorkflowLifecycleKind::ResultShown {
                name: workflow.into(),
                status: WorkflowStatus::Succeeded,
                result: WorkflowResult::Absent,
                error: None,
            },
            vec![workflow],
        ),
    ];
    let mut rendered = Vec::new();
    for (name, kind, names) in &events {
        let mut event = completed(WorkflowResult::Absent);
        event.workflow_name = workflow.into();
        event.kind = kind.clone();
        let cell = WorkflowLifecycleHistoryCell::new(event);
        let heading = cell.heading();
        assert_eq!(
            heading
                .spans
                .iter()
                .filter(|span| span.style.fg == Some(crate::style::accent_color()))
                .map(|span| span.content.as_ref())
                .collect::<Vec<_>>(),
            *names
        );
        let lines = cell.display_lines(width);
        assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
        let spans = lines.iter().map(|line| {
                assert!(!line.style.add_modifier.contains(Modifier::BOLD),"the heading must not impose bold on all its text");
                let spans = line.spans.iter().map(|span| {
                    let accent = span.style.fg == Some(crate::style::accent_color());
                    let effective = Style::default().bold().patch(line.style).patch(span.style);
                    if accent {assert!(!effective.add_modifier.contains(Modifier::BOLD),"variable names explicitly cancel inherited bold");}
                    json!({"text":span.content,"accent":accent,"bold":line.style.patch(span.style).add_modifier.contains(Modifier::BOLD),"clears_bold":span.style.sub_modifier.contains(Modifier::BOLD)})
                }).collect::<Vec<_>>();
                json!({"text":line.to_string(),"spans":spans})
            }).collect::<Vec<_>>();
        rendered.push(json!({"event":name,"lines":spans}));
    }
    insta::assert_snapshot!(
        format!("workflow_lifecycle_name_styles_{width}"),
        serde_json::to_string_pretty(&rendered).unwrap()
    );
}
