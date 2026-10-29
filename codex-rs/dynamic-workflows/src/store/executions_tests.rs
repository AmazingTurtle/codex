//! Real manifest replacement and cleanup must leave original execution records readable.
use super::super::Store;
use super::*;
use crate::AdmissionStatus;
use pretty_assertions::assert_eq;
use serde_json::json;

fn fixture() -> WorkflowRun {
    serde_json::from_value(json!({
        "id":uuid::Uuid::new_v4().to_string(),"executionId":uuid::Uuid::new_v4().to_string(),"formatVersion":2,
        "lifecycleVersion":1,"lifecycle":{"enabled":true},"ownerThreadId":"owner",
        "source":"workflow.meta({name:'archive',description:'Original script',phases:[]}); return null;",
        "input":{"original":true},"metadata":{"name":"archive","description":"Original script","phases":[]},
        "status":"pending","phase":null,"steps":{},"error":null,"createdAt":1,"updatedAt":1,
    })).unwrap()
}

fn attempt(id: &str, state: WorkflowStatus) -> WorkflowAttempt {
    serde_json::from_value(
        json!({"id":id,"admission":"accepted","childThreadId":format!("child-{id}"),
        "ownedTurnIds":[format!("owned-{id}")],"finalTurnId":format!("owned-{id}"),
        "resolvedModel":"original-model","reasoningEffort":"high","status":state}),
    )
    .unwrap()
}

fn complete(store: &Store, id: &str) {
    store
        .update(id, |run| {
            run.status = WorkflowStatus::Succeeded;
            run.lifecycle.terminal_ready = true;
            run.result = Some(serde_json::Value::Null);
            run.steps.insert(
                "task".into(),
                WorkflowStep {
                    id: "task".into(),
                    updated_at: None,
                    completed_at: None,
                    metrics: Default::default(),
                    logical_id: "root/review".into(),
                    label: "Original review".into(),
                    phase: Some("Review".into()),
                    worker_sequence: 1,
                    scope: "root".into(),
                    key: None,
                    disposition: crate::WorkflowStepDisposition::Native,
                    control_outcome: None,
                    effective_options: None,
                    options: json!({"message":"Original prompt"}),
                    schema_attempts: 0,
                    attempts: vec![
                        attempt("failed", WorkflowStatus::Failed),
                        attempt("success", WorkflowStatus::Succeeded),
                    ],
                    result: Some(serde_json::Value::Null),
                },
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn all_original_attempts_and_null_results_survive_replacement_and_record_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let root = AbsolutePathBuf::from_absolute_path(directory.path()).unwrap();
    let store = Store::new(root.clone()).unwrap();
    let mut run = fixture();
    store.insert(&mut run).unwrap();
    complete(&store, &run.id);
    let original =
        read_execution_attempt(&root, &run.id, &run.execution_id, "task", "success").unwrap();
    assert_eq!(
        execution_transcript_limitation(&root, &run.id, &run.execution_id).unwrap(),
        None
    );
    let snapshot = read_execution_snapshot(&root, &run.id, &run.execution_id).unwrap();
    let pinned = manifest(&root, &run.id).unwrap();
    assert_eq!(
        original.result,
        WorkflowResult::Value {
            value: serde_json::Value::Null
        }
    );
    assert_eq!(
        read_execution_attempt(&root, &run.id, &run.execution_id, "task", "failed")
            .unwrap()
            .result,
        WorkflowResult::Absent
    );
    let next_execution = uuid::Uuid::new_v4().to_string();
    store
        .update(&run.id, |run| {
            run.execution_id = next_execution.clone();
            run.source =
                "workflow.meta({name:'changed',description:'New script',phases:[]}); return 42;"
                    .into();
            run.input = json!({"new":true});
            run.lifecycle.terminal_ready = false;
            run.status = WorkflowStatus::Running;
            let step = run.steps.get_mut("task").unwrap();
            step.label = "New review".into();
            step.options = json!({"message":"New prompt"});
            step.attempts = vec![attempt("replacement", WorkflowStatus::Succeeded)];
            step.result = Some(json!(42));
            Ok(())
        })
        .unwrap();
    store
        .update(&run.id, |run| {
            run.status = WorkflowStatus::Succeeded;
            run.lifecycle.terminal_ready = true;
            run.result = Some(json!(42));
            Ok(())
        })
        .unwrap();
    for _ in 0..3 {
        store
            .update(&run.id, |run| {
                run.notification_delivered = true;
                Ok(())
            })
            .unwrap();
    }
    drop(store);
    let _reopened = Store::new(root.clone()).unwrap();
    assert_eq!(
        read_execution_attempt(&root, &run.id, &run.execution_id, "task", "success").unwrap(),
        original
    );
    assert_eq!(
        read_execution_snapshot(&root, &run.id, &run.execution_id).unwrap(),
        snapshot
    );
    assert_eq!(
        list_execution_steps(&root, &run.id, &run.execution_id, 0, 1).unwrap(),
        vec![WorkflowExecutionStepIndex {
            step_id: "task".into(),
            logical_id: "root/review".into(),
            label: "Original review".into(),
            phase: Some("Review".into()),
            worker_sequence: 1,
            attempt_ids: vec!["failed".into(), "success".into()],
        }]
    );
    assert!(
        list_execution_steps(&root, &run.id, &run.execution_id, 1, 1)
            .unwrap()
            .is_empty()
    );
    assert!(
        read_execution_attempt(&root, &run.id, &run.execution_id, "task", "replacement").is_err()
    );
    assert!(
        read_execution_attempt(&root, &run.id, &run.execution_id, "other-step", "success").is_err()
    );
    assert!(
        read_execution_attempt(&root, &run.id, "unknown-execution", "task", "success").is_err()
    );
    let current =
        read_execution_attempt(&root, &run.id, &next_execution, "task", "replacement").unwrap();
    assert_eq!(
        (current.label, current.options, current.result),
        (
            "New review".into(),
            json!({"message":"New prompt"}),
            WorkflowResult::Value { value: json!(42) }
        )
    );
    let after = manifest(&root, &run.id).unwrap();
    assert_eq!(
        after.execution_steps[&run.execution_id],
        pinned.execution_steps[&run.execution_id]
    );
    for pin in pinned.execution_pins {
        assert!(
            root.join(&run.id)
                .join("records")
                .join(format!("{pin}.json"))
                .is_file()
        );
    }
    assert!(
        after
            .previous_records
            .iter()
            .all(|record| !after.execution_pins.contains(record))
    );
}

#[test]
fn old_snapshot_fallback_requires_its_exact_retained_attempt_and_original_definition() {
    let directory = tempfile::tempdir().unwrap();
    let root = AbsolutePathBuf::from_absolute_path(directory.path()).unwrap();
    let store = Store::new(root.clone()).unwrap();
    let mut run = fixture();
    store.insert(&mut run).unwrap();
    complete(&store, &run.id);
    let mut legacy_index = manifest(&root, &run.id).unwrap();
    legacy_index.execution_steps.clear();
    legacy_index.execution_pins.clear();
    files::atomic_json(&files::path(&root, &run.id).unwrap(), &legacy_index).unwrap();
    let listed = list_execution_steps(&root, &run.id, &run.execution_id, 0, 50).unwrap();
    assert_eq!(listed[0].attempt_ids, vec!["success"]);
    assert!(
        execution_transcript_limitation(&root, &run.id, &run.execution_id)
            .unwrap()
            .is_some_and(|reason| reason.contains("only its explicitly recorded attempts")),
        "a safe last-attempt fallback must disclose the omitted earlier failed attempt"
    );
    assert_eq!(
        read_execution_attempt(&root, &run.id, &run.execution_id, "task", "success")
            .unwrap()
            .attempt
            .admission,
        AdmissionStatus::Accepted
    );
    assert!(
        read_execution_attempt(&root, &run.id, &run.execution_id, "task", "failed").is_err(),
        "an older attempt absent from the old snapshot cannot be invented from current state"
    );
    drop(store);
    let store = Store::new(root.clone()).unwrap();
    store
        .update(&run.id, |run| {
            run.execution_id = uuid::Uuid::new_v4().to_string();
            run.source.push_str("\n// revised");
            Ok(())
        })
        .unwrap();
    assert!(
        list_execution_steps(&root, &run.id, &run.execution_id, 0, 50)
            .unwrap_err()
            .contains("not pinned")
    );
    assert!(execution_transcript_limitation(&root, &run.id, &run.execution_id).is_err());
}

#[test]
fn current_execution_read_is_partial_without_creating_terminal_history() {
    let directory = tempfile::tempdir().unwrap();
    let root = AbsolutePathBuf::from_absolute_path(directory.path()).unwrap();
    let store = Store::new(root.clone()).unwrap();
    let mut run = fixture();
    store.insert(&mut run).unwrap();
    let path = files::path(&root, &run.id).unwrap();
    let before = std::fs::read(&path).unwrap();
    let snapshot = read_execution_snapshot(&root, &run.id, &run.execution_id).unwrap();
    assert_eq!(
        (
            snapshot.run_id,
            snapshot.execution_id,
            snapshot.status,
            snapshot.partial,
            snapshot.result,
            snapshot.source,
            snapshot.input
        ),
        (
            run.id.clone(),
            run.execution_id.clone(),
            WorkflowStatus::Pending,
            true,
            WorkflowResult::Absent,
            run.source,
            run.input
        )
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert!(
        manifest(&root, &run.id)
            .unwrap()
            .execution_snapshots
            .is_empty()
    );
    assert_eq!(
        execution_transcript_limitation(&root, &run.id, &run.execution_id).unwrap(),
        None
    );
    assert!(read_execution_snapshot(&root, &run.id, "unknown-execution").is_err());
    assert!(execution_transcript_limitation(&root, &run.id, "unknown-execution").is_err());
}
