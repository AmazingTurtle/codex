//! Verify projectless startup permissions through the actual approval dispatcher.

use super::*;
use codex_app_server_client::AppServerEvent;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test]
async fn projectless_global_auto_review_reaches_guardian_for_elevation() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let source = tempfile::tempdir()?;
    let model = responses::start_mock_server().await;
    let response = responses::mount_sse_sequence(&model, vec![
        responses::sse(vec![
            responses::ev_response_created("parent"),
            responses::ev_function_call("elevated", "exec_command", &json!({
                "cmd": "pwd", "sandbox_permissions": "require_escalated",
                "justification": "check the workspace outside the sandbox"
            }).to_string()),
            responses::ev_completed("parent"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("guardian"),
            responses::ev_assistant_message("review", r#"{"risk_level":"high","user_authorization":"low","outcome":"deny","rationale":"The unsandboxed command is not authorized."}"#),
            responses::ev_completed("guardian"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("done"),
            responses::ev_assistant_message("answer", "done"),
            responses::ev_completed("done"),
        ]),
    ]).await;
    std::fs::write(
        app.config.codex_home.join("config.toml"),
        format!(
            r#"
model = "gpt-5.2"
model_provider = "test"
approvals_reviewer = "auto_review"
[model_providers.test]
name = "test"
base_url = "{}/v1"
wire_api = "responses"
supports_websockets = false
[features]
guardianv2 = false
enable_request_compression = false
[windows]
sandbox = "unelevated"
"#,
            model.uri()
        ),
    )?;
    app.config = app
        .rebuild_config_for_cwd(source.path().to_path_buf())
        .await?;
    app.chat_widget
        .handle_thread_session_quiet(test_thread_session(
            ThreadId::new(),
            source.path().to_path_buf(),
        ));
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    tui.pause_events();
    app.start_fresh_session(
        &mut tui,
        &mut server,
        /*session_start_source*/ None,
        /*initial_user_message*/ None,
        /*new_thread_name*/ None,
    )
    .await;
    assert_eq!(
        (
            app.config.permissions.approval_policy.value(),
            app.config.approvals_reviewer
        ),
        (
            codex_protocol::protocol::AskForApproval::OnRequest,
            ApprovalsReviewer::AutoReview
        )
    );
    while events.try_recv().is_ok() {}
    app.chat_widget.open_approvals_popup();
    let fetch = events.try_recv().expect("permission discovery request");
    app.handle_event(&mut tui, &mut server, fetch).await?;
    let loaded = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(event @ AppEvent::PermissionProfilesLoaded { .. }) = events.recv().await {
                break event;
            }
        }
    })
    .await?;
    app.handle_event(&mut tui, &mut server, loaded).await?;
    assert!(
        render_bottom_popup(&app.chat_widget, /*width*/ 120).contains("Approve for me (current)")
    );
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    while events.try_recv().is_ok() {}
    app.chat_widget
        .apply_external_edit("Check the workspace.".into());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let op = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::CodexOp(op @ AppCommand::UserTurn { .. }) => Some(op),
            _ => None,
        })
        .expect("user turn");
    let thread_id = app.chat_widget.thread_id().expect("started thread");
    assert!(
        app.try_submit_active_thread_op_via_app_server(&mut server, thread_id, &op)
            .await?
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match server.next_event().await.expect("connected server").event {
                AppServerEvent::ServerRequest(request) => {
                    assert!(
                        !matches!(
                            *request,
                            ServerRequest::CommandExecutionRequestApproval { .. }
                        ),
                        "elevation must be reviewed by Guardian"
                    );
                }
                AppServerEvent::ServerNotification(notification)
                    if matches!(*notification, ServerNotification::TurnCompleted(_)) =>
                {
                    break;
                }
                _ => {}
            }
        }
    })
    .await?;
    let requests = response.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests.iter().any(
            |request| request.body_json()["client_metadata"]["x-openai-subagent"] == "guardian"
        )
    );
    assert!(
        response
            .function_call_output_text("elevated")
            .expect("command output")
            .contains("not authorized")
    );
    server.shutdown().await?;
    Ok(())
}
