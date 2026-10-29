use super::*;
use codex_protocol::protocol::ExecutionControlState;
use pretty_assertions::assert_eq;

#[test]
fn paused_activity_retains_clock_without_spinner_or_interrupt() {
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut row = StatusIndicatorWidget::new(
        AppEventSender::new(sender),
        FrameRequester::test_dummy(),
        /*animations_enabled*/ true,
        Default::default(),
    );
    row.update_header("Inspecting the change".into());
    let start = Instant::now();
    let mut timer = StatusTimer {
        last_resume_at: start,
        ..Default::default()
    };
    timer.set_execution_paused(/*paused*/ true, start + Duration::from_secs(12));
    row.set_execution_state(Some(ExecutionControlState::Pausing));
    let render = |row: &StatusIndicatorWidget| {
        StatusIndicator { row, timer: &timer }
            .lines(/*width*/ 80)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    };
    insta::assert_snapshot!("pausing_activity", render(&row));
    row.set_execution_state(Some(ExecutionControlState::Paused));
    insta::assert_snapshot!("paused_activity", render(&row));
    assert_eq!(render(&row), "Paused \n  (12s)");
    assert_eq!(row.header(), "Inspecting the change");
}
