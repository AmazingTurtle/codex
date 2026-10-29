use super::*;
use crate::AdmissionStatus;
use crate::WorkflowAttempt;
use crate::WorkflowStep;
use pretty_assertions::assert_eq;
use serde_json::json;

fn fixture() -> WorkflowRun {
    serde_json::from_value(json!({
        "id":"9f4d915f-005a-49a3-92cb-7e3b836c18a6","formatVersion":2,
        "ownerThreadId":"owner","source":"workflow.meta({name:'stored',description:'',phases:[]});","input":{"sourceSnapshot":true},
        "status":"pending","phase":null,"steps":{},"error":null,"createdAt":1,"updatedAt":1,
    })).unwrap()
}
fn root(directory: &tempfile::TempDir) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(directory.path()).unwrap()
}
fn usage(input: u64, output: u64, cached: u64) -> WorkflowTokenUsage {
    WorkflowTokenUsage {
        input_tokens: input,
        output_tokens: output,
        cached_input_tokens: cached,
        total_tokens: input + output,
    }
}
fn step() -> WorkflowStep {
    WorkflowStep {
        id: "task".into(),
        updated_at: None,
        completed_at: None,
        metrics: Default::default(),
        scope: String::new(),
        key: None,
        disposition: crate::WorkflowStepDisposition::Native,
        control_outcome: None,
        effective_options: None,
        logical_id: "task".into(),
        label: "Task".into(),
        schema_attempts: 0,
        worker_sequence: 1,
        phase: None,
        options: json!({"message":"immutable agent prompt"}),
        attempts: vec![WorkflowAttempt {
            id: "attempt".into(),
            updated_at: None,
            completed_at: None,
            metrics: Default::default(),
            activity: Default::default(),
            clock_execution_id: None,
            clock_incomplete: false,
            execution_id: String::new(),
            owned_turn_ids: Vec::new(),
            final_turn_id: None,
            resolved_model: None,
            reasoning_effort: None,
            admission: AdmissionStatus::Accepted,
            child_thread_id: Some("child".into()),
            status: WorkflowStatus::Running,
            usage: None,
            error: None,
            schema_error: None,
            validation_feedback: None,
        }],
        result: None,
    }
}

#[test]
fn legacy_running_and_terminal_snapshots_remain_byte_for_byte_read_only() {
    let directory = tempfile::tempdir().unwrap();
    let root = root(&directory);
    let mut legacy = serde_json::to_value(fixture()).unwrap();
    legacy.as_object_mut().unwrap().remove("formatVersion");
    legacy["status"] = json!("running");
    legacy["phases"] = json!(["Historical plan", "Historical result"]);
    let bytes = serde_json::to_vec(&legacy).unwrap();
    let id = legacy["id"].as_str().unwrap().to_string();
    fs::write(files::path(&root, &id).unwrap(), &bytes).unwrap();
    let store = Store::new(root.clone()).unwrap();
    assert_eq!(store.get(&id).unwrap().format_version, 1);
    assert_eq!(
        store.list_summaries("owner", 0, 10).unwrap()[0].phase_count,
        2
    );
    assert!(
        store
            .update(&id, |run| {
                run.status = WorkflowStatus::Stopped;
                Ok(())
            })
            .is_err()
    );
    assert!(store.acknowledge_notification(&id, "revision").is_err());
    assert_eq!(fs::read(files::path(&root, &id).unwrap()).unwrap(), bytes);
    legacy["status"] = json!("succeeded");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(files::path(&root, &id).unwrap(), &bytes).unwrap();
    let reopened = Store::new(root.clone()).unwrap();
    assert_eq!(
        reopened.list_summaries("owner", 0, 10).unwrap()[0].format_version,
        1
    );
    assert_eq!(
        reopened.list_summaries("owner", 0, 10).unwrap()[0].phase_count,
        2
    );
    assert_eq!(fs::read(files::path(&root, &id).unwrap()).unwrap(), bytes);
}

#[test]
fn usage_checkpoints_replace_per_thread_highwater_and_keep_attempt_details_independent() {
    let directory = tempfile::tempdir().unwrap();
    let root = root(&directory);
    let store = Store::new(root.clone()).unwrap();
    let mut run = fixture();
    store.insert(&mut run).unwrap();
    store
        .update(&run.id, |run| {
            run.status = WorkflowStatus::Running;
            run.accepted_starts = 2;
            run.steps.insert("task".into(), step());
            Ok(())
        })
        .unwrap();
    let before: serde_json::Value =
        files::read_json(&files::path(&root, &run.id).unwrap()).unwrap();
    assert_eq!(before["header"]["steps"], json!({}));
    assert_eq!(before["header"]["source"], json!(""));
    store
        .record_usage(&run.id, "attempt", "child", usage(100, 20, 40))
        .unwrap();
    store
        .record_usage(&run.id, "attempt", "child", usage(200, 30, 70))
        .unwrap();
    store
        .record_usage(&run.id, "attempt", "child", usage(100, 20, 40))
        .unwrap();
    store
        .record_usage(&run.id, "attempt", "descendant", usage(50, 10, 10))
        .unwrap();
    store.complete_usage(&run.id, "attempt", "child").unwrap();
    store
        .complete_usage(&run.id, "attempt", "descendant")
        .unwrap();
    store.checkpoint(&run.id).unwrap();
    let observed = store.get(&run.id).unwrap();
    assert_eq!(observed.metrics.usage, Some(usage(250, 40, 80)));
    assert!(!observed.metrics.usage_complete);
    let after: serde_json::Value = files::read_json(&files::path(&root, &run.id).unwrap()).unwrap();
    assert_eq!(after["definition"], before["definition"]);
    assert_ne!(
        after["steps"], before["steps"],
        "typed entity metrics use new COW detail records"
    );
    assert_eq!(observed.steps["task"].options, step().options);
    assert_eq!(
        observed.steps["task"].attempts[0].usage, None,
        "typed contributor rollups do not replace the native terminal usage payload"
    );
    assert_eq!(
        observed.steps["task"].metrics.usage,
        Some(usage(250, 40, 80))
    );
    store
        .update(&run.id, |run| {
            run.status = WorkflowStatus::Succeeded;
            Ok(())
        })
        .unwrap();
    let complete = store.get(&run.id).unwrap();
    assert!(complete.metrics.usage_complete);
    assert_eq!(complete.metrics.usage, Some(usage(250, 40, 80)));
    assert_eq!(Store::new(root).unwrap().get(&run.id).unwrap(), complete);
}

#[test]
fn summary_listing_never_hydrates_source_or_attempt_records() {
    let directory = tempfile::tempdir().unwrap();
    let root = root(&directory);
    let store = Store::new(root.clone()).unwrap();
    let mut run = fixture();
    store.insert(&mut run).unwrap();
    let manifest: files::Manifest =
        files::read_json(&files::path(&root, &run.id).unwrap()).unwrap();
    fs::remove_file(
        root.join(&run.id)
            .join("records")
            .join(format!("{}.json", manifest.definition)),
    )
    .unwrap();
    let rows = store.list_summaries("owner", 0, 1).unwrap();
    assert_eq!(rows[0].id, run.id);
    assert_eq!(rows[0].format_version, 2);
    assert!(files::read(&root, &run.id).is_err());
}

#[test]
fn active_duration_closes_pause_and_recovery_intervals_without_counting_offline_time() {
    let mut run = fixture();
    metrics::set_active(&mut run, true, 1000);
    metrics::set_active(&mut run, false, 4000);
    metrics::set_active(&mut run, true, 9000);
    metrics::checkpoint_clock(&mut run, 11000);
    assert_eq!(run.activity.active_duration_ms, 5000);
    // Recovery closes at the durable checkpoint instead of the later startup clock.
    let checkpoint = run.activity.checkpoint_at_ms.unwrap();
    metrics::set_active(&mut run, false, checkpoint);
    run.status = WorkflowStatus::Interrupted;
    metrics::refresh(&mut run, &BTreeMap::new(), 90000);
    assert_eq!(run.metrics.active_duration_ms, Some(5000));
    let frozen = run.metrics.clone();
    metrics::refresh(&mut run, &BTreeMap::new(), 180000);
    assert_eq!(run.metrics, frozen);
}

#[test]
fn execution_clock_counts_each_revision_highwater_once_and_never_extrapolates_wall_time() {
    let directory = tempfile::tempdir().unwrap();
    let root = root(&directory);
    let store = Store::new(root.clone()).unwrap();
    let mut run = fixture();
    run.execution_id = "first".into();
    store.insert(&mut run).unwrap();
    assert_eq!(store.get(&run.id).unwrap().metrics.active_duration_ms, None);
    store.record_clock(&run.id, "first", 500, 1000).unwrap();
    store.record_clock(&run.id, "first", 300, 999_000).unwrap();
    store.checkpoint(&run.id).unwrap();
    assert_eq!(
        store.get(&run.id).unwrap().metrics.active_duration_ms,
        Some(500)
    );
    store
        .update(&run.id, |run| {
            run.execution_id = "retry".into();
            run.activity.execution_duration_ms = 0;
            Ok(())
        })
        .unwrap();
    store
        .record_clock(&run.id, "first", 50_000, 1_000_000)
        .unwrap();
    store
        .record_clock(&run.id, "retry", 200, 1_000_100)
        .unwrap();
    store
        .record_clock(&run.id, "retry", 200, 1_000_200)
        .unwrap();
    store.checkpoint(&run.id).unwrap();
    assert_eq!(
        store.get(&run.id).unwrap().metrics.active_duration_ms,
        Some(700)
    );
    let reopened = Store::new(root).unwrap().get(&run.id).unwrap();
    assert_eq!(reopened.status, WorkflowStatus::Interrupted);
    assert_eq!(reopened.metrics.active_duration_ms, Some(700));
}

#[test]
fn accounting_checkpoints_use_compact_events_and_keep_only_current_and_previous_records() {
    let directory = tempfile::tempdir().unwrap();
    let root = root(&directory);
    let store = Store::new(root.clone()).unwrap();
    let mut run = fixture();
    store.insert(&mut run).unwrap();
    store
        .update(&run.id, |run| {
            run.status = WorkflowStatus::Running;
            run.accepted_starts = 1;
            run.steps.insert("task".into(), step());
            Ok(())
        })
        .unwrap();
    let mut lifecycle = store.events.subscribe();
    let mut progress = store.progress.subscribe();
    for tokens in 1..=10 {
        store
            .record_usage(&run.id, "attempt", "child", usage(tokens, 2, 0))
            .unwrap();
        store.checkpoint(&run.id).unwrap();
        let mut event = None;
        while let Ok(next) = progress.try_recv() {
            event = Some(next);
        }
        let event = event.expect("checkpoint emits compact progress");
        assert_eq!(event.metrics.usage, Some(usage(tokens, 2, 0)));
        assert_eq!(event.step_count, 1);
        assert!(matches!(
            lifecycle.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }
    let records = root.join(&run.id).join("records");
    // The definition is stable. Typed step and attempt metrics, and the contributor
    // checkpoint, each retain only their current and previous immutable generation.
    assert_eq!(fs::read_dir(&records).unwrap().count(), 7);
    let retained = fs::read_dir(&records)
        .unwrap()
        .map(|entry| {
            files::read_json::<serde_json::Value>(&records.join(entry.unwrap().file_name()))
                .unwrap()
        })
        .collect::<Vec<_>>();
    for entity in ["task", "attempt"] {
        let mut inputs = retained
            .iter()
            .filter(|value| value["id"] == entity)
            .map(|value| value["metrics"]["usage"]["inputTokens"].as_u64().unwrap())
            .collect::<Vec<_>>();
        inputs.sort_unstable();
        assert_eq!(
            inputs,
            vec![9, 10],
            "only the latest two typed {entity} metric generations survive ten checkpoints"
        );
    }
    let orphan = records.join(format!("{}.json", uuid::Uuid::new_v4()));
    fs::write(&orphan, b"unpublished").unwrap();
    let reopened = Store::new(root).unwrap();
    assert!(!orphan.exists());
    assert_eq!(
        reopened.get(&run.id).unwrap().metrics.usage,
        Some(usage(10, 2, 0))
    );
}

#[test]
fn missing_and_corrupt_detail_records_are_errors_without_a_new_manifest_revision() {
    for bytes in [None, Some(b"{broken".as_slice())] {
        let directory = tempfile::tempdir().unwrap();
        let root = root(&directory);
        let store = Store::new(root.clone()).unwrap();
        let mut run = fixture();
        store.insert(&mut run).unwrap();
        let manifest: files::Manifest =
            files::read_json(&files::path(&root, &run.id).unwrap()).unwrap();
        let definition = root
            .join(&run.id)
            .join("records")
            .join(format!("{}.json", manifest.definition));
        match bytes {
            Some(bytes) => fs::write(definition, bytes).unwrap(),
            None => fs::remove_file(definition).unwrap(),
        }
        assert!(files::read(&root, &run.id).is_err());
        let current: files::Manifest =
            files::read_json(&files::path(&root, &run.id).unwrap()).unwrap();
        assert_eq!(current.revision, manifest.revision);
    }
}

#[test]
fn unknown_usage_and_tracking_failure_never_become_complete_zero() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new(root(&directory)).unwrap();
    let mut run = fixture();
    store.insert(&mut run).unwrap();
    store
        .mark_usage_incomplete(&run.id, "Contributor tracking was incomplete".into())
        .unwrap();
    store
        .update(&run.id, |run| {
            run.status = WorkflowStatus::Failed;
            Ok(())
        })
        .unwrap();
    let run = store.get(&run.id).unwrap();
    assert_eq!(run.metrics.usage, None);
    assert!(!run.metrics.usage_complete);
    assert_eq!(
        run.metrics.usage_incomplete_reason.as_deref(),
        Some("Contributor tracking was incomplete")
    );
}

#[test]
fn replay_budget_and_schema_repair_state_survive_independent_record_checkpoints() {
    let directory = tempfile::tempdir().unwrap();
    let root = root(&directory);
    let store = Store::new(root.clone()).unwrap();
    let mut run = fixture();
    run.budget_observations.insert(
        "root/b0".into(),
        crate::BudgetObservation {
            kind: crate::BudgetObservationKind::Spent,
            spent: 0,
            total: Some(100),
        },
    );
    store.insert(&mut run).unwrap();
    let original: files::Manifest =
        files::read_json(&files::path(&root, &run.id).unwrap()).unwrap();
    assert!(original.header.budget_observations.is_empty());
    store
        .update(&run.id, |run| {
            run.budget_observations.insert(
                "root/b1".into(),
                crate::BudgetObservation {
                    kind: crate::BudgetObservationKind::Remaining,
                    spent: 42,
                    total: Some(100),
                },
            );
            let mut step = step();
            step.schema_attempts = 2;
            step.attempts[0].validation_feedback =
                Some("Keep the UTF-8 validation error: 🦀".into());
            run.steps.insert(step.id.clone(), step);
            run.status = WorkflowStatus::Failed;
            Ok(())
        })
        .unwrap();
    let complete = store.get(&run.id).unwrap();
    let current: files::Manifest = files::read_json(&files::path(&root, &run.id).unwrap()).unwrap();
    assert_ne!(current.definition, original.definition);
    assert!(current.header.budget_observations.is_empty());
    assert_eq!(Store::new(root).unwrap().get(&run.id).unwrap(), complete);
    assert_eq!(complete.budget_observations.len(), 2);
    assert_eq!(complete.steps["task"].schema_attempts, 2);
    assert_eq!(
        complete.steps["task"].attempts[0]
            .validation_feedback
            .as_deref(),
        Some("Keep the UTF-8 validation error: 🦀")
    );
}

#[test]
fn delivery_receipts_preserve_public_work_timestamps_metrics_and_settlement_wakeups() {
    let directory = tempfile::tempdir().unwrap();
    let root = root(&directory);
    let store = Store::new(root.clone()).unwrap();
    let mut run = fixture();
    run.lifecycle_version = 1;
    run.lifecycle.enabled = true;
    run.execution_id = uuid::Uuid::new_v4().to_string();
    run.metadata = Some(crate::compile_script("workflow.meta({name:'receipts',description:'Delivery is bookkeeping',phases:[]}); return null;").unwrap().metadata);
    store.insert(&mut run).unwrap();
    store
        .update(&run.id, |run| {
            run.status = WorkflowStatus::Running;
            Ok(())
        })
        .unwrap();
    store
        .update(&run.id, |run| {
            run.status = WorkflowStatus::Succeeded;
            run.result = Some(serde_json::Value::Null);
            run.lifecycle.terminal_ready = true;
            Ok(())
        })
        .unwrap();
    drop(store);
    // Model a cold historical completion without waiting for the wall clock to tick.
    let path = files::path(&root, &run.id).unwrap();
    let mut manifest: serde_json::Value = files::read_json(&path).unwrap();
    manifest["header"]["updatedAt"] = json!(1);
    files::atomic_json(&path, &manifest).unwrap();
    let store = Store::new(root.clone()).unwrap();
    let mut events = store.events.subscribe();
    let mut expected = store.get(&run.id).unwrap();
    assert_eq!(expected.updated_at, 1);
    let event_id = expected.lifecycle.events[0].id.clone();
    store
        .acknowledge_lifecycle_event(&run.id, &event_id)
        .unwrap();
    expected.lifecycle.delivered.insert(event_id.clone());
    assert_eq!(events.try_recv().unwrap(), expected);
    assert_eq!(store.get(&run.id).unwrap(), expected);
    store
        .acknowledge_lifecycle_event(&run.id, &event_id)
        .unwrap();
    assert_eq!(store.get(&run.id).unwrap(), expected);
    store
        .acknowledge_notification(&run.id, &run.execution_id)
        .unwrap();
    expected.notification_delivered = true;
    assert_eq!(events.try_recv().unwrap(), expected);
    assert_eq!(store.get(&run.id).unwrap(), expected);
    drop(store);
    assert_eq!(Store::new(root).unwrap().get(&run.id).unwrap(), expected);
}
