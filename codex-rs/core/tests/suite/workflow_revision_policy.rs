//! New revision work obeys the original caller's current native permissions.

use anyhow::Context;
use anyhow::Result;
use codex_dynamic_workflows::RevisionApplyMode;
use codex_dynamic_workflows::RevisionApplyRequest;
use codex_dynamic_workflows::RevisionResumeRequest;
use codex_dynamic_workflows::RevisionStageRequest;
use codex_dynamic_workflows::WorkflowResult;
use codex_dynamic_workflows::WorkflowRevisionStatus;
use codex_protocol::ThreadId;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::ThreadSettingsOverrides;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;

use super::workflow_revision::builder;
use super::workflow_revision::observe;
use super::workflow_revision::source;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn appended_native_work_obeys_the_same_callers_refreshed_read_only_policy() -> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    core_test_support::skip_if_remote!(
        Ok(()),
        "the fixture verifies native command effects in its local selected cwd"
    );
    core_test_support::skip_if_target_windows!(
        Ok(()),
        "uses POSIX printf to prove actual allowed and denied native writes"
    );
    let provider = responses::start_mock_server().await;
    let model = responses::mount_sse_sequence(
        &provider,
        vec![
            responses::sse(vec![
                responses::ev_function_call(
                    "base-write",
                    "exec_command",
                    &json!({"cmd":"printf BASE > policy-base.txt","yield_time_ms":1000})
                        .to_string(),
                ),
                responses::ev_completed_with_tokens("base-write", /*total_tokens*/ 2),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("base-result", r#"{"base":"before_restriction"}"#),
                responses::ev_completed_with_tokens("base-result", /*total_tokens*/ 5),
            ]),
            responses::sse(vec![
                responses::ev_function_call(
                    "restricted-write",
                    "exec_command",
                    &json!({"cmd":"printf FORBIDDEN > policy-forbidden.txt","yield_time_ms":1000})
                        .to_string(),
                ),
                responses::ev_completed_with_tokens("restricted-write", /*total_tokens*/ 3),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("restricted-result", r#"{"writeDenied":true}"#),
                responses::ev_completed_with_tokens("restricted-result", /*total_tokens*/ 7),
            ]),
        ],
    )
    .await;
    let test = builder()
        .with_config(|config| {
            config
                .permissions
                .set_permission_profile(PermissionProfile::Disabled)
                .expect("allow the initial authorized native write");
            config.permissions.approval_policy =
                codex_config::Constrained::allow_any(AskForApproval::Never);
        })
        .build_with_auto_env(&provider)
        .await?;
    let prefix = "const base = await workflow.agent('Perform the initial authorized base write.',{key:'base',label:'base'});";
    let original_source = source(&format!("{prefix}\nreturn {{base}};"));
    let revised_source = source(&format!(
        "{prefix}\nconst suffix = await workflow.agent('Attempt the suffix write using the callers current restrictions.',{{key:'restricted',label:'restricted'}});\nreturn {{base,suffix}};"
    ));
    let started = test
        .codex
        .start_workflow(original_source, json!({}))
        .await
        .map_err(anyhow::Error::msg)?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let original = observe(&service, &started.id, |run| {
        run.status == codex_core::workflows::WorkflowStatus::Succeeded
            && run.lifecycle.terminal_ready
    })
    .await?;
    assert_eq!(
        std::fs::read_to_string(test.cwd.path().join("policy-base.txt"))?,
        "BASE"
    );
    let base = original
        .steps
        .values()
        .find(|step| step.label == "base")
        .context("the accepted broad-policy base")?;
    let broad_child = test
        .thread_manager
        .get_thread(ThreadId::from_string(
            base.attempts[0]
                .child_thread_id
                .as_deref()
                .context("the original native child")?,
        )?)
        .await?;
    assert_eq!(
        broad_child.config_snapshot().await.permission_profile,
        PermissionProfile::Disabled
    );
    test.codex
        .update_thread_settings(ThreadSettingsOverrides {
            permission_profile: Some(PermissionProfile::read_only()),
            approval_policy: Some(AskForApproval::Never),
            ..Default::default()
        })
        .await?;
    assert_eq!(
        test.codex.config_snapshot().await.permission_profile,
        PermissionProfile::read_only()
    );
    let current = test
        .codex
        .workflow_control_state(&started.id)
        .await
        .map_err(anyhow::Error::msg)?;
    let foreign = broad_child
        .stage_workflow_revision(
            &started.id,
            RevisionStageRequest {
                command_id: "broader-different-caller".into(),
                expected_control_revision: current.control_revision,
                source: revised_source.clone(),
                input: WorkflowResult::Absent,
            },
        )
        .await
        .expect_err(
            "a broader different native caller cannot take over the original launch authority",
        );
    assert!(
        foreign.contains("cross-caller"),
        "the rejection must be original-caller authority rather than an unrelated failure: {foreign}"
    );
    assert_eq!(
        test.codex
            .workflow_control_state(&started.id)
            .await
            .map_err(anyhow::Error::msg)?,
        current
    );
    let staged = test
        .codex
        .stage_workflow_revision(
            &started.id,
            RevisionStageRequest {
                command_id: "restricted-stage".into(),
                expected_control_revision: current.control_revision,
                source: revised_source.clone(),
                input: WorkflowResult::Absent,
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    assert!(staged.revision.reusable_statements >= 1);
    test.codex
        .apply_workflow_revision(
            &started.id,
            RevisionApplyRequest {
                command_id: "restricted-apply".into(),
                expected_control_revision: staged.state.control_revision,
                revision_id: staged.revision.id.clone(),
                mode: RevisionApplyMode::DrainAccepted,
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let ready = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
        loop {
            let state = test
                .codex
                .workflow_control_state(&started.id)
                .await
                .map_err(anyhow::Error::msg)?;
            if state.status == WorkflowRevisionStatus::Ready {
                break Ok::<_, anyhow::Error>(state);
            }
            ticks.tick().await;
        }
    })
    .await??;
    assert_eq!(model.requests().len(), 2);
    test.codex
        .resume_workflow_revision(
            &started.id,
            RevisionResumeRequest {
                command_id: "restricted-resume".into(),
                expected_control_revision: ready.control_revision,
                revision_id: staged.revision.id,
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let complete = observe(&service, &started.id, |run| {
        run.status == codex_core::workflows::WorkflowStatus::Succeeded
            && run.source == revised_source
            && run.lifecycle.terminal_ready
    })
    .await?;
    assert_eq!(
        complete.result,
        Some(json!({"base":{"base":"before_restriction"},"suffix":{"writeDenied":true}}))
    );
    assert_eq!(complete.accepted_starts, 2);
    assert_eq!(
        complete
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(17)
    );
    assert_eq!(
        complete.steps.values().find(|step| step.label == "base"),
        Some(base)
    );
    let suffix = complete
        .steps
        .values()
        .find(|step| step.label == "restricted")
        .context("the native suffix accepted after the policy refresh")?;
    let restricted = test
        .thread_manager
        .get_thread(ThreadId::from_string(
            suffix.attempts[0]
                .child_thread_id
                .as_deref()
                .context("the actual restricted native worker")?,
        )?)
        .await?;
    assert_eq!(
        restricted.config_snapshot().await.permission_profile,
        PermissionProfile::read_only()
    );
    let requests = model.requests();
    assert_eq!(requests.len(), 4);
    let denied = requests[3]
        .function_call_output("restricted-write")
        .to_string()
        .to_lowercase();
    assert!(
        denied.contains("denied")
            || denied.contains("not permitted")
            || denied.contains("read-only"),
        "the actual native suffix tool must reject the write under the refreshed policy: {denied}"
    );
    assert!(
        !test.cwd.path().join("policy-forbidden.txt").exists(),
        "the suffix must not reuse the original broader execution policy"
    );
    assert_eq!(
        std::fs::read_to_string(test.cwd.path().join("policy-base.txt"))?,
        "BASE"
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
