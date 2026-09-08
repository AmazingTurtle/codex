use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn report_completion_renders_link_and_path() {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let path = app.config.codex_home.join("report with spaces.html");
    app.finish_report(Ok(path.to_path_buf()));
    let mut output = Vec::new();
    while let Ok(AppEvent::InsertHistoryCell(cell)) = events.try_recv() {
        output.extend(cell.display_lines(/*width*/ 500));
    }
    let rendered =
        lines_to_single_string(&output).replace(&path.display().to_string(), "<REPORT_PATH>");
    insta::assert_snapshot!("telemetry_report_completion", rendered);
}

#[tokio::test]
async fn report_failure_is_visible() {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    app.finish_report(Err("temporary directory is not writable".to_string()));
    let cell = match events.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected error history cell, got {other:?}"),
    };
    insta::assert_snapshot!(
        "telemetry_report_failure",
        lines_to_single_string(&cell.display_lines(/*width*/ 100))
    );
}

#[tokio::test]
async fn report_generation_emits_progress_then_completes() {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    app.generate_report();
    let cell = match events.recv().await {
        Some(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected progress before report result, got {other:?}"),
    };
    insta::assert_snapshot!(
        "telemetry_report_progress",
        lines_to_single_string(&cell.display_lines(/*width*/ 80))
    );
    let path = match events.recv().await {
        Some(AppEvent::ReportGenerated { result }) => result.expect("report generation"),
        other => panic!("expected report result, got {other:?}"),
    };
    assert_eq!(path.file_name().unwrap(), "report.html");
    assert!(path.is_file());
    std::fs::remove_dir_all(path.parent().unwrap()).expect("remove generated test report");
}
