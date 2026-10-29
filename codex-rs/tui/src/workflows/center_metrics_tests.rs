//! Authoritative accounting and terminal timestamps survive selection and unrelated updates.
use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn accounting_state() -> SharedWorkflows {
    let now = chrono::Utc::now().timestamp();
    let run=serde_json::from_value(json!({
        "id":"accounting-run","executionId":"execution-1","ownerThreadId":ThreadId::from_u128(7).to_string(),
        "formatVersion":2,"readOnly":false,"metadata":{"name":"audit","description":"Review the project","phases":[{"title":"Review"}]},
        "source":"return workflow.args;","input":{},"status":"succeeded","phase":"Review","phases":["Review"],"createdAt":1,"updatedAt":now,
        "metrics":{"agentsUsed":4,"usageComplete":true},
        "phaseProgress":[{"title":"Review","status":"completed","stepsDiscovered":1,"stepsCompleted":1,"updatedAt":now-10,"completedAt":now-180,
            "metrics":{"usage":{"inputTokens":221564,"outputTokens":380804,"cachedInputTokens":0,"totalTokens":602368},"usageComplete":true,"modelTurns":10,"agentsUsed":4}}],
        "steps":{"opaque":{"id":"opaque","logicalId":"review","label":"Independent audit","phase":"Review","options":{"message":"Check the migration plan"},"result":"Ready for **review**", "updatedAt":now-5,"completedAt":now-120,
            "metrics":{"usage":{"inputTokens":55391,"outputTokens":95201,"cachedInputTokens":12000,"totalTokens":150592},"usageComplete":true,"modelTurns":3,"agentsUsed":1,"activeDurationMs":220000},
            "attempts":[{"id":"rejected","status":"failed","admission":"rejected","error":"Capacity unavailable"},
                {"id":"accepted","status":"succeeded","admission":"accepted","childThreadId":ThreadId::from_u128(42).to_string(),"resolvedModel":"gpt-6.1-sol","reasoningEffort":"high","completedAt":now-120,"updatedAt":now-5,
                "metrics":{"usage":{"inputTokens":55391,"outputTokens":95201,"cachedInputTokens":12000,"totalTokens":150592},"usageComplete":true,"modelTurns":3,"agentsUsed":1,"activeDurationMs":220000}}]}}
    })).unwrap();
    Rc::new(Mutex::new(WorkflowState {
        owner: Some(ThreadId::from_u128(7)),
        displayed_thread: Some(ThreadId::from_u128(7)),
        rows: vec![project(run)],
        child: Some(1),
        ..Default::default()
    }))
}

#[test]
fn agent_overview_pairs_full_counts_actual_model_and_accepted_attempt_number() {
    let state = accounting_state();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let center = WorkflowCenter::new(state, AppEventSender::new(tx));
    let screen = render(&center, 320, 30);
    for text in [
        "Completed (1st attempt)",
        "gpt-6.1-sol (high)",
        "55.391 in, 95.201 out (3 model turns)",
        "12.000 (included in input)",
        "3m 40s",
        "2m ago",
    ] {
        assert!(screen.contains(text), "missing {text}: {screen}");
    }
    insta::assert_snapshot!("workflow_agent_accounting", screen);
}

#[test]
fn phase_overview_uses_producer_totals_without_summing_the_visible_steps_or_clocks() {
    let state = accounting_state();
    state.lock().unwrap().child = Some(0);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let center = WorkflowCenter::new(state, AppEventSender::new(tx));
    let screen = render(&center, 320, 28);
    assert!(
        screen.contains("221.564 in, 380.804 out (10 model turns across 4 agents)"),
        "{screen}"
    );
    assert!(
        screen.contains("3m ago") && screen.contains("Independent audit"),
        "{screen}"
    );
    assert!(
        !screen.contains("Active time"),
        "a phase has no clock: {screen}"
    );
    insta::assert_snapshot!("workflow_phase_accounting", screen);
}

#[test]
fn narrow_partial_agent_accounting_and_legacy_unknowns_remain_distinct() {
    let state = accounting_state();
    {
        let mut state = state.lock().unwrap();
        let metrics = state.rows[0].children[1]
            .overview
            .as_mut()
            .unwrap()
            .metrics
            .as_mut()
            .unwrap();
        metrics.usage_complete = false;
        metrics.model_turns = None;
    }
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    render(&center, 42, 20);
    center.handle_key_event(KeyCode::Char('1').into());
    let partial = render(&center, 42, 20);
    let visible_words = partial.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        visible_words.contains("≥55.391 in, ≥95.201 out")
            && visible_words.contains("? model turns"),
        "{partial}"
    );
    *state.lock().unwrap().rows[0].children[1]
        .overview
        .as_mut()
        .unwrap()
        .metrics
        .as_mut()
        .unwrap() = Default::default();
    let unknown = render(&center, 42, 20);
    let unknown_words = unknown.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        unknown_words.contains("Tokens: ? in, ? out (? model turns)"),
        "{unknown}"
    );
    insta::assert_snapshot!(
        "workflow_agent_accounting_narrow_partial",
        format!("Partial accounting\n{partial}\n\nUnknown accounting\n{unknown}")
    );
}

#[test]
fn terminal_entity_timestamps_and_model_response_counts_do_not_follow_run_refresh_time() {
    let state = accounting_state();
    let mut state = state.lock().unwrap();
    let phase = state.rows[0].children[0].updated_at;
    let step = state.rows[0].children[1].updated_at;
    let mut refresh = state.rows[0].clone();
    refresh.updated_at += 1000;
    state.apply_update(refresh);
    assert_eq!(
        (
            state.rows[0].children[0].updated_at,
            state.rows[0].children[1].updated_at
        ),
        (phase, step)
    );
    let overview = state.rows[0].children[1].overview.as_ref().unwrap();
    assert_eq!(overview.metrics.as_ref().unwrap().model_turns, Some(3));
    let mut legacy = state.rows[0].clone();
    legacy.children[1].updated_at = None;
    legacy.children[1].overview.as_mut().unwrap().metrics = Some(Default::default());
    state.apply_update(legacy);
    assert_eq!(state.rows[0].children[1].updated_at, None);
    assert_eq!(
        super::super::display::overview_metrics(
            state.rows[0].children[1]
                .overview
                .as_ref()
                .unwrap()
                .metrics
                .as_ref()
                .unwrap()
        ),
        vec![("Tokens".into(), "? in, ? out (? model turns)".into())]
    );
}

#[test]
fn compact_progress_refreshes_phase_accounting_without_reopening_terminal_time_or_regressing_counts()
 {
    let state = accounting_state();
    let mut state = state.lock().unwrap();
    let row = &state.rows[0];
    let mut summary:codex_app_server_protocol::WorkflowSummary=serde_json::from_value(json!({
        "id":row.id,"executionId":row.execution_id,"ownerThreadId":row.owner.unwrap().to_string(),"formatVersion":2,"readOnly":false,
        "status":"succeeded","phase":"Review","phaseCount":1,"stepCount":1,"createdAt":row.created_at,"updatedAt":row.updated_at,
        "metrics":row.metrics,"phaseProgress":row.phase_progress
    })).unwrap();
    let completed = state.rows[0].children[0].updated_at;
    summary.phase_progress[0].metrics.model_turns = Some(11);
    summary.phase_progress[0].completed_at = None;
    summary.phase_progress[0].updated_at = Some(summary.updated_at + 1000);
    state.apply_progress(summary.clone());
    assert_eq!(
        (
            state.rows[0].children[0]
                .overview
                .as_ref()
                .unwrap()
                .metrics
                .as_ref()
                .unwrap()
                .model_turns,
            state.rows[0].children[0].updated_at
        ),
        (Some(11), completed)
    );
    summary.phase_progress[0].metrics.model_turns = Some(3);
    state.apply_progress(summary);
    assert_eq!(
        state.rows[0].children[0]
            .overview
            .as_ref()
            .unwrap()
            .metrics
            .as_ref()
            .unwrap()
            .model_turns,
        Some(11)
    );
    assert_eq!(state.selected_node_key(), "accounting-run:step:opaque");
}
