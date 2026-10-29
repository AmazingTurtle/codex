//! Public workflow snapshots retain current-agent routing and inspectable retry history.
use super::*;
use crate::workflows::WorkflowState;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

fn run(mut steps: Value) -> Result<WorkflowRun, serde_json::Error> {
    for (key, step) in steps.as_object_mut().expect("fixture steps").iter_mut() {
        step["logicalId"] = json!(key);
        if step.get("label").is_none() {
            step["label"] = json!(key);
        }
    }
    serde_json::from_value(json!({
        "id": "run", "ownerThreadId": ThreadId::from_u128(7).to_string(),
        "source": "", "input": {}, "status": "running", "phase": null,
        "phases": [], "steps": steps, "createdAt": 10, "updatedAt": 20,
        "formatVersion":2,"readOnly":false,"metadata":{"name":"audit","description":"Inspect source changes","phases":[]},
        "limits":{"maxAgents":32,"maxTokens":null},"acceptedStarts":0,"phaseProgress":[],"logs":[],
        "metrics":{"usage":null,"usageComplete":false,"agentsUsed":0,"activeDurationMs":null,"wallDurationMs":null}
    }))
}

#[test]
fn phase_headings_keep_the_declared_plan_separate_from_the_stable_workflow_name()
-> Result<(), serde_json::Error> {
    let unphased = project(run(
        json!({"check": {"id": "check", "options": {}, "attempts": []}}),
    )?);
    assert_eq!(
        unphased
            .children
            .iter()
            .map(|child| (child.label.as_str(), child.depth))
            .collect::<Vec<_>>(),
        vec![("check · Discovered", 1)]
    );

    let mut single =
        run(json!({"check": {"id": "check", "phase": "Review", "options": {}, "attempts": []}}))?;
    single.phase = Some("Review".into());
    single.phases = vec!["Review".into()];
    single.metadata.as_mut().unwrap().phases = serde_json::from_value(json!([{"title":"Review"}]))?;
    single.phase_progress = serde_json::from_value(
        json!([{"title":"Review","status":"active","stepsDiscovered":1,"stepsCompleted":0}]),
    )?;
    let single = project(single);
    assert_eq!(
        (
            single.title.as_str(),
            single
                .children
                .iter()
                .map(|child| (child.label.as_str(), child.depth))
                .collect::<Vec<_>>()
        ),
        (
            "audit",
            vec![("Review · Active", 1), ("check · Discovered", 2)]
        )
    );

    let mut multiple = run(json!({
        "first": {"id": "first", "phase": "Steps", "options": {}, "attempts": [{"id":"done","status":"succeeded"}]},
        "last": {"id": "last", "phase": "Review", "options": {}, "attempts": []},
        "unphased": {"id": "unphased", "options": {}, "attempts": []}
    }))?;
    multiple.phase = Some("Review".into());
    multiple.phases = vec!["Steps".into(), "Review".into()];
    multiple.metadata.as_mut().unwrap().phases =
        serde_json::from_value(json!([{"title":"Steps"},{"title":"Review"}]))?;
    multiple.phase_progress = serde_json::from_value(
        json!([{"title":"Steps","status":"completed","stepsDiscovered":1,"stepsCompleted":1},{"title":"Review","status":"active","stepsDiscovered":1,"stepsCompleted":0}]),
    )?;
    let multiple = project(multiple);
    assert_eq!(
        multiple
            .children
            .iter()
            .map(|child| (child.label.as_str(), child.depth))
            .collect::<Vec<_>>(),
        vec![
            ("Steps · Completed", 1),
            ("first · Completed", 2),
            ("Review · Active", 1),
            ("last · Discovered", 2),
            ("unphased · Discovered", 1)
        ]
    );
    Ok(())
}

#[test]
fn latest_step_routes_to_its_child_and_keeps_selection_across_retry_updates()
-> Result<(), serde_json::Error> {
    let old_child = ThreadId::from_u128(41);
    let latest_child = ThreadId::from_u128(42);
    let snapshot = run(
        json!({"check": {"id": "check", "options": {"message": "Inspect source"}, "attempts": [
            {"id": "old", "childThreadId": old_child.to_string(), "status": "failed", "error": "Old failure"},
            {"id": "latest", "childThreadId": null, "status": "running"}
        ]}}),
    )?;
    let row = project(snapshot.clone());
    let mut state = WorkflowState {
        rows: vec![row],
        child: Some(0),
        ..Default::default()
    };
    state.center.show_attempt_history = true;
    assert_eq!(
        (state.selected_node_key(), state.selected_thread()),
        ("run:step:check".into(), None)
    );
    assert_eq!(state.rows[0].children[0].label, "check · Running");

    let mut completed = snapshot;
    let step = completed
        .steps
        .get_mut("check")
        .expect("fixture step exists");
    step.result = Some(json!({"ready": true}));
    let latest = step
        .attempts
        .last_mut()
        .expect("fixture latest attempt exists");
    latest.child_thread_id = Some(latest_child.to_string());
    latest.status = WorkflowStatus::Succeeded;
    latest.resolved_model = Some("resolved-worker-model".into());
    latest.reasoning_effort = Some("low".into());
    state.merge(vec![project(completed)]);
    assert_eq!(
        (
            state.rows[0].children[0].resolved_model.as_deref(),
            state.rows[0].children[0].reasoning_effort.as_deref()
        ),
        (Some("resolved-worker-model"), Some("low"))
    );
    assert_eq!(
        (state.selected_node_key(), state.selected_thread()),
        ("run:step:check".into(), Some(latest_child))
    );
    let overview = state.rows[0].children[0]
        .overview
        .as_ref()
        .expect("step overview exists");
    assert_eq!(
        (
            overview.prompt.as_deref(),
            overview.error.as_deref(),
            overview.result.as_ref()
        ),
        (Some("Inspect source"), None, Some(&json!({"ready": true})))
    );
    Ok(())
}

#[test]
fn failed_latest_attempt_exposes_its_error_without_a_redundant_attempt_node()
-> Result<(), serde_json::Error> {
    let row = project(run(
        json!({"check": {"id": "check", "options": {"prompt": "Inspect source"}, "attempts": [
            {"id": "latest", "childThreadId": null, "status": "failed", "error": "Model unavailable"}
        ]}}),
    )?);
    assert_eq!(
        (
            row.children.len(),
            row.children[0].label.as_str(),
            row.agents_used
        ),
        (1, "check · Failed", Some(0))
    );
    let overview = row.children[0]
        .overview
        .as_ref()
        .expect("step overview exists");
    assert_eq!(
        (
            overview.prompt.as_deref(),
            overview.error.as_deref(),
            overview.result.as_ref()
        ),
        (Some("Inspect source"), Some("Model unavailable"), None)
    );
    Ok(())
}

#[test]
fn previous_attempts_keep_their_own_output_and_return_selection_to_the_step_when_hidden()
-> Result<(), serde_json::Error> {
    let first_child = ThreadId::from_u128(41);
    let second_child = ThreadId::from_u128(42);
    let current_child = ThreadId::from_u128(43);
    let mut snapshot = run(
        json!({"check": {"id": "check", "options": {}, "result": "Current success", "attempts": [
            {"id": "first", "childThreadId": first_child.to_string(), "status": "failed", "error": "First failure", "usage": {"totalTokens": 11}},
            {"id": "second", "childThreadId": second_child.to_string(), "status": "failed", "error": "Second failure", "usage": {"totalTokens": 13}},
            {"id": "current", "childThreadId": current_child.to_string(), "status": "succeeded", "usage": {"totalTokens": 17}}
        ]}}),
    )?;
    snapshot.accepted_starts = 3;
    snapshot.metrics.agents_used = Some(3);
    for (index, attempt) in snapshot
        .steps
        .get_mut("check")
        .unwrap()
        .attempts
        .iter_mut()
        .enumerate()
    {
        attempt.metrics.usage = Some(codex_app_server_protocol::WorkflowTokenUsage {
            input_tokens: [11, 13, 17][index],
            output_tokens: 0,
            cached_input_tokens: 0,
            total_tokens: [11, 13, 17][index],
        });
    }
    let row = project(snapshot);
    assert_eq!(
        row.children
            .iter()
            .map(|child| (
                child.key.as_str(),
                child.label.as_str(),
                child.depth,
                child.parent_step_key.as_deref()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("step:check", "check · Completed", 1, None),
            (
                "attempt:second",
                "check · Attempt 2 · Failed",
                1,
                Some("step:check")
            ),
            (
                "attempt:first",
                "check · Attempt 1 · Failed",
                1,
                Some("step:check")
            )
        ]
    );
    assert_eq!((row.agents_used, row.created_at), (Some(3), 10));
    for (child, error, tokens) in [
        (&row.children[1], "Second failure", 13),
        (&row.children[2], "First failure", 11),
    ] {
        let overview = child.overview.as_ref().expect("history overview exists");
        assert_eq!(
            (overview.error.as_deref(), overview.result.as_ref()),
            (Some(error), None)
        );
        assert_eq!(
            overview
                .metrics
                .as_ref()
                .and_then(|metrics| metrics.usage.as_ref())
                .map(|usage| usage.input_tokens),
            Some(tokens)
        );
    }
    let mut state = WorkflowState {
        rows: vec![row],
        child: Some(0),
        ..Default::default()
    };
    assert_eq!(
        state
            .nodes()
            .iter()
            .map(|node| node.key.as_str())
            .collect::<Vec<_>>(),
        vec!["run:run", "run:step:check"]
    );
    state.center.show_attempt_history = true;
    state.child = Some(1);
    assert_eq!(state.selected_thread(), Some(second_child));
    state.center.show_attempt_history = false;
    state.reconcile_attempt_history_selection();
    assert_eq!(
        (state.selected_node_key(), state.selected_thread()),
        ("run:step:check".into(), Some(current_child))
    );
    Ok(())
}

#[test]
fn agents_used_keeps_the_accepted_start_ledger_across_recovery_and_cold_summaries()
-> Result<(), serde_json::Error> {
    let child = ThreadId::from_u128(41);
    let mut snapshot = run(json!({
        "check": {"id": "check", "options": {}, "attempts": [{"id": "accepted", "childThreadId": child.to_string(), "status": "interrupted"}]},
        "unresolved": {"id": "unresolved", "options": {}, "attempts": [{"id": "missing", "childThreadId": null, "status": "interrupted"}]},
        "invalid": {"id": "invalid", "options": {}, "attempts": [{"id": "invalid", "childThreadId": "invalid-id", "status": "failed"}]}
    }))?;
    snapshot.accepted_starts = 1;
    snapshot.metrics.agents_used = Some(1);
    assert_eq!(project(snapshot.clone()).agents_used, Some(1));
    // Recovery changes the existing attempt's state, retaining its child identity.
    snapshot
        .steps
        .get_mut("check")
        .expect("fixture step exists")
        .attempts[0]
        .status = WorkflowStatus::Running;
    assert_eq!(project(snapshot).agents_used, Some(1));
    let summary = summary(serde_json::from_value(json!({
        "id": "run", "ownerThreadId": ThreadId::from_u128(7).to_string(), "status": "interrupted",
        "phase": null, "phaseCount": 0, "stepCount": 3, "createdAt": 10, "updatedAt": 20,
        "formatVersion":2,"readOnly":false,"metadata":{"name":"audit","description":"Inspect source changes","phases":[]},
        "limits":{"maxAgents":32,"maxTokens":null},"acceptedStarts":1,"phaseProgress":[],
        "metrics":{"usage":null,"usageComplete":false,"agentsUsed":1,"activeDurationMs":null,"wallDurationMs":null}
    }))?);
    assert_eq!(
        (summary.agents_used, summary.created_at, summary.discovered),
        (Some(1), 10, 3)
    );
    Ok(())
}

#[test]
fn agents_used_includes_children_retired_by_a_revision() -> Result<(), serde_json::Error> {
    let mut snapshot = run(json!({}))?;
    snapshot.accepted_starts = 3;
    snapshot.metrics.agents_used = Some(3);
    let mut compact = serde_json::to_value(&snapshot)?;
    compact["phaseCount"] = json!(0);
    compact["stepCount"] = json!(0);
    let compact = serde_json::from_value(compact)?;
    let full = project(snapshot);
    let compact = summary(compact);
    assert_eq!(
        (full.agents_used, compact.agents_used, full.children.len()),
        (Some(3), Some(3), 0)
    );
    Ok(())
}

#[test]
fn skipped_disposition_has_no_fake_attempt_and_keeps_replacement_and_edited_prompt()
-> Result<(), serde_json::Error> {
    let row = project(run(json!({
        "skipped":{"id":"skipped","label":"Skip review","options":{"message":"Original prompt"},"effectiveOptions":{"message":"Edited queued prompt"},"disposition":"skipped","controlOutcome":{"commandId":"skip-command","revisionId":"revision-1","replacement":{"type":"value","value":null}},"attempts":[],"result":null},
        "native":{"id":"native","options":{},"attempts":[{"id":"done","status":"succeeded"}],"result":null}
    }))?);
    let skipped = row
        .children
        .iter()
        .find(|child| child.key == "step:skipped")
        .unwrap();
    let overview = skipped.overview.as_ref().unwrap();
    assert_eq!(
        (
            skipped.label.as_str(),
            skipped.thread_id,
            overview.prompt.as_deref(),
            overview.result_label.as_deref(),
            overview.result.as_ref()
        ),
        (
            "Skip review · Skipped",
            None,
            Some("Edited queued prompt"),
            Some("Replacement"),
            Some(&Value::Null)
        )
    );
    assert_eq!(row.agents_used, Some(0));
    assert!(
        !row.children
            .iter()
            .any(|child| child.key.starts_with("attempt:"))
    );
    assert_eq!(
        row.children
            .iter()
            .find(|child| child.key == "step:native")
            .unwrap()
            .label,
        "native · Completed"
    );
    let state = std::rc::Rc::new(std::sync::Mutex::new(crate::workflows::WorkflowState {
        owner: Some(ThreadId::from_u128(7)),
        child: Some(
            row.children
                .iter()
                .position(|child| child.key == "step:skipped")
                .unwrap(),
        ),
        rows: vec![row],
        ..Default::default()
    }));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let view = crate::workflows::WorkflowCenter::new(
        state,
        crate::app_event_sender::AppEventSender::new(tx),
    );
    let area = ratatui::layout::Rect::new(0, 0, 120, 22);
    let mut buffer = ratatui::buffer::Buffer::empty(area);
    crate::render::renderable::Renderable::render(&view, area, &mut buffer);
    let rendered = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Skipped")
            && rendered.contains("Replacement")
            && rendered.contains("null"),
        "{rendered}"
    );
    insta::assert_snapshot!("workflow_skipped_step_replacement", rendered);
    Ok(())
}
