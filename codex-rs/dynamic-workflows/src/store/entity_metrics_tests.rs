use super::*;
use crate::WorkflowAttempt;
use crate::WorkflowStep;
use pretty_assertions::assert_eq;
use serde_json::json;

fn setup() -> (tempfile::TempDir, Store, WorkflowRun) {
    let directory = tempfile::tempdir().unwrap();
    let root = AbsolutePathBuf::from_absolute_path_checked(directory.path()).unwrap();
    let store = Store::new(root).unwrap();
    let mut run: WorkflowRun = serde_json::from_value(json!({
        "id":"ba45e3bc-cde9-484c-85b1-cf9a50714614","formatVersion":2,"executionId":"execution-one",
        "ownerThreadId":"owner","source":"workflow.meta({name:'metrics',description:'Entity metrics',phases:[]}); return null;",
        "input":{},"status":"pending","phase":null,"steps":{},"error":null,"createdAt":1,"updatedAt":1,
        "metrics":{"usage":null,"usageComplete":false,"activeDurationMs":null,"wallDurationMs":null,"modelTurns":0,"agentsUsed":0}
    })).unwrap();
    store.insert(&mut run).unwrap();
    store.update(&run.id, |run| {
        run.status = WorkflowStatus::Running;
        let attempt: WorkflowAttempt = serde_json::from_value(json!({
            "id":"attempt-one","executionId":"execution-one","admission":"reserved","status":"running","childThreadId":null
        })).unwrap();
        let step: WorkflowStep = serde_json::from_value(json!({
            "id":"task","label":"Review","phase":"Review","options":{},"attempts":[attempt]
        })).unwrap();
        run.steps.insert("task".into(), step);
        run.phase_progress = vec![serde_json::from_value(json!({
            "title":"Review","status":"active","stepsDiscovered":1,"stepsCompleted":0
        })).unwrap()];
        crate::orchestration::record_acceptance(run, "attempt-one", "child-one".into())
    }).unwrap();
    (directory, store, run)
}

fn usage(input: u64, output: u64) -> WorkflowTokenUsage {
    WorkflowTokenUsage {
        input_tokens: input,
        output_tokens: output,
        cached_input_tokens: 0,
        total_tokens: input + output,
    }
}

#[test]
fn contributor_counts_replace_highwaters_and_reopen_without_recharging_or_adopting_unknown() {
    let (directory, store, run) = setup();
    store
        .record_usage(&run.id, "attempt-one", "child-one", usage(10, 5))
        .unwrap();
    for count in [Some(2), Some(2), Some(1), None] {
        store
            .record_model_turns(&run.id, "attempt-one", "child-one", count)
            .unwrap();
    }
    store
        .record_usage(&run.id, "attempt-one", "private-review", usage(7, 3))
        .unwrap();
    store
        .record_model_turns(&run.id, "attempt-one", "private-review", Some(3))
        .unwrap();
    let observed = store.get(&run.id).unwrap();
    assert_eq!(observed.metrics.model_turns, Some(5));
    assert_eq!(observed.metrics.agents_used, Some(1));
    assert_eq!(observed.steps["task"].metrics.usage, Some(usage(17, 8)));
    assert_eq!(
        observed.steps["task"].attempts[0].metrics.model_turns,
        Some(5)
    );
    assert_eq!(observed.phase_progress[0].metrics.model_turns, Some(5));
    assert_eq!(observed.phase_progress[0].metrics.agents_used, Some(1));
    store
        .update(&run.id, |run| {
            run.status = WorkflowStatus::Succeeded;
            run.steps.get_mut("task").unwrap().result = Some(json!(null));
            run.steps.get_mut("task").unwrap().attempts[0].status = WorkflowStatus::Succeeded;
            Ok(())
        })
        .unwrap();
    let terminal = store.get(&run.id).unwrap();
    let root = AbsolutePathBuf::from_absolute_path_checked(directory.path()).unwrap();
    let reopened = Store::new(root).unwrap();
    assert_eq!(reopened.get(&run.id).unwrap(), terminal);
    reopened
        .record_model_turns(&run.id, "attempt-one", "child-one", None)
        .unwrap();
    assert_eq!(
        reopened.get(&run.id).unwrap(),
        terminal,
        "cold unknown cannot erase a durable trusted count"
    );
    reopened
        .mark_model_turns_incomplete(&run.id, "attempt-one", "child-one")
        .unwrap();
    let partial = reopened.get(&run.id).unwrap();
    assert_eq!(partial.metrics.model_turns, None);
    assert_eq!(partial.steps["task"].metrics.model_turns, None);
    assert_eq!(
        partial.steps["task"].completed_at,
        terminal.steps["task"].completed_at
    );
    assert_eq!(
        partial.steps["task"].updated_at,
        terminal.steps["task"].updated_at
    );
}

#[test]
fn late_interruption_reason_keeps_entity_coverage_incomplete_after_contributors_settle() {
    let (directory, store, run) = setup();
    store
        .record_usage(&run.id, "attempt-one", "child-one", usage(10, 5))
        .unwrap();
    store
        .record_model_turns(&run.id, "attempt-one", "child-one", Some(2))
        .unwrap();
    store
        .update(&run.id, |run| {
            let step = run.steps.get_mut("task").unwrap();
            step.result = Some(json!(null));
            step.attempts[0].status = WorkflowStatus::Succeeded;
            Ok(())
        })
        .unwrap();
    let original = store.get(&run.id).unwrap().steps["task"].attempts[0].clone();
    assert!(!original.metrics.usage_complete);
    store
        .mark_usage_incomplete(
            &run.id,
            "owned descendant was interrupted before its authoritative final usage".into(),
        )
        .unwrap();
    store
        .complete_usage(&run.id, "attempt-one", "child-one")
        .unwrap();
    store
        .update(&run.id, |run| {
            run.status = WorkflowStatus::Succeeded;
            Ok(())
        })
        .unwrap();
    let complete = store.get(&run.id).unwrap();
    let attempt = &complete.steps["task"].attempts[0];
    assert!(!complete.metrics.usage_complete);
    assert!(!attempt.metrics.usage_complete);
    assert!(!complete.steps["task"].metrics.usage_complete);
    assert!(!complete.phase_progress[0].metrics.usage_complete);
    let mut expected = original;
    expected.metrics.usage_incomplete_reason = complete.metrics.usage_incomplete_reason.clone();
    assert_eq!(
        attempt, &expected,
        "later settlement adds the explicit coverage reason without rewriting owned facts, counters or timestamps"
    );
    let root = AbsolutePathBuf::from_absolute_path_checked(directory.path()).unwrap();
    assert_eq!(Store::new(root).unwrap().get(&run.id).unwrap(), complete);
}

#[test]
fn coverage_gaps_do_not_revoke_certified_siblings_or_contaminate_later_independent_work() {
    let (directory, store, run) = setup();
    store
        .record_usage(&run.id, "attempt-one", "child-one", usage(10, 5))
        .unwrap();
    store
        .record_model_turns(&run.id, "attempt-one", "child-one", Some(1))
        .unwrap();
    store
        .complete_usage(&run.id, "attempt-one", "child-one")
        .unwrap();
    store
        .update(&run.id, |run| {
            let step = run.steps.get_mut("task").unwrap();
            step.result = Some(json!("certified result"));
            step.attempts[0].status = WorkflowStatus::Succeeded;
            Ok(())
        })
        .unwrap();
    let certified = store.get(&run.id).unwrap().steps["task"].clone();
    assert!(certified.metrics.usage_complete);
    for (name, pending_descendant) in [
        ("pending", true),
        ("independent", false),
        ("later-gap", true),
    ] {
        let attempt_id = format!("attempt-{name}");
        let child = format!("child-{name}");
        store.update(&run.id, |run| {
            let mut step: WorkflowStep = serde_json::from_value(json!({
                "id":name,"label":name,"phase":name,"options":{},"attempts":[{
                    "id":attempt_id,"executionId":"execution-one","admission":"reserved","status":"running","childThreadId":null
                }]
            })).unwrap();
            step.logical_id = name.into();
            run.steps.insert(name.into(), step);
            run.phase_progress.push(serde_json::from_value(json!({
                "title":name,"status":"active","stepsDiscovered":1,"stepsCompleted":0
            })).unwrap());
            crate::orchestration::record_acceptance(run, &attempt_id, child.clone())
        }).unwrap();
        store
            .record_usage(&run.id, &attempt_id, &child, usage(7, 3))
            .unwrap();
        store
            .record_model_turns(&run.id, &attempt_id, &child, Some(1))
            .unwrap();
        store.complete_usage(&run.id, &attempt_id, &child).unwrap();
        if pending_descendant {
            store
                .record_usage(
                    &run.id,
                    &attempt_id,
                    &format!("private-{name}"),
                    usage(2, 1),
                )
                .unwrap();
        }
        store
            .update(&run.id, |run| {
                let step = run.steps.get_mut(name).unwrap();
                step.result = Some(json!(name));
                step.attempts[0].status = WorkflowStatus::Succeeded;
                Ok(())
            })
            .unwrap();
        if pending_descendant {
            store
                .mark_usage_incomplete(&run.id, "owned descendant interrupted".into())
                .unwrap();
            store
                .complete_usage(&run.id, &attempt_id, &format!("private-{name}"))
                .unwrap();
        }
        let current = store.get(&run.id).unwrap();
        assert_eq!(
            current.steps["task"], certified,
            "another attempt cannot rewrite the completed certificate"
        );
        assert_eq!(
            current.steps[name].metrics.usage_complete,
            !pending_descendant
        );
        assert_eq!(
            current.steps[name].attempts[0]
                .metrics
                .usage_incomplete_reason
                .is_some(),
            pending_descendant
        );
    }
    store
        .update(&run.id, |run| {
            run.status = WorkflowStatus::Succeeded;
            Ok(())
        })
        .unwrap();
    let complete = store.get(&run.id).unwrap();
    assert!(
        !complete.metrics.usage_complete,
        "run totals retain the earlier uncertainty"
    );
    assert!(complete.steps["independent"].metrics.usage_complete);
    assert!(
        complete
            .phase_progress
            .iter()
            .find(|phase| phase.title == "Review")
            .unwrap()
            .metrics
            .usage_complete
    );
    assert!(
        complete
            .phase_progress
            .iter()
            .find(|phase| phase.title == "independent")
            .unwrap()
            .metrics
            .usage_complete
    );
    assert!(
        !complete.steps["later-gap"].metrics.usage_complete,
        "an existing global reason must not suppress new attempt-scoped evidence"
    );
    let root = AbsolutePathBuf::from_absolute_path_checked(directory.path()).unwrap();
    assert_eq!(Store::new(root).unwrap().get(&run.id).unwrap(), complete);
}

#[test]
fn late_unknown_contributor_invalidates_terminal_total_and_survives_reopen_without_another_event() {
    let (directory, store, run) = setup();
    store
        .record_usage(&run.id, "attempt-one", "child-one", usage(10, 5))
        .unwrap();
    store
        .record_model_turns(&run.id, "attempt-one", "child-one", Some(2))
        .unwrap();
    store
        .complete_usage(&run.id, "attempt-one", "child-one")
        .unwrap();
    store
        .update(&run.id, |run| {
            run.status = WorkflowStatus::Succeeded;
            let step = run.steps.get_mut("task").unwrap();
            step.result = Some(json!(null));
            step.attempts[0].status = WorkflowStatus::Succeeded;
            Ok(())
        })
        .unwrap();
    let terminal = store.get(&run.id).unwrap();
    assert_eq!(terminal.metrics.model_turns, Some(2));
    store
        .record_model_turns(&run.id, "attempt-one", "late-private", None)
        .unwrap();
    let unknown = store.get(&run.id).unwrap();
    assert_eq!(unknown.metrics.model_turns, None);
    assert_eq!(unknown.steps["task"].metrics.model_turns, None);
    assert_eq!(
        unknown.steps["task"].completed_at,
        terminal.steps["task"].completed_at
    );
    assert_eq!(
        unknown.steps["task"].updated_at,
        terminal.steps["task"].updated_at
    );
    let root = AbsolutePathBuf::from_absolute_path_checked(directory.path()).unwrap();
    let durable = files::read(&root, &run.id).unwrap();
    assert_eq!(
        durable.usage["child-one"].model_turns,
        Some(2),
        "the proven contributor highwater remains intact"
    );
    assert_eq!(durable.usage["late-private"].model_turns, None);
    assert_eq!(Store::new(root).unwrap().get(&run.id).unwrap(), unknown);
}

#[test]
fn attempt_clock_adds_each_execution_highwater_once_and_freezes_terminal_entity_time() {
    let (_directory, store, run) = setup();
    for elapsed in [1200, 1200, 700] {
        store
            .record_attempt_clock(&run.id, "execution-one", "attempt-one", elapsed, 100_000)
            .unwrap();
    }
    store
        .update(&run.id, |run| {
            run.execution_id = "execution-two".into();
            Ok(())
        })
        .unwrap();
    store
        .record_attempt_clock(&run.id, "execution-one", "attempt-one", 50_000, 200_000)
        .unwrap();
    store
        .record_attempt_clock(&run.id, "execution-two", "attempt-one", 300, 500_000)
        .unwrap();
    assert_eq!(
        store.get(&run.id).unwrap().steps["task"]
            .metrics
            .active_duration_ms,
        Some(1500)
    );
    store
        .update(&run.id, |run| {
            run.steps.get_mut("task").unwrap().attempts[0].status = WorkflowStatus::Succeeded;
            run.steps.get_mut("task").unwrap().result = Some(json!(null));
            crate::orchestration::refresh_phases(run);
            Ok(())
        })
        .unwrap();
    let before = store.get(&run.id).unwrap().steps["task"].clone();
    assert_eq!(before.completed_at, before.updated_at);
    store
        .record_attempt_clock(&run.id, "execution-two", "attempt-one", 60_000, 900_000)
        .unwrap();
    assert_eq!(store.get(&run.id).unwrap().steps["task"], before);
    assert_eq!(
        store.get(&run.id).unwrap().phase_progress[0]
            .metrics
            .active_duration_ms,
        None
    );
}

#[test]
fn old_terminal_entities_keep_unknown_time_and_count_when_only_other_state_changes() {
    let (_directory, store, run) = setup();
    store
        .update(&run.id, |run| {
            let step = run.steps.get_mut("task").unwrap();
            step.result = Some(json!(null));
            step.attempts[0].status = WorkflowStatus::Succeeded;
            Ok(())
        })
        .unwrap();
    let mut historical = store.get(&run.id).unwrap();
    let step = historical.steps.get_mut("task").unwrap();
    step.updated_at = None;
    step.completed_at = None;
    step.attempts[0].updated_at = None;
    step.attempts[0].completed_at = None;
    let previous = historical.clone();
    historical.logs.push(crate::WorkflowLog {
        sequence: 1,
        message: "unrelated later activity".into(),
        phase: None,
        created_at: 500,
    });
    entity_metrics::stamp(
        &previous,
        &mut historical,
        500,
        entity_metrics::ChangeSource::Transition,
    );
    assert_eq!(historical.steps, previous.steps);
    assert_eq!(historical.steps["task"].metrics.model_turns, None);
    assert_eq!(historical.steps["task"].metrics.active_duration_ms, None);
    let mut old_active = previous;
    let step = old_active.steps.get_mut("task").unwrap();
    step.result = None;
    step.attempts[0].status = WorkflowStatus::Running;
    step.metrics = Default::default();
    step.attempts[0].metrics = Default::default();
    old_active.phase_progress[0].updated_at = None;
    let mut hydrated = old_active.clone();
    entity_metrics::refresh(&mut hydrated, &BTreeMap::new());
    entity_metrics::stamp(
        &old_active,
        &mut hydrated,
        700,
        entity_metrics::ChangeSource::Transition,
    );
    assert_eq!(
        hydrated.steps["task"].updated_at, None,
        "initializing new projections is not historical work"
    );
    assert_eq!(hydrated.steps["task"].attempts[0].updated_at, None);
    assert_eq!(hydrated.phase_progress[0].updated_at, None);
}

#[test]
fn current_phase_members_reset_without_dropping_cumulative_run_contributors_or_workers() {
    let (_directory, store, run) = setup();
    store
        .record_usage(&run.id, "attempt-one", "child-one", usage(10, 5))
        .unwrap();
    store
        .record_model_turns(&run.id, "attempt-one", "child-one", Some(2))
        .unwrap();
    store
        .update(&run.id, |run| {
            // A revision keeps past observations and worker identities while replacing its plan members.
            run.steps.clear();
            run.phase_progress[0].status = crate::PhaseStatus::Pending;
            run.phase_progress[0].steps_discovered = 0;
            run.phase_progress[0].steps_completed = 0;
            Ok(())
        })
        .unwrap();
    let revised = store.get(&run.id).unwrap();
    assert_eq!(revised.metrics.usage, Some(usage(10, 5)));
    assert_eq!(revised.metrics.model_turns, Some(2));
    assert_eq!(revised.metrics.agents_used, Some(1));
    assert_eq!(revised.phase_progress[0].metrics.model_turns, Some(0));
    assert_eq!(revised.phase_progress[0].metrics.agents_used, Some(0));
    assert_eq!(revised.phase_progress[0].metrics.active_duration_ms, None);
}

#[test]
fn native_settled_stop_stamps_completion_but_lost_vm_never_claims_native_settlement() {
    let (_directory, store, run) = setup();
    let initial = store.get(&run.id).unwrap();
    let mut lost = initial.clone();
    lost.status = WorkflowStatus::Interrupted;
    lost.lifecycle.terminal_ready = true;
    lost.lifecycle.interrupted_unsettled = true;
    entity_metrics::stamp(
        &initial,
        &mut lost,
        500,
        entity_metrics::ChangeSource::Transition,
    );
    assert_eq!(lost.steps["task"].attempts[0].completed_at, None);
    assert_eq!(lost.steps["task"].completed_at, None);
    let mut stopped = initial.clone();
    stopped.status = WorkflowStatus::Stopped;
    stopped.lifecycle.terminal_ready = true;
    entity_metrics::stamp(
        &initial,
        &mut stopped,
        600,
        entity_metrics::ChangeSource::Transition,
    );
    assert_eq!(stopped.steps["task"].attempts[0].completed_at, Some(600));
    assert_eq!(stopped.steps["task"].completed_at, Some(600));
    assert_eq!(stopped.phase_progress[0].completed_at, Some(600));
    assert_eq!(
        stopped.steps["task"].attempts[0].status,
        WorkflowStatus::Running,
        "settled cancellation is not a fabricated successful provider final"
    );
}
