use super::*;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::render::renderable::Renderable;
use crossterm::event::KeyCode;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn fixture() -> SharedWorkflows {
    Rc::new(Mutex::new(WorkflowState {
        rows: vec![WorkflowRow {
            id: "workflow-1".into(),
            title: "Review changes".into(),
            status: "Running".into(),
            owner: Some(ThreadId::from_u128(7)),
            completed: Some(2),
            discovered: 5,
            agents_used: Some(2),
            metadata: Some(serde_json::from_value(serde_json::json!({"name":"Review changes","description":"Review project changes","phases":[]})).unwrap()),
            project_root: Some("/project".into()),
            created_at: 0,
            updated_at: 0,
            details: Vec::new(),
            source: "workflow.meta({ name: 'Review changes', description: 'Review project changes', phases: [] });\nconst result = await workflow.agent('review', { message: 'Review source' });"
                .into(),
            overview: Some(WorkflowOverview {
                title: "Review changes".into(),
                metadata: vec![("Status".into(), "Running".into())],
                input: Some(serde_json::json!({"target":"src"})),
                result: Some(
                    serde_json::json!({"summary":"## Findings\n\n- **One** issue found\n- Check `src/main.rs`"}),
                ),
                ..Default::default()
            }),
            children: vec![
                WorkflowChild {
                    key: "step:review".into(),
                    label: "review · Running".into(),
                    depth: 1,
                    thread_id: Some(ThreadId::from_u128(42)),
                    transcript_target: Some(codex_app_server_protocol::WorkflowTranscriptTarget::Step {step_id:"review".into()}),
                    action_target:Some(actions::ActionTarget::Step {logical_id:"review".into(),expected_attempt_id:Some("reviewer-latest".into()),expected_turn_id:None}),
                    overview: Some(WorkflowOverview {
                        title: "review".into(),
                        prompt: Some("Review source".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                WorkflowChild {
                    key: "attempt:reviewer".into(),
                    parent_step_key: Some("step:review".into()),
                    label: "review · Attempt 1 · Failed".into(),
                    depth: 1,
                    thread_id: Some(ThreadId::from_u128(43)),
                    transcript_target: Some(codex_app_server_protocol::WorkflowTranscriptTarget::Attempt {step_id:"review".into(),attempt_id:"reviewer".into()}),
                    action_target:Some(actions::ActionTarget::Step {logical_id:"review".into(),expected_attempt_id:Some("reviewer".into()),expected_turn_id:None}),
                    overview: Some(WorkflowOverview {
                        title: "review · Attempt 1".into(),
                        metadata: vec![
                            ("Status".into(), "Failed".into()),
                            ("Attempt".into(), "1".into()),
                        ],
                        prompt: Some("Review source".into()),
                        result: None,
                        error: Some("Review tool failed before producing a result.".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        owner: Some(ThreadId::from_u128(7)),
        displayed_thread: Some(ThreadId::from_u128(7)),
        saved: vec![SavedWorkflowRow {
            id: "saved-review".into(),
            name: "Review project".into(),
            description: Some("Project workflow".into()),
            scope: Some(codex_app_server_protocol::WorkflowScope::Project),
            metadata: Some(serde_json::from_value(serde_json::json!({"name":"Review project","description":"Project workflow","phases":[]})).unwrap()),
            ..Default::default()
        }],
        ..Default::default()
    }))
}

fn render(view: &dyn Renderable, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer);
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn selected_transcript_key(state: &SharedWorkflows) -> String {
    state
        .lock()
        .unwrap()
        .selected_transcript_target()
        .expect("selected workflow target")
        .cache_key()
}

#[test]
fn refresh_keeps_order_selection_and_footer_hidden_work_inspectable() {
    let shared = fixture();
    let mut state = shared.lock().unwrap();
    let first = state.rows[0].clone();
    state.rows.push(WorkflowRow {
        id: "workflow-2".into(),
        ..first.clone()
    });
    state.selected = 1;
    state.hidden_from_strip.insert("hidden".into());
    state.merge(vec![
        WorkflowRow {
            id: "workflow-2".into(),
            status: "Completed".into(),
            ..first.clone()
        },
        WorkflowRow {
            id: "hidden".into(),
            ..first.clone()
        },
        first,
    ]);
    assert_eq!(
        state
            .rows
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        vec!["workflow-1", "workflow-2", "hidden"]
    );
    assert_eq!(
        (state.selected, state.rows[1].status.as_str()),
        (1, "Completed")
    );
    assert_eq!(state.current_indices(), vec![0, 1]);
    assert_eq!(state.visible_run_indices(), vec![0, 1, 2]);
    state.hidden_from_strip.remove("hidden");
    assert_eq!(state.current_indices(), vec![0, 1, 2]);
}

#[test]
fn command_center_navigates_steps_and_dispatches_selected_agent_inspection() {
    let state = fixture();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    insta::assert_snapshot!("workflow_center_wide", render(&center, 120, 22));
    insta::assert_snapshot!("workflow_center_narrow", render(&center, 42, 12));
    let short = render(&center, 42, 4);
    assert!(
        short.contains("› Review changes"),
        "the selected run must remain visible: {short}"
    );
    insta::assert_snapshot!("workflow_center_short", short);
    let overview = render(&center, 120, 22);
    assert!(!overview.contains("const result"));
    assert!(!overview.contains("\\n"));
    center.handle_key_event(KeyCode::Down.into());
    assert_eq!(state.lock().unwrap().child, Some(0));
    center.handle_key_event(KeyCode::Char('o').into());
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::SelectAgent(id)) if id == ThreadId::from_u128(42))
    );
    assert!(center.is_complete());
    assert_eq!(state.lock().unwrap().child, Some(0));
}

#[test]
fn arrows_reach_all_visible_tree_levels_and_next_run_without_enter() {
    let shared = fixture();
    let mut state = shared.lock().unwrap();
    state.rows[0].children.insert(
        0,
        WorkflowChild {
            key: "phase:review".into(),
            label: "Review".into(),
            depth: 1,
            ..Default::default()
        },
    );
    let mut second = state.rows[0].clone();
    second.id = "next-run".into();
    second.children.clear();
    state.rows.push(second);
    let mut visited = vec![state.selected_node_key()];
    for _ in 0..3 {
        state.navigate_tree(/*down*/ true);
        visited.push(state.selected_node_key());
    }
    assert_eq!(
        visited,
        vec![
            "run:workflow-1",
            "workflow-1:phase:review",
            "workflow-1:step:review",
            "run:next-run"
        ]
    );
    state.navigate_tree(/*down*/ false);
    assert_eq!(state.selected_node_key(), "workflow-1:step:review");
}

#[test]
fn saved_workflow_preserves_invalid_input_until_corrected() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let state = fixture();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Char('l').into());
    center.handle_key_event(KeyCode::Enter.into());
    center.handle_paste("{\"target\":\"src\"}".into());
    insta::assert_snapshot!("workflow_saved_input", render(&center, 90, 14));
    insta::assert_snapshot!("workflow_saved_input_compact", render(&center, 42, 4));
    center.handle_key_event(KeyCode::Enter.into());
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::StartSaved { id, input, .. }) if id == "saved-review" && input == serde_json::json!({ "target": "src" }))
    );
    insta::assert_snapshot!("workflow_saved_starting", render(&center, 90, 14));
    {
        let mut state = state.lock().unwrap();
        state.operation_pending = false;
        state.operation_result = Some(Ok(()));
    }
    center.pre_draw_tick(std::time::Instant::now());
    center.handle_key_event(KeyCode::Char('l').into());
    center.handle_key_event(KeyCode::Enter.into());
    center.handle_paste("invalid JSON".into());
    center.handle_key_event(KeyCode::Enter.into());
    assert!(rx.try_recv().is_err());
    insta::assert_snapshot!("workflow_saved_invalid_input", render(&center, 90, 14));
    insta::assert_snapshot!(
        "workflow_saved_invalid_input_compact",
        render(&center, 42, 4)
    );
    center.handle_key_event(KeyCode::Esc.into());
    assert!(!center.is_complete());
}

#[test]
fn immediate_view_tabs_and_selection_survive_return_to_the_center() {
    let state = fixture();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let sender = AppEventSender::new(tx);
    let mut center = WorkflowCenter::new(state.clone(), sender.clone());
    center.handle_key_event(KeyCode::Down.into());
    center.handle_key_event(KeyCode::Right.into());
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::ReadTranscript { target, older: false }) if target.target == codex_app_server_protocol::WorkflowTranscriptTarget::Step {step_id:"review".into()})
    );
    center.handle_key_event(KeyCode::Right.into());
    insta::assert_snapshot!("workflow_source_detail", render(&center, 120, 18));
    center.handle_key_event(KeyCode::Left.into());
    let transcript_key = selected_transcript_key(&state);
    state.lock().unwrap().transcripts.insert(
        transcript_key,
        WorkflowTranscript {
            cells: vec![std::sync::Arc::new(
                crate::history_cell::AgentMarkdownCell::new(
                    "## Actual transcript\n\n**Shared** history renderer.".into(),
                    std::path::Path::new("/tmp"),
                ),
            )],
            ..Default::default()
        },
    );
    insta::assert_snapshot!("workflow_transcript_detail", render(&center, 120, 18));
    render(&center, 42, 10);
    center.handle_key_event(KeyCode::Char('2').into());
    insta::assert_snapshot!("workflow_transcript_narrow", render(&center, 42, 10));
    center.handle_key_event(KeyCode::Esc.into());
    assert!(!center.is_complete());
    center.handle_key_event(KeyCode::Esc.into());
    assert!(center.is_complete());
    let reopened = WorkflowCenter::new(state.clone(), sender);
    {
        let state = state.lock().unwrap();
        assert_eq!(
            (state.child, state.center.detail_tab),
            (Some(0), DetailTab::Transcript)
        );
    }
    insta::assert_snapshot!(
        "workflow_center_restored_selection",
        render(&reopened, 120, 18)
    );
}

#[test]
fn api_projection_groups_named_phases_in_declaration_order_and_counts_null_success() {
    let run = serde_json::from_value(serde_json::json!({
        "id": "run", "ownerThreadId": ThreadId::from_u128(7).to_string(),
        "source": "workflow.phase('Review');", "input": {}, "status": "running",
        "formatVersion":2,"readOnly":false,"metadata":{"name":"audit","description":"Review changes","phases":[{"title":"Research"},{"title":"Review"}]},
        "limits":{"maxAgents":32,"maxTokens":null},"acceptedStarts":1,
        "metrics":{"usage":null,"usageComplete":false,"agentsUsed":1,"activeDurationMs":null,"wallDurationMs":null},"logs":[],
        "phaseProgress":[{"title":"Research","status":"completed","stepsDiscovered":1,"stepsCompleted":1},{"title":"Review","status":"active","stepsDiscovered":1,"stepsCompleted":0}],
        "phase": "Review", "phases": ["Research", "Review"], "createdAt": 0, "updatedAt": 0,
        "steps": {
            "a-review": { "id": "a-review", "logicalId":"a-review","label":"Review source", "phase": "Review", "options": {"prompt": "Review"}, "attempts": [] },
            "z-research": { "id": "z-research", "logicalId":"z-research","label":"Research source", "phase": "Research", "options": {"prompt": "Research"}, "attempts": [{ "id": "researcher", "status": "succeeded", "usage": {"totalTokens": 17} }], "result": null }
        }
    })).unwrap();
    let row = project(run);
    assert_eq!(
        (row.completed, row.discovered, row.agents_used),
        (Some(1), 2, Some(1))
    );
    assert_eq!(
        row.children
            .iter()
            .map(|child| child.label.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Research · Completed",
            "Research source · Completed",
            "Review · Active",
            "Review source · Discovered"
        ]
    );
}

#[test]
fn delayed_refresh_keeps_newer_notifications_and_hidden_footer_runs_in_the_center() {
    let shared = fixture();
    let mut state = shared.lock().unwrap();
    let stale = state.rows[0].clone();
    state.loading = true;
    state.apply_update(WorkflowRow {
        status: "Completed".into(),
        ..stale.clone()
    });
    state.apply_update(WorkflowRow {
        id: "hidden".into(),
        ..stale.clone()
    });
    state.hidden_from_strip.insert("hidden".into());
    state.loading = false;
    state.merge(vec![stale]);
    assert_eq!(
        state
            .rows
            .iter()
            .map(|row| (row.id.as_str(), row.status.as_str()))
            .collect::<Vec<_>>(),
        vec![("workflow-1", "Completed"), ("hidden", "Running")]
    );
    assert_eq!(state.current_indices(), vec![0]);
    assert_eq!(state.visible_run_indices(), vec![0, 1]);
}

#[test]
fn saved_input_stays_bound_to_its_original_script_across_refreshes() {
    let state = fixture();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Char('l').into());
    center.handle_key_event(KeyCode::Enter.into());
    center.handle_paste("{\"target\":\"src\"}".into());
    state.lock().unwrap().saved.insert(
        0,
        SavedWorkflowRow {
            id: "other".into(),
            name: "Other script".into(),
            description: None,
            scope: Some(codex_app_server_protocol::WorkflowScope::Project),
            ..Default::default()
        },
    );
    center.pre_draw_tick(std::time::Instant::now());
    center.handle_key_event(KeyCode::Enter.into());
    assert!(
        matches!(rx.try_recv().unwrap(), AppEvent::Workflow(WorkflowEvent::StartSaved { id, .. }) if id == "saved-review")
    );
    {
        let mut state = state.lock().unwrap();
        state.operation_pending = false;
        state.saved.retain(|saved| saved.id != "saved-review");
    }
    center.handle_key_event(KeyCode::Enter.into());
    assert!(rx.try_recv().is_err());
    insta::assert_snapshot!("workflow_removed_saved_input", render(&center, 90, 14));
}

#[test]
fn discovered_steps_preserve_selected_agent_identity() {
    let shared = fixture();
    let mut state = shared.lock().unwrap();
    state.child = Some(0);
    let mut updated = state.rows[0].clone();
    updated.children.insert(
        0,
        WorkflowChild {
            key: "phase:new".into(),
            label: "New phase".into(),
            ..Default::default()
        },
    );
    state.apply_update(updated.clone());
    assert_eq!(state.child, Some(1));
    assert_eq!(
        state.rows[0].children[state.child.unwrap()].thread_id,
        Some(ThreadId::from_u128(42))
    );
    updated.children.remove(0);
    state.merge(vec![updated]);
    assert_eq!(state.child, Some(0));
}

#[path = "center_disclosure_tests.rs"]
mod center_disclosure;
#[path = "center_metrics_tests.rs"]
mod center_metrics;
#[path = "center_navigation_tests.rs"]
mod center_navigation;
#[path = "command_center_tests.rs"]
mod command_center;
#[path = "detail_viewport_tests.rs"]
mod detail_viewport;
#[path = "display_v2_tests.rs"]
mod display_v2;
#[path = "worker_rows_tests.rs"]
mod worker_rows;
