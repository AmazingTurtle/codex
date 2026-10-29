use super::*;
use codex_app_server_protocol::ThreadTimelineEntry;
use pretty_assertions::assert_eq;
use ratatui::style::Modifier;
use serde_json::json;

pub(super) fn task(sequence: u64) -> WorkflowLifecycleEvent {
    serde_json::from_value(json!({
        "id": format!("fact-{sequence}"), "ownerThreadId":"owner", "runId":"run",
        "executionId":"execution", "workflowName":"review", "completionMode":"display",
        "sequence":sequence, "createdAt":1,
        "kind":{"type":"stepCompleted","stepId":format!("step-{sequence}"),
            "attemptId":format!("attempt-{sequence}"), "childThreadId":format!("child-{sequence}"),
            "ownedTurnIds":[format!("turn-{sequence}")], "finalTurnId":format!("turn-{sequence}"),
            "label":format!("Review {sequence}"),"phase":null,"status":"succeeded",
            "result":{"type":"absent"},"error":null,"resolvedModel":null,
            "reasoningEffort":null,"activeDurationMs":220_000}
    }))
    .expect("persisted workflow task fact")
}

#[test]
fn adjacent_successes_keep_original_ids_and_recorded_elapsed_in_one_bounded_group_narrow() {
    assert_completed_task_group(/*width*/ 28);
}

#[test]
fn adjacent_successes_keep_original_ids_and_recorded_elapsed_in_one_bounded_group_wide() {
    assert_completed_task_group(/*width*/ 88);
}

fn assert_completed_task_group(width: u16) {
    let mut group = WorkflowLifecycleHistoryCell::new(task(1));
    assert!(group.append(&WorkflowLifecycleHistoryCell::new(task(2))));
    assert_eq!(
        group.event_ids().collect::<Vec<_>>(),
        vec!["fact-1", "fact-2"]
    );
    let lines = group.display_lines(width);
    assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
    insta::assert_snapshot!(
        format!("workflow_completed_tasks_{width}"),
        lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
    for span in lines.iter().flat_map(|line| &line.spans) {
        if span.style.fg == Some(crate::style::accent_color()) {
            assert!(span.style.sub_modifier.contains(Modifier::BOLD));
        }
        if span.content.contains("Completed tasks") {
            assert!(span.style.add_modifier.contains(Modifier::BOLD));
        }
    }
    for sequence in 3..=64 {
        assert!(group.append(&WorkflowLifecycleHistoryCell::new(task(sequence))));
    }
    assert!(!group.append(&WorkflowLifecycleHistoryCell::new(task(65))));
    assert_eq!(group.event_ids().count(), 64);
    assert_eq!(
        group.raw_lines().len(),
        64,
        "raw copy preserves every original fact"
    );
}

#[test]
fn failed_or_different_execution_facts_cannot_join_and_unknown_duration_is_not_zero() {
    let first = WorkflowLifecycleHistoryCell::new(task(1));
    for boundary in ["owner", "run", "execution", "failure", "other lifecycle"] {
        let mut event = task(2);
        match boundary {
            "owner" => event.owner_thread_id = "another-owner".into(),
            "run" => event.run_id = "another-run".into(),
            "execution" => event.execution_id = "another-execution".into(),
            "failure" => {
                if let WorkflowLifecycleKind::StepCompleted { status, .. } = &mut event.kind {
                    *status = WorkflowStatus::Failed;
                }
            }
            "other lifecycle" => {
                event.kind = WorkflowLifecycleKind::Started {
                    name: "review".into(),
                    description: "Next run".into(),
                }
            }
            _ => unreachable!(),
        }
        let mut group = first.clone();
        assert!(
            !group.append(&WorkflowLifecycleHistoryCell::new(event)),
            "{boundary}"
        );
        assert_eq!(group.event_ids().collect::<Vec<_>>(), vec!["fact-1"]);
    }
    let mut unknown = task(1);
    if let WorkflowLifecycleKind::StepCompleted {
        active_duration_ms, ..
    } = &mut unknown.kind
    {
        *active_duration_ms = None;
    }
    assert_eq!(
        WorkflowLifecycleHistoryCell::new(unknown)
            .heading()
            .to_string(),
        "• Completed task Review 1 in workflow review"
    );
}

#[test]
fn page_join_requires_actual_canonical_adjacency_not_just_matching_workflow_identity() {
    let first = task(1);
    let second = task(2);
    let entries = vec![first.clone(), second.clone()]
        .into_iter()
        .enumerate()
        .map(|(position, event)| ThreadTimelineEntry::Workflow {
            position: position as u64,
            event: Box::new(event),
        })
        .collect::<Vec<_>>();
    let mut group = WorkflowLifecycleHistoryCell::new(first.clone());
    assert!(
        group.append_from_timeline(&WorkflowLifecycleHistoryCell::new(second.clone()), &entries)
    );
    let mut interrupted = entries;
    interrupted.insert(
        1,
        ThreadTimelineEntry::TurnStarted {
            position: 1,
            turn_id: "ordinary".into(),
            started_at: None,
        },
    );
    let mut group = WorkflowLifecycleHistoryCell::new(first);
    assert!(!group.append_from_timeline(&WorkflowLifecycleHistoryCell::new(second), &interrupted));
    assert_eq!(group.event_ids().collect::<Vec<_>>(), vec!["fact-1"]);
}
