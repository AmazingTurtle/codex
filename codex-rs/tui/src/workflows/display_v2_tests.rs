//! Canonical snapshot transitions and legacy inspection use the actual center/strip renderers.
use super::*;
use codex_app_server_protocol::WorkflowPhaseStatus;
use codex_app_server_protocol::WorkflowRun;
use codex_app_server_protocol::WorkflowStatus;
use pretty_assertions::assert_eq;
use serde_json::json;

fn canonical_run() -> WorkflowRun {
    serde_json::from_value(json!({
        "id":"audit-run","executionId":"execution-1","ownerThreadId":ThreadId::from_u128(7).to_string(),
        "formatVersion":2,"readOnly":false,
        "metadata":{"name":"audit","description":"Audit the project before release","phases":[{"title":"Inspect"},{"title":"Verify"}]},
        "source":"workflow.meta({name:'audit',description:'Audit the project before release',phases:[{title:'Inspect'},{title:'Verify'}]});\nworkflow.phase('Inspect');",
        "input":{"target":"src"},"status":"pending","phase":null,"phases":[],"steps":{},
        "limits":{"maxAgents":4,"maxTokens":null},"acceptedStarts":0,
        "phaseProgress":[{"title":"Inspect","status":"pending","stepsDiscovered":0,"stepsCompleted":0},{"title":"Verify","status":"pending","stepsDiscovered":0,"stepsCompleted":0}],
        "metrics":{"usage":null,"usageComplete":false,"activeDurationMs":0,"wallDurationMs":0},
        "logs":[{"sequence":1,"message":"Audit accepted","phase":null,"createdAt":100}],
        "result":null,"error":null,"createdAt":100,"updatedAt":100
    })).unwrap()
}

#[test]
fn canonical_plan_is_visible_before_execution_and_keeps_its_name_during_phase_updates() {
    let initial = canonical_run();
    let state = Rc::new(Mutex::new(WorkflowState {
        owner: Some(ThreadId::from_u128(7)),
        rows: vec![project(initial.clone())],
        ..Default::default()
    }));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    let planned = render(&center, 140, 26);
    assert!(
        planned.contains("Inspect · Planned")
            && planned.contains("Verify · Planned")
            && planned.contains("Audit accepted"),
        "{planned}"
    );
    insta::assert_snapshot!("workflow_declared_plan_before_execution", planned);
    {
        let mut state = state.lock().unwrap();
        state.child = Some(1);
        assert_eq!(state.rows[0].children[1].label, "Verify · Planned");
    }
    let mut running = initial;
    running.status = WorkflowStatus::Running;
    running.phase = Some("Inspect".into());
    running.phases = vec!["Inspect".into()];
    running.phase_progress[0].status = WorkflowPhaseStatus::Active;
    running.phase_progress[0].steps_discovered = 1;
    running.accepted_starts = 2;
    running.steps = serde_json::from_value(json!({"opaque-step":{
        "id":"opaque-step","logicalId":"review","label":"Review source","phase":"Inspect","options":{"prompt":"Review source"},
        "attempts":[{"id":"failed","admission":"accepted","childThreadId":ThreadId::from_u128(41).to_string(),"status":"failed","error":"First review failed","usage":{"input_tokens":12000,"output_tokens":1000,"cached_input_tokens":0,"total_tokens":13000}},
                    {"id":"latest","admission":"accepted","childThreadId":ThreadId::from_u128(42).to_string(),"status":"running","usage":{"input_tokens":300000,"output_tokens":85000,"cached_input_tokens":12000,"total_tokens":385000}}]
    }})).unwrap();
    running.metrics = serde_json::from_value(json!({"usage":{"inputTokens":312000,"outputTokens":86000,"cachedInputTokens":12000,"totalTokens":398000},"usageComplete":false,"activeDurationMs":1737000,"wallDurationMs":2000000})).unwrap();
    state.lock().unwrap().apply_update(project(running.clone()));
    {
        let state = state.lock().unwrap();
        assert_eq!(
            (state.rows[0].title.as_str(), state.selected_node_key()),
            ("audit", "audit-run:phase:Some(\"Verify\")".into())
        );
        assert_eq!(state.rows[0].children[1].label, "Review source · Running");
        assert!(
            !state
                .nodes()
                .iter()
                .any(|node| node.label.contains("opaque-step"))
        );
    }
    state.lock().unwrap().child = None;
    let active = render(&center, 140, 26);
    assert!(
        active.contains("Tokens: ≥312.000 in, ≥86.000 out")
            && active.contains("28m 57s")
            && !active.contains("First review failed"),
        "{active}"
    );
    insta::assert_snapshot!("workflow_active_plan_usage_and_log", active);
    running.status = WorkflowStatus::Succeeded;
    let step = running.steps.get_mut("opaque-step").unwrap();
    step.attempts.last_mut().unwrap().status = WorkflowStatus::Succeeded;
    step.result = Some(json!({"review":"Ready for release"}));
    running.phase_progress[0].status = WorkflowPhaseStatus::Completed;
    running.phase_progress[0].steps_completed = 1;
    running.phase_progress[1].status = WorkflowPhaseStatus::Skipped;
    running.metrics.usage_complete = true;
    running.updated_at = 9000000;
    state.lock().unwrap().apply_update(project(running));
    let strip = WorkflowStrip(state);
    let finished = render(&strip, 140, 1);
    assert!(
        finished.contains("audit")
            && finished.contains("2/2 phases")
            && finished.contains("in 312K · out 86K")
            && finished.contains("28m 57s"),
        "{finished}"
    );
    assert!(render(&center, 140, 26).contains("Verify · Skipped"));
    insta::assert_snapshot!("workflow_completed_phase_metrics", finished);
}

#[test]
fn legacy_run_rejects_mutations_but_keeps_source_transcript_and_agent_chat() {
    let mut legacy = canonical_run();
    legacy.read_only = true;
    assert!(
        project(legacy.clone())
            .overview
            .unwrap()
            .metadata
            .iter()
            .any(|(label, value)| label == "Agent starts" && value == "0/4"),
        "V2 budgets remain authoritative when inspection is read-only"
    );
    legacy.metadata = None;
    legacy.format_version = 1;
    legacy.metrics = Default::default();
    legacy.phase_progress.clear();
    legacy.phases = vec!["Inspect".into()];
    legacy.source = "workflow.phase('Inspect');".into();
    legacy.steps = serde_json::from_value(json!({"review":{"id":"review","logicalId":"review","label":"Review source","phase":"Inspect","options":{},"attempts":[{"id":"latest","childThreadId":ThreadId::from_u128(42).to_string(),"status":"succeeded"}]}})).unwrap();
    let state = Rc::new(Mutex::new(WorkflowState {
        owner: Some(ThreadId::from_u128(7)),
        rows: vec![project(legacy)],
        ..Default::default()
    }));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state, AppEventSender::new(tx));
    for key in ['s', 'r', 'c', 'w'] {
        center.handle_key_event(KeyCode::Char(key).into());
    }
    assert!(
        rx.try_recv().is_err(),
        "legacy controls cannot schedule a mutation"
    );
    let readonly = render(&center, 140, 22);
    assert!(
        readonly.contains("[legacy]") && readonly.contains("read-only"),
        "{readonly}"
    );
    assert!(
        !readonly.contains("Agent starts") && !readonly.contains("Active time"),
        "legacy accounting is unavailable: {readonly}"
    );
    assert_eq!(
        readonly.matches("Legacy workflow is read-only.").count(),
        1,
        "the blocked-action reason is already visible"
    );
    insta::assert_snapshot!("workflow_legacy_read_only", readonly);
    center.handle_key_event(KeyCode::Char('3').into());
    assert!(render(&center, 140, 22).contains("workflow.phase"));
    center.handle_key_event(KeyCode::Char('2').into());
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::ReadTranscript { target, .. }) if target.run_id == "audit-run" && target.target == codex_app_server_protocol::WorkflowTranscriptTarget::Run)
    );
    center.handle_key_event(KeyCode::Down.into());
    center.handle_key_event(KeyCode::Down.into());
    center.handle_key_event(KeyCode::Char('o').into());
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|event| matches!(event, AppEvent::Workflow(WorkflowEvent::SelectAgent(id)) if id == ThreadId::from_u128(42))));
}

#[test]
fn unsuccessful_v2_runs_keep_stop_available_for_explicit_cleanup_retries() {
    for status in [
        WorkflowStatus::Failed,
        WorkflowStatus::Stopped,
        WorkflowStatus::Interrupted,
    ] {
        let mut run = canonical_run();
        run.status = status;
        run.error = Some("An owned process still requires cleanup.".into());
        let state = Rc::new(Mutex::new(WorkflowState {
            owner: Some(ThreadId::from_u128(7)),
            rows: vec![project(run)],
            ..Default::default()
        }));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut center = WorkflowCenter::new(state, AppEventSender::new(tx));
        center.handle_key_event(KeyCode::Char('s').into());
        let AppEvent::Workflow(WorkflowEvent::Action {
            owner,
            id,
            action: WorkflowAction::Stop,
        }) = rx.try_recv().unwrap()
        else {
            panic!("Stop must remain available for cleanup retries: {status:?}");
        };
        assert_eq!((owner, id), (ThreadId::from_u128(7), "audit-run".into()));
    }
    let mut completed = canonical_run();
    completed.status = WorkflowStatus::Succeeded;
    for phase in &mut completed.phase_progress {
        phase.status = WorkflowPhaseStatus::Skipped;
    }
    let state = Rc::new(Mutex::new(WorkflowState {
        owner: Some(ThreadId::from_u128(7)),
        rows: vec![project(completed)],
        ..Default::default()
    }));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state, AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Char('s').into());
    assert!(
        rx.try_recv().is_err(),
        "completed runs have no work to stop"
    );
    let screen = render(&center, 140, 22);
    assert!(screen.contains("No work remains to stop"), "{screen}");
    insta::assert_snapshot!("workflow_completed_stop_unavailable", screen);
}

#[test]
fn legacy_saved_workflow_enter_shows_why_launch_is_disabled() {
    let state = fixture();
    {
        let mut state = state.lock().unwrap();
        state.saved[0].read_only = true;
        state.saved[0].metadata = None;
        state.saved[0].read_only_reason = Some("Legacy saved workflow is read-only.".into());
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state, AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Char('l').into());
    render(&center, 42, 14);
    center.handle_key_event(KeyCode::Enter.into());
    assert!(rx.try_recv().is_err());
    assert!(
        !center.handle_paste("{}".into()),
        "a read-only item does not open the input editor"
    );
    let screen = render(&center, 42, 14);
    assert!(screen.contains("read-only"), "{screen}");
    insta::assert_snapshot!("workflow_legacy_saved_launch_disabled", screen);
}

#[test]
fn saved_source_is_explicit_scrollable_and_preserves_run_and_library_selections() {
    let state = fixture();
    {
        let mut state = state.lock().unwrap();
        state.saved[0].read_only = true;
        state.saved[0].metadata = None;
        state.saved.push(SavedWorkflowRow {
            id: "other-source".into(),
            name: "Another saved script".into(),
            ..Default::default()
        });
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Down.into());
    center.handle_key_event(KeyCode::Char('l').into());
    assert!(
        rx.try_recv().is_err(),
        "opening the library never loads script source"
    );
    render(&center, 42, 14);
    center.handle_key_event(KeyCode::Char('3').into());
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::ReadSavedSource { id, refresh:false }) if id == "saved-review")
    );
    insta::assert_snapshot!(
        "workflow_saved_source_loading_narrow",
        render(&center, 42, 14)
    );
    state.lock().unwrap().saved_sources.insert(
        "saved-review".into(),
        SavedWorkflowSource {
            error: Some("Saved script is unavailable.".into()),
            ..Default::default()
        },
    );
    insta::assert_snapshot!(
        "workflow_saved_source_error_narrow",
        render(&center, 42, 14)
    );
    center.handle_key_event(KeyCode::Char('R').into());
    assert!(matches!(
        rx.try_recv().unwrap(),
        AppEvent::Workflow(WorkflowEvent::Refresh)
    ));
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::ReadSavedSource { id, refresh:true }) if id == "saved-review")
    );
    assert!(matches!(
        rx.try_recv().unwrap(),
        AppEvent::Workflow(WorkflowEvent::RefreshSavedCatalog)
    ));
    state.lock().unwrap().saved_sources.insert(
        "saved-review".into(),
        SavedWorkflowSource {
            source: Some(format!(
                "// Inspect this legacy script\n{}\nreturn 42;",
                (0..60)
                    .map(|index| format!("// source line {index}\n"))
                    .collect::<String>()
            )),
            ..Default::default()
        },
    );
    let source = render(&center, 120, 18);
    assert!(source.contains("Inspect this legacy script"), "{source}");
    insta::assert_snapshot!("workflow_saved_legacy_source", source);
    center.handle_key_event(KeyCode::PageDown.into());
    let details = |screen: String| {
        screen
            .lines()
            .filter_map(|line| line.split_once('│').map(|(_, detail)| detail.to_owned()))
            .collect::<Vec<_>>()
    };
    let bottom = details(render(&center, 120, 18));
    assert!(
        bottom.iter().any(|line| line.contains("return 42")),
        "{bottom:?}"
    );
    center.handle_key_event(KeyCode::Down.into());
    assert!(render(&center, 120, 18).contains("Loading saved source"));
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::ReadSavedSource { id, refresh:false }) if id == "other-source")
    );
    center.handle_key_event(KeyCode::Up.into());
    assert_eq!(details(render(&center, 120, 18)), bottom);
    assert!(rx.try_recv().is_err(), "the original script remains cached");
    center.handle_key_event(KeyCode::Char('2').into());
    let unavailable = render(&center, 120, 18);
    assert!(unavailable.contains("Saved scripts have no transcript"));
    insta::assert_snapshot!("workflow_saved_transcript_unavailable", unavailable);
    assert!(
        rx.try_recv().is_err(),
        "saved scripts cannot request a fabricated agent transcript"
    );
    center.handle_key_event(KeyCode::Char('3').into());
    center.handle_key_event(KeyCode::Char('R').into());
    assert!(matches!(
        rx.try_recv().unwrap(),
        AppEvent::Workflow(WorkflowEvent::Refresh)
    ));
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::ReadSavedSource { id, refresh:true }) if id == "saved-review")
    );
    assert!(matches!(
        rx.try_recv().unwrap(),
        AppEvent::Workflow(WorkflowEvent::RefreshSavedCatalog)
    ));
    render(&center, 42, 14);
    center.handle_key_event(KeyCode::Char('1').into());
    center.handle_key_event(KeyCode::Esc.into());
    assert!(
        state.lock().unwrap().center.saved,
        "narrow Back first reveals the library list"
    );
    center.handle_key_event(KeyCode::Esc.into());
    let state = state.lock().unwrap();
    assert!(!state.center.saved);
    assert_eq!(
        (state.selected_node_key(), state.center.detail_tab),
        ("workflow-1:step:review".into(), DetailTab::Overview)
    );
}

#[test]
fn lightweight_progress_preserves_details_and_rejects_stale_executions_and_resurrection() {
    let mut run = canonical_run();
    run.status = WorkflowStatus::Running;
    run.metrics.active_duration_ms = Some(1000);
    let original = project(run.clone());
    let mut state = WorkflowState {
        owner: Some(ThreadId::from_u128(7)),
        rows: vec![original.clone()],
        child: Some(1),
        loading: true,
        ..Default::default()
    };
    let mut value = serde_json::to_value(&run).unwrap();
    value["stepCount"] = json!(0);
    value["phaseCount"] = json!(2);
    let mut progress: codex_app_server_protocol::WorkflowSummary =
        serde_json::from_value(value).unwrap();
    progress.updated_at = 101;
    progress.metrics.active_duration_ms = Some(2000);
    progress.metrics.usage = Some(codex_app_server_protocol::WorkflowTokenUsage {
        input_tokens: 12000,
        output_tokens: 2000,
        cached_input_tokens: 1000,
        total_tokens: 14000,
    });
    progress.metrics.usage_complete = false;
    state.apply_progress(progress.clone());
    assert_eq!(
        (state.rows[0].metrics.as_ref(), state.selected_node_key()),
        (
            Some(&progress.metrics),
            "audit-run:phase:Some(\"Verify\")".into()
        )
    );
    assert_eq!(
        (&state.rows[0].source, &state.rows[0].logs),
        (&original.source, &original.logs)
    );
    assert_eq!(
        state.rows[0]
            .children
            .iter()
            .map(|child| (&child.key, &child.label))
            .collect::<Vec<_>>(),
        original
            .children
            .iter()
            .map(|child| (&child.key, &child.label))
            .collect::<Vec<_>>()
    );
    state.loading = false;
    let mut newer_details = original;
    newer_details.phase = Some("Inspect".into());
    newer_details.phase_progress[0].status = WorkflowPhaseStatus::Active;
    newer_details.children[0].label = "Inspect · Active".into();
    state.merge(vec![newer_details]);
    assert_eq!(
        state.rows[0].metrics.as_ref(),
        Some(&progress.metrics),
        "an older list reply cannot erase accepted progress"
    );
    assert_eq!(
        (
            state.rows[0].phase.as_deref(),
            state.rows[0].phase_progress[0].status,
            state.rows[0].children[0].label.as_str()
        ),
        (
            Some("Inspect"),
            WorkflowPhaseStatus::Active,
            "Inspect · Active"
        )
    );
    let mut older = progress.clone();
    older.updated_at = 102;
    older.metrics.active_duration_ms = Some(1500);
    state.apply_progress(older);
    assert_eq!(state.rows[0].metrics.as_ref(), Some(&progress.metrics));
    let mut older_usage = progress.clone();
    older_usage.metrics.active_duration_ms = Some(2500);
    older_usage.metrics.usage.as_mut().unwrap().input_tokens = 1;
    state.apply_progress(older_usage);
    assert_eq!(state.rows[0].metrics.as_ref(), Some(&progress.metrics));
    run.execution_id = "execution-2".into();
    run.updated_at = 103;
    state.apply_update(project(run.clone()));
    progress.updated_at = 999;
    progress.metrics.active_duration_ms = Some(900000);
    state.apply_progress(progress.clone());
    assert_eq!(
        state.rows[0].metrics.as_ref(),
        Some(&run.metrics),
        "a prior retry execution cannot update the current run"
    );
    progress.execution_id = run.execution_id.clone();
    progress.owner_thread_id = ThreadId::from_u128(8).to_string();
    state.apply_progress(progress.clone());
    assert_eq!(
        state.rows[0].metrics.as_ref(),
        Some(&run.metrics),
        "the workflow owner must match"
    );
    progress.owner_thread_id = run.owner_thread_id.clone();
    let mut terminal = run.clone();
    terminal.status = WorkflowStatus::Succeeded;
    for phase in &mut terminal.phase_progress {
        phase.status = WorkflowPhaseStatus::Skipped;
    }
    terminal.metrics.usage_complete = true;
    state.apply_update(project(terminal.clone()));
    state.apply_progress(progress.clone());
    assert_eq!(
        (
            state.rows[0].status.as_str(),
            state.rows[0].metrics.as_ref()
        ),
        ("Completed", Some(&terminal.metrics)),
        "stale running accounting cannot regress the terminal lifecycle"
    );
    state.hidden_from_strip.insert(run.id);
    progress.status = WorkflowStatus::Succeeded;
    progress.metrics = terminal.metrics.clone();
    progress.metrics.active_duration_ms = Some(9000);
    state.apply_progress(progress);
    assert_eq!(
        (
            state.rows.len(),
            state.rows[0]
                .metrics
                .as_ref()
                .and_then(|metrics| metrics.active_duration_ms),
            state.current_indices()
        ),
        (1, Some(9000), vec![]),
        "footer-hidden runs keep current accounting and remain inspectable"
    );
}

#[test]
fn delayed_full_and_list_snapshots_keep_newer_accounting_and_apply_lifecycle_details() {
    let mut run = canonical_run();
    run.status = WorkflowStatus::Running;
    run.metrics = serde_json::from_value(json!({"usage":{"inputTokens":1000,"outputTokens":100,"cachedInputTokens":0,"totalTokens":1100},"usageComplete":false,"activeDurationMs":1000,"wallDurationMs":1000})).unwrap();
    let mut value = serde_json::to_value(&run).unwrap();
    value["stepCount"] = json!(0);
    value["phaseCount"] = json!(2);
    let mut progress: codex_app_server_protocol::WorkflowSummary =
        serde_json::from_value(value).unwrap();
    progress.updated_at = 101;
    progress.metrics = serde_json::from_value(json!({"usage":{"inputTokens":12000,"outputTokens":2000,"cachedInputTokens":1000,"totalTokens":14000},"usageComplete":false,"activeDurationMs":2000,"wallDurationMs":2000})).unwrap();
    let mut state = WorkflowState {
        rows: vec![project(run.clone())],
        child: Some(1),
        ..Default::default()
    };
    state.apply_progress(progress.clone());
    run.phase = Some("Inspect".into());
    run.phase_progress[0].status = WorkflowPhaseStatus::Active;
    run.logs.push(codex_app_server_protocol::WorkflowLog {
        sequence: 2,
        message: "Inspection began".into(),
        phase: Some("Inspect".into()),
        created_at: 100,
    });
    state.apply_update(project(run.clone()));
    assert_eq!(
        state.rows[0].metrics.as_ref(),
        Some(&progress.metrics),
        "a delayed full event keeps newer accounting"
    );
    assert_eq!(
        (
            state.rows[0].phase.as_deref(),
            state.rows[0].phase_progress[0].status,
            state.rows[0].logs.last().unwrap().message.as_str(),
            state.selected_node_key()
        ),
        (
            Some("Inspect"),
            WorkflowPhaseStatus::Active,
            "Inspection began",
            "audit-run:phase:Some(\"Verify\")".into()
        )
    );
    state.merge(vec![project(run.clone())]);
    assert_eq!(
        state.rows[0].metrics.as_ref(),
        Some(&progress.metrics),
        "a fresh list may contain an older persisted accounting checkpoint"
    );
    run.status = WorkflowStatus::Succeeded;
    run.phase_progress[0].status = WorkflowPhaseStatus::Completed;
    run.phase_progress[1].status = WorkflowPhaseStatus::Skipped;
    run.metrics = progress.metrics;
    run.metrics.usage_complete = true;
    state.apply_update(project(run.clone()));
    run.metrics.usage_complete = false;
    run.metrics.usage_incomplete_reason = Some("stale incomplete accounting".into());
    state.apply_update(project(run));
    let metrics = state.rows[0].metrics.as_ref().unwrap();
    assert!(metrics.usage_complete);
    assert_eq!(
        metrics.usage_incomplete_reason, None,
        "complete terminal coverage cannot regress"
    );
}

#[test]
fn saved_input_defaults_to_an_object_but_preserves_explicit_null_and_json() {
    for (text, expected) in [
        ("", json!({})),
        (" \n\t ", json!({})),
        ("null", serde_json::Value::Null),
        ("{\"target\":\"src\"}", json!({"target":"src"})),
    ] {
        let state = fixture();
        state.lock().unwrap().displayed_thread = Some(ThreadId::from_u128(42));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
        center.handle_key_event(KeyCode::Char('l').into());
        center.handle_key_event(KeyCode::Enter.into());
        center.handle_paste(text.into());
        center.handle_key_event(KeyCode::Enter.into());
        let AppEvent::Workflow(WorkflowEvent::StartSaved {
            owner,
            caller,
            id,
            input,
            request_id,
        }) = rx.try_recv().unwrap()
        else {
            panic!("saved input must dispatch a workflow start");
        };
        assert_eq!(
            (owner, caller, id, input),
            (
                ThreadId::from_u128(7),
                ThreadId::from_u128(42),
                "saved-review".into(),
                expected
            )
        );
        assert_eq!(state.lock().unwrap().operation_id, Some(request_id));
    }
}

#[test]
fn command_catalog_overflow_has_a_visible_library_fallback_at_both_widths() {
    let state = fixture();
    state.lock().unwrap().catalog_overflow = true;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state, AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Char('l').into());
    let wide = render(&center, 120, 18);
    assert!(
        wide.contains("catalog limit reached; use /workflows"),
        "{wide}"
    );
    insta::assert_snapshot!("workflow_saved_catalog_overflow", wide);
    let narrow = render(&center, 42, 14);
    assert!(
        narrow.contains("catalog limit reached; use /workflows"),
        "{narrow}"
    );
    insta::assert_snapshot!("workflow_saved_catalog_overflow_narrow", narrow);
}
