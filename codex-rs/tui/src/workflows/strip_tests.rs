use super::WorkflowStrip;
use crate::render::renderable::Renderable;
use crate::width::display_width;
use crate::workflows::SharedWorkflows;
use crate::workflows::WorkflowRow;
use crate::workflows::WorkflowState;
use codex_protocol::ThreadId;
use codex_protocol::protocol::ExecutionControlState;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use serde_json::json;
use std::rc::Rc;
use std::sync::Mutex;

fn row(id: &str, title: &str, status: &str) -> WorkflowRow {
    let mut row = WorkflowRow {
        id: id.into(), title: title.into(), status: status.into(),
        owner: Some(ThreadId::from_u128(7)), completed: Some(2), discovered: 3, agents_used: Some(3),
        metadata: Some(serde_json::from_value(json!({"name":title,"description":"Audit project changes","phases":[{"title":"Inspect"},{"title":"Verify"}]})).unwrap()),
        phase: Some("Verify".into()),
        phase_progress: serde_json::from_value(json!([
            {"title":"Inspect","status":"completed","stepsDiscovered":2,"stepsCompleted":2},
            {"title":"Verify","status":"active","stepsDiscovered":1,"stepsCompleted":0}
        ])).unwrap(),
        metrics: Some(serde_json::from_value(json!({"usage":{"inputTokens":312000,"outputTokens":86000,"cachedInputTokens":12000,"totalTokens":398000},"usageComplete":true,"activeDurationMs":1737000,"wallDurationMs":2000000})).unwrap()),
        created_at: 100, updated_at: 200, ..Default::default()
    };
    if status == "Completed" {
        row.phase_progress[1].status = codex_app_server_protocol::WorkflowPhaseStatus::Completed;
    }
    if status == "Failed" {
        row.phase_progress[1].status = codex_app_server_protocol::WorkflowPhaseStatus::Failed;
    }
    if status == "Pending" {
        row.phase = None;
        for phase in &mut row.phase_progress {
            phase.status = codex_app_server_protocol::WorkflowPhaseStatus::Pending;
            phase.steps_discovered = 0;
            phase.steps_completed = 0;
        }
        row.completed = Some(0);
        row.discovered = 0;
        row.agents_used = Some(0);
        row.metrics.as_mut().unwrap().usage = None;
        row.metrics.as_mut().unwrap().usage_complete = false;
        row.metrics.as_mut().unwrap().active_duration_ms = Some(0);
    }
    row
}

fn state(rows: Vec<WorkflowRow>) -> SharedWorkflows {
    Rc::new(Mutex::new(WorkflowState {
        owner: Some(ThreadId::from_u128(7)),
        rows,
        ..Default::default()
    }))
}

fn render_height(strip: &WorkflowStrip, width: u16, height: u16) -> (Buffer, Vec<String>) {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    strip.render(area, &mut buffer);
    let rows = (0..height)
        .map(|y| {
            let mut text = String::new();
            let mut x = 0;
            while x < width {
                let symbol = buffer[(x, y)].symbol();
                text.push_str(symbol);
                x += u16::try_from(display_width(symbol).max(1)).expect("terminal cell width");
            }
            text.trim_end().to_owned()
        })
        .collect();
    (buffer, rows)
}

#[test]
fn physical_workflow_rows_prioritize_current_active_work_then_recent_terminal_work() {
    let shared = state(vec![
        WorkflowRow {
            updated_at: 1000,
            ..row("finished", "Newest report", "Completed")
        },
        WorkflowRow {
            created_at: 200,
            ..row("c-running", "Inspect source", "Running")
        },
        row("pending", "Prepare checks", "Pending"),
        WorkflowRow {
            created_at: 200,
            ..row("a-paused", "Paused audit", "Paused")
        },
        WorkflowRow {
            updated_at: 999,
            ..row("failed", "Failed check", "Failed")
        },
        WorkflowRow {
            owner: Some(ThreadId::from_u128(8)),
            ..row("foreign", "Other conversation", "Running")
        },
    ]);
    let strip = WorkflowStrip(shared);
    assert_eq!(strip.desired_height(120), 5);
    let (buffer, rows) = render_height(&strip, 140, 5);
    for (line, title) in rows.iter().zip([
        "Paused audit",
        "Inspect source",
        "Prepare checks",
        "Newest report",
        "… +1 workflow",
    ]) {
        assert!(line.contains(title), "{line}");
    }
    assert!(
        rows[0].contains("3 agents used"),
        "all recorded agent identities remain visible when space allows"
    );
    assert!(!rows.join("\n").contains("Other conversation"));
    assert!(buffer[(2, 0)].modifier.contains(Modifier::BOLD));
    assert!(!buffer[(2, 3)].modifier.contains(Modifier::BOLD));
    insta::assert_snapshot!("workflow_strip_current_work", rows.join("\n"));
    let short = render_height(&strip, 60, 2).1;
    assert!(short[0].starts_with("  Paused") && short[1].contains("+4 workflows"));
    insta::assert_snapshot!("workflow_strip_short_overflow", short.join("\n"));
}

#[test]
fn terminal_order_uses_update_time_and_stable_identity() {
    let strip = WorkflowStrip(state(vec![
        WorkflowRow {
            updated_at: 99,
            ..row("older", "Old result", "Completed")
        },
        row("b", "Result B", "Stopped"),
        row("a", "Result A", "Failed"),
    ]));
    let (_, rows) = render_height(&strip, 120, 3);
    for (line, title) in rows.iter().zip(["Result A", "Result B", "Old result"]) {
        assert!(line.contains(title));
    }
}

#[test]
fn pause_and_terminal_transitions_freeze_the_authoritative_active_duration() {
    let shared = state(vec![row("audit", "audit", "Running")]);
    let strip = WorkflowStrip(shared.clone());
    let running = render_height(&strip, 140, 1).1.remove(0);
    assert!(
        running.contains("[■■■■□□□□] 1/2 phases")
            && running.contains("in 312K · out 86K")
            && running.contains("28m 57s"),
        "{running}"
    );
    shared.lock().unwrap().execution = Some(ExecutionControlState::Paused);
    let paused = render_height(&strip, 140, 1).1.remove(0);
    assert!(
        paused.contains("Paused") && paused.contains("28m 57s"),
        "{paused}"
    );
    {
        let mut state = shared.lock().unwrap();
        state.rows[0].status = "Completed".into();
        state.rows[0].phase_progress[1].status =
            codex_app_server_protocol::WorkflowPhaseStatus::Completed;
        state.rows[0].updated_at = 9000000;
    }
    let completed = render_height(&strip, 140, 1).1.remove(0);
    assert!(
        completed.contains("Completed")
            && completed.contains("28m 57s")
            && !completed.contains("Paused"),
        "{completed}"
    );
    insta::assert_snapshot!(
        "workflow_strip_authoritative_clock",
        format!("{running}\n{paused}\n{completed}")
    );
}

#[test]
fn narrow_rows_keep_input_and_output_together_and_mark_partial_or_unknown_usage() {
    let shared = state(vec![row("audit", "audit", "Running")]);
    let strip = WorkflowStrip(shared.clone());
    let narrow = render_height(&strip, 32, 1).1.remove(0);
    assert!(narrow.contains("I 312K/O 86K"), "{narrow}");
    assert!(
        !narrow.contains("Verify") && !narrow.contains('■'),
        "optional phase and bar yield before usage: {narrow}"
    );
    shared.lock().unwrap().rows[0]
        .metrics
        .as_mut()
        .unwrap()
        .usage_complete = false;
    let partial = render_height(&strip, 100, 1).1.remove(0);
    assert!(partial.contains("in ≥312K · out ≥86K"), "{partial}");
    shared.lock().unwrap().rows[0]
        .metrics
        .as_mut()
        .unwrap()
        .usage = None;
    let unknown = render_height(&strip, 100, 1).1.remove(0);
    assert!(
        unknown.contains("in ? · out ?") && !unknown.contains("in 0"),
        "{unknown}"
    );
    insta::assert_snapshot!(
        "workflow_strip_usage_states",
        format!("{narrow}\n{partial}\n{unknown}")
    );
}

#[test]
fn unknown_phase_totals_are_indeterminate_and_skipped_phases_are_explicitly_resolved() {
    let shared = state(vec![row("audit", "audit", "Running")]);
    let strip = WorkflowStrip(shared.clone());
    shared.lock().unwrap().rows[0].phase_progress[1].status =
        codex_app_server_protocol::WorkflowPhaseStatus::Skipped;
    shared.lock().unwrap().rows[0].phase_progress[1].steps_discovered = 0;
    let skipped = render_height(&strip, 140, 1).1.remove(0);
    assert!(skipped.contains("[■■■■■■■■] 2/2 phases"), "{skipped}");
    let mut unphased = row("unphased", "unphased audit", "Running");
    unphased.metadata.as_mut().unwrap().phases.clear();
    unphased.phase = None;
    unphased.phase_progress.clear();
    let unknown = render_height(&WorkflowStrip(state(vec![unphased])), 140, 1)
        .1
        .remove(0);
    assert!(
        unknown.contains("[········] indeterminate · 2/3 steps") && !unknown.contains("2/2 phases"),
        "{unknown}"
    );
    let mut publish = row("publish", "publish", "Completed");
    publish.metadata.as_mut().unwrap().phases.clear();
    publish.phase = None;
    publish.phase_progress.clear();
    publish.completed = Some(0);
    publish.discovered = 0;
    publish.agents_used = Some(0);
    publish.metrics.as_mut().unwrap().usage = None;
    publish.metrics.as_mut().unwrap().active_duration_ms = Some(0);
    let publish = render_height(&WorkflowStrip(state(vec![publish])), 140, 1)
        .1
        .remove(0);
    assert!(
        publish.contains("No agent steps") && !publish.contains("0/0"),
        "{publish}"
    );
    insta::assert_snapshot!(
        "workflow_strip_phase_states",
        format!("{skipped}\n{unknown}\n{publish}")
    );
}

#[test]
fn empty_current_conversation_and_agent_inspection_hint_respect_height() {
    let shared = state(vec![WorkflowRow {
        owner: Some(ThreadId::from_u128(9)),
        ..row("other", "Foreign report", "Running")
    }]);
    let strip = WorkflowStrip(shared.clone());
    assert_eq!(strip.desired_height(80), 0);
    assert_eq!(render_height(&strip, 80, 1).1, vec![""]);
    shared.lock().unwrap().inspected_agent = Some(ThreadId::from_u128(42));
    assert_eq!(strip.desired_height(80), 1);
    assert_eq!(
        render_height(&strip, 80, 1).1,
        vec!["  Workflow agent · ↓ /workflows"]
    );
}

#[test]
fn unicode_names_and_overflow_fit_each_physical_row_at_tiny_sizes() {
    let mut rows = vec![row(
        "first",
        "研究 🦀 e\u{301}valuation\nlong report",
        "Running",
    )];
    for index in 2..=6 {
        rows.push(row(
            &format!("row-{index}"),
            &format!("Result {index}"),
            "Completed",
        ));
    }
    let strip = WorkflowStrip(state(rows));
    for width in [1, 2, 8, 20, 32, 40, 80, 160] {
        for height in [1, 2, 4, 5] {
            let (_, rows) = render_height(&strip, width, height);
            assert!(
                rows.iter()
                    .all(|row| display_width(row) <= usize::from(width))
            );
            assert_eq!(rows.len(), usize::from(height));
        }
    }
    assert!(render_height(&strip, 40, 1).1[0].ends_with("… +5"));
    insta::assert_snapshot!(
        "workflow_strip_unicode_narrow",
        render_height(&strip, 32, 5).1.join("\n")
    );
    insta::assert_snapshot!(
        "workflow_strip_unicode_wide",
        render_height(&strip, 140, 5).1.join("\n")
    );
}

#[test]
fn active_and_terminal_rows_keep_theme_status_and_secondary_text_readable() {
    for (fg, bg) in [
        ((240, 240, 240), (20, 20, 20)),
        ((20, 20, 20), (250, 250, 250)),
    ] {
        crate::terminal_palette::with_test_default_colors(
            crate::terminal_probe::DefaultColors { fg, bg },
            || {
                let strip = WorkflowStrip(state(vec![
                    row("audit", "audit", "Running"),
                    row("report", "report", "Completed"),
                ]));
                let (buffer, rows) = render_height(&strip, 140, 2);
                assert!(buffer[(2, 0)].modifier.contains(Modifier::BOLD));
                assert!(!buffer[(2, 1)].modifier.contains(Modifier::BOLD));
                let completed = rows[1].find("Completed").unwrap();
                let x = display_width(&rows[1][..completed]) as u16;
                assert_eq!(
                    buffer[(x, 1)].fg,
                    crate::style::status_style(crate::style::StatusTone::Success)
                        .fg
                        .unwrap()
                );
                let usage = rows[0].find("in 312K").unwrap();
                let x = display_width(&rows[0][..usage]) as u16;
                assert_eq!(
                    buffer[(x, 0)].fg,
                    crate::style::readable_color_on(buffer[(x, 0)].fg, None)
                );
            },
        );
    }
}
