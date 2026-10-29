//! Real native effects and accepted ownership survive guarded workflow revisions.

use anyhow::Context;
use anyhow::Result;
use codex_core::workflows::DynamicWorkflowService;
use codex_core::workflows::WorkflowRun;
use codex_core::workflows::WorkflowStatus;
use codex_dynamic_workflows::RevisionApplyMode;
use codex_dynamic_workflows::RevisionApplyRequest;
use codex_dynamic_workflows::RevisionResumeRequest;
use codex_dynamic_workflows::RevisionStageRequest;
use codex_dynamic_workflows::WorkflowResult;
use codex_dynamic_workflows::WorkflowRevisionStatus;
use codex_features::Feature;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::MultiAgentVersion;
use core_test_support::responses;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use test_case::test_case;

pub(super) fn builder() -> TestCodexBuilder {
    test_codex()
        .with_model("gpt-5.6-sol")
        .with_model_info_override("gpt-5.6-sol", |model| {
            model.tool_mode = Some(ToolMode::Direct);
            model.multi_agent_version = Some(MultiAgentVersion::V2);
        })
        .with_config(|config| {
            for feature in [
                Feature::Collab,
                Feature::MultiAgentV2,
                Feature::CodeModeHost,
                Feature::DynamicWorkflows,
            ] {
                config
                    .features
                    .enable(feature)
                    .expect("enable real keyed workflow revisions");
            }
            config.multi_agent_v2.non_code_mode_only = false;
            config.multi_agent_v2.max_concurrent_threads_per_session = 4;
            config.dynamic_workflows.max_agents = 2;
            config
                .features
                .disable(Feature::WakeOnAgentMessages)
                .expect("display-only results stay outside model turns");
        })
}

pub(super) fn source(body: &str) -> String {
    format!(
        "workflow.meta({{name:'native-revision',description:'Retain accepted work while appending a plan',phases:[]}});\n{body}"
    )
}

fn answer(id: &str, value: Value, tokens: i64) -> String {
    responses::sse(vec![
        responses::ev_assistant_message(id, &value.to_string()),
        responses::ev_completed_with_tokens(id, tokens),
    ])
}

pub(super) async fn observe(
    service: &DynamicWorkflowService,
    id: &str,
    expected: impl Fn(&WorkflowRun) -> bool,
) -> Result<WorkflowRun> {
    let mut events = service.subscribe();
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        loop {
            let run = service.get(id).map_err(anyhow::Error::msg)?;
            if expected(&run) {
                return Ok(run);
            }
            match events.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn appending_a_keyed_agent_reuses_the_native_prefix_and_retains_its_cumulative_budget()
-> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    core_test_support::skip_if_remote!(
        Ok(()),
        "the observable append-only file effects are read from the host fixture cwd"
    );
    core_test_support::skip_if_target_windows!(
        Ok(()),
        "uses POSIX printf to count real native tool effects"
    );
    let server = responses::start_mock_server().await;
    let model=responses::mount_sse_sequence(&server,vec![
        responses::sse(vec![responses::ev_function_call("base-effect","exec_command",&json!({"cmd":"printf 'base\\n' >> revision-effects.txt","yield_time_ms":1000}).to_string()),responses::ev_completed_with_tokens("base-tool",/*total_tokens*/ 2)]),
        answer("base-result",json!({"base":"original"}),/*tokens*/ 5),
        responses::sse(vec![responses::ev_function_call("extra-effect","exec_command",&json!({"cmd":"printf 'extra\\n' >> revision-effects.txt","yield_time_ms":1000}).to_string()),responses::ev_completed_with_tokens("extra-tool",/*total_tokens*/ 3)]),
        answer("extra-result",json!({"extra":"appended"}),/*tokens*/ 11),
    ]).await;
    let test = builder()
        .with_config(|config| {
            config
                .permissions
                .set_permission_profile(PermissionProfile::Disabled)
                .expect("allow the authorized fixture file effects")
        })
        .build_with_auto_env(&server)
        .await?;
    let prefix = "const base = await workflow.agent('Perform the base append-only effect.', {key:'base',label:'base'});";
    let original_source = source(&format!("{prefix}\nreturn {{base}};"));
    let revised_source = source(&format!(
        "{prefix}\nconst extra = await workflow.agent('Perform the new appended effect.', {{key:'extra',label:'extra'}});\nreturn {{base,extra}};"
    ));
    let started = test
        .codex
        .start_workflow(original_source.clone(), json!({"retained":true}))
        .await
        .map_err(anyhow::Error::msg)?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let original = observe(&service, &started.id, |run| {
        run.status == WorkflowStatus::Succeeded && run.lifecycle.terminal_ready
    })
    .await?;
    assert_eq!(original.result, Some(json!({"base":{"base":"original"}})));
    assert_eq!(original.accepted_starts, 1);
    assert_eq!(
        original
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(7)
    );
    assert_eq!(
        std::fs::read_to_string(test.cwd.path().join("revision-effects.txt"))?,
        "base\n"
    );
    let original_step = original
        .steps
        .values()
        .find(|step| step.label == "base")
        .context("the accepted keyed base")?
        .clone();
    let original_control = test
        .codex
        .workflow_control_state(&started.id)
        .await
        .map_err(anyhow::Error::msg)?;
    let staged = test
        .codex
        .stage_workflow_revision(
            &started.id,
            RevisionStageRequest {
                command_id: "append-source".into(),
                expected_control_revision: original_control.control_revision,
                source: revised_source.clone(),
                input: WorkflowResult::Absent,
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(staged.revision.status, WorkflowRevisionStatus::Draft);
    assert!(staged.revision.reusable_statements >= 1);
    assert_eq!(
        model.requests().len(),
        2,
        "staging a source proposal must not run its suffix"
    );
    let apply = tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        test.codex.apply_workflow_revision(
            &started.id,
            RevisionApplyRequest {
                command_id: "append-apply".into(),
                expected_control_revision: staged.state.control_revision,
                revision_id: staged.revision.id.clone(),
                mode: RevisionApplyMode::DrainAccepted,
            },
        ),
    )
    .await?
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
    assert_eq!(
        ready.pending_revision_id.as_deref(),
        Some(apply.revision.id.as_str())
    );
    assert_eq!(
        model.requests().len(),
        2,
        "a ready revision must await explicit resume before accepting new work"
    );
    test.codex
        .resume_workflow_revision(
            &started.id,
            RevisionResumeRequest {
                command_id: "append-resume".into(),
                expected_control_revision: ready.control_revision,
                revision_id: staged.revision.id.clone(),
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let final_run = observe(&service, &started.id, |run| {
        run.status == WorkflowStatus::Succeeded
            && run.lifecycle.terminal_ready
            && run.source == revised_source
    })
    .await?;
    assert_eq!(
        final_run.result,
        Some(json!({"base":{"base":"original"},"extra":{"extra":"appended"}}))
    );
    assert_eq!(
        final_run.accepted_starts, 2,
        "the successful prefix must not consume a third global native start"
    );
    assert_eq!(
        final_run
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(21)
    );
    assert_eq!(
        final_run.steps.values().find(|step| step.label == "base"),
        Some(&original_step)
    );
    assert_eq!(
        std::fs::read_to_string(test.cwd.path().join("revision-effects.txt"))?,
        "base\nextra\n",
        "each observable native file effect must happen exactly once"
    );
    let revisions = test
        .codex
        .workflow_revisions(&started.id, /*offset*/ 0, /*limit*/ 50)
        .await
        .map_err(anyhow::Error::msg)?;
    assert!(
        revisions
            .iter()
            .any(|revision| revision.source == original_source
                && revision.input == json!({"retained":true}))
    );
    let snapshot = final_run
        .lifecycle
        .snapshots
        .get(&original.execution_id)
        .context("the immutable original execution result")?;
    assert_eq!(
        (&snapshot.source, &snapshot.result),
        (
            &original_source,
            &WorkflowResult::Value {
                value: json!({"base":{"base":"original"}})
            }
        )
    );
    assert_eq!(model.requests().len(), 4);
    assert_eq!(
        model.requests()[1].function_call_output("base-effect")["call_id"],
        json!("base-effect")
    );
    assert_eq!(
        model.requests()[3].function_call_output("extra-effect")["call_id"],
        json!("extra-effect")
    );
    let capped_source=revised_source.replace("return {base,extra};","const third = await workflow.agent('This exceeds the shared native start limit.',{key:'third',label:'third'});\nreturn {base,extra,third};");
    let current = test
        .codex
        .workflow_control_state(&started.id)
        .await
        .map_err(anyhow::Error::msg)?;
    let capped = test
        .codex
        .stage_workflow_revision(
            &started.id,
            RevisionStageRequest {
                command_id: "budget-stage".into(),
                expected_control_revision: current.control_revision,
                source: capped_source.clone(),
                input: WorkflowResult::Absent,
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    test.codex
        .apply_workflow_revision(
            &started.id,
            RevisionApplyRequest {
                command_id: "budget-apply".into(),
                expected_control_revision: capped.state.control_revision,
                revision_id: capped.revision.id.clone(),
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
    test.codex
        .resume_workflow_revision(
            &started.id,
            RevisionResumeRequest {
                command_id: "budget-resume".into(),
                expected_control_revision: ready.control_revision,
                revision_id: capped.revision.id,
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let exhausted = observe(&service, &started.id, |run| {
        run.source == capped_source
            && run.status == WorkflowStatus::Failed
            && run.lifecycle.terminal_ready
    })
    .await?;
    assert_eq!(
        exhausted.accepted_starts, 2,
        "a revision must retain the starts charged by earlier accepted work"
    );
    assert!(
        exhausted
            .error
            .as_deref()
            .is_some_and(|error| error.contains("agent") && error.contains("limit")),
        "{:?}",
        exhausted.error
    );
    assert_eq!(
        exhausted
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(21)
    );
    assert_eq!(
        model.requests().len(),
        4,
        "the shared limit must reject before a third native provider request"
    );
    assert_eq!(
        std::fs::read_to_string(test.cwd.path().join("revision-effects.txt"))?,
        "base\nextra\n"
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn draining_a_revision_settles_accepted_work_without_starting_the_suffix_or_cancelling_a_sibling()
-> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let (release_a, gate_a) = tokio::sync::oneshot::channel();
    let (release_b, gate_b) = tokio::sync::oneshot::channel();
    let (provider, _) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("accepted-A")]),
            },
            StreamingSseChunk {
                gate: Some(gate_a),
                body: answer("accepted-A", json!({"base":"drained"}), /*tokens*/ 3),
            },
        ],
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("sibling-B")]),
            },
            StreamingSseChunk {
                gate: Some(gate_b),
                body: answer("sibling-B", json!({"sibling":true}), /*tokens*/ 5),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: answer("suffix-A", json!({"extra":"new"}), /*tokens*/ 7),
        }],
    ])
    .await;
    let bootstrap = responses::start_mock_server().await;
    let base_url = format!("{}/v1", provider.uri());
    let test = builder()
        .with_config(move |config| config.model_provider.base_url = Some(base_url))
        .build_with_auto_env(&bootstrap)
        .await?;
    let prefix = "const base = await workflow.agent('Hold the accepted original A worker.', {key:'base',label:'base'});";
    let original_source = source(&format!("{prefix}\nreturn {{base}};"));
    let revised_source = source(&format!(
        "{prefix}\nconst extra = await workflow.agent('Run the new suffix only after explicit resume.', {{key:'extra',label:'extra'}});\nreturn {{base,extra}};"
    ));
    let a = test
        .codex
        .start_workflow(original_source, json!({}))
        .await
        .map_err(anyhow::Error::msg)?;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        provider.wait_for_request_count(/*count*/ 1),
    )
    .await?;
    let b=test.codex.start_workflow(source("return await workflow.agent('Keep unrelated sibling B active.',{key:'sibling',label:'sibling'});"),json!({})).await.map_err(anyhow::Error::msg)?;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        provider.wait_for_request_count(/*count*/ 2),
    )
    .await?;
    let state = test
        .codex
        .workflow_control_state(&a.id)
        .await
        .map_err(anyhow::Error::msg)?;
    let staged = test
        .codex
        .stage_workflow_revision(
            &a.id,
            RevisionStageRequest {
                command_id: "drain-stage".into(),
                expected_control_revision: state.control_revision,
                source: revised_source.clone(),
                input: WorkflowResult::Absent,
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let applying = tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        test.codex.apply_workflow_revision(
            &a.id,
            RevisionApplyRequest {
                command_id: "drain-apply".into(),
                expected_control_revision: staged.state.control_revision,
                revision_id: staged.revision.id.clone(),
                mode: RevisionApplyMode::DrainAccepted,
            },
        ),
    )
    .await?
    .map_err(anyhow::Error::msg)?;
    assert!(matches!(
        applying.state.status,
        WorkflowRevisionStatus::Fencing | WorkflowRevisionStatus::Draining
    ));
    assert_eq!(provider.requests().await.len(), 2);
    release_b
        .send(())
        .expect("the unrelated sibling remains usable while A drains");
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let sibling = observe(&service, &b.id, |run| {
        run.status == WorkflowStatus::Succeeded && run.lifecycle.terminal_ready
    })
    .await?;
    assert_eq!(sibling.result, Some(json!({"sibling":true})));
    assert_eq!(
        provider.requests().await.len(),
        2,
        "drain fences fresh suffix work while the accepted A worker is still open"
    );
    release_a
        .send(())
        .expect("allow only the already accepted A task to settle");
    let ready = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
        loop {
            let state = test
                .codex
                .workflow_control_state(&a.id)
                .await
                .map_err(anyhow::Error::msg)?;
            if state.status == WorkflowRevisionStatus::Ready {
                break Ok::<_, anyhow::Error>(state);
            }
            ticks.tick().await;
        }
    })
    .await??;
    assert_eq!(
        provider.requests().await.len(),
        2,
        "settling accepted work must not itself dispatch the revised suffix"
    );
    test.codex
        .resume_workflow_revision(
            &a.id,
            RevisionResumeRequest {
                command_id: "drain-resume".into(),
                expected_control_revision: ready.control_revision,
                revision_id: staged.revision.id,
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let completed = observe(&service, &a.id, |run| {
        run.status == WorkflowStatus::Succeeded
            && run.lifecycle.terminal_ready
            && run.source == revised_source
    })
    .await?;
    assert_eq!(
        completed.result,
        Some(json!({"base":{"base":"drained"},"extra":{"extra":"new"}}))
    );
    assert_eq!(completed.accepted_starts, 2);
    assert_eq!(
        completed
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(10)
    );
    assert_eq!(provider.requests().await.len(), 3);
    assert_eq!(
        service.get(&b.id).map_err(anyhow::Error::msg)?.result,
        Some(json!({"sibling":true}))
    );
    assert!(
        responses::received_responses_requests(&bootstrap)
            .await
            .is_empty()
    );
    test.codex.shutdown_and_wait().await?;
    provider.shutdown().await;
    Ok(())
}

#[derive(Clone, Copy)]
enum QueuedChange {
    SkipNull,
    SkipValidatedValue,
    EditPrompt,
}

#[test_case(QueuedChange::SkipNull; "skip_queued_defaults_to_null_without_a_native_charge")]
#[test_case(QueuedChange::SkipValidatedValue; "skip_queued_uses_the_validated_replacement_without_a_native_charge")]
#[test_case(QueuedChange::EditPrompt; "edited_queued_work_uses_the_new_prompt_after_capacity_is_released")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_controls_cancel_only_preaccepted_work_and_keep_command_identity(
    change: QueuedChange,
) -> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    use codex_dynamic_workflows::WorkflowControlOperation;
    use codex_dynamic_workflows::WorkflowControlRequest;
    use codex_dynamic_workflows::WorkflowControlStatus;
    use codex_dynamic_workflows::WorkflowControlTarget;
    let held_result = if matches!(change, QueuedChange::SkipValidatedValue) {
        json!({"skipped":true})
    } else {
        json!({"held":true})
    };
    let (release, gate) = tokio::sync::oneshot::channel();
    let mut streams = vec![vec![
        StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![responses::ev_response_created("capacity-holder")]),
        },
        StreamingSseChunk {
            gate: Some(gate),
            body: answer("capacity-holder", held_result.clone(), /*tokens*/ 3),
        },
    ]];
    if matches!(change, QueuedChange::EditPrompt) {
        streams.push(vec![StreamingSseChunk {
            gate: None,
            body: answer("edited-result", json!({"edited":true}), /*tokens*/ 5),
        }]);
    }
    let (provider, _) = start_streaming_sse_server(streams).await;
    let bootstrap = responses::start_mock_server().await;
    let base_url = format!("{}/v1", provider.uri());
    let test = builder()
        .with_config(move |config| {
            config.model_provider.base_url = Some(base_url);
            config.multi_agent_v2.max_concurrent_threads_per_session = 2;
        })
        .build_with_auto_env(&bootstrap)
        .await?;
    let schema=match change {
        QueuedChange::SkipValidatedValue=>",schema:{type:'object',properties:{skipped:{const:true}},required:['skipped'],additionalProperties:false}".to_owned(),
        QueuedChange::EditPrompt=>format!(",schema:{}",json!({"type":"object","description":"schema annotation ".repeat(530)})),
        QueuedChange::SkipNull=>String::new(),
    };
    let started=test.codex.start_workflow(source(&format!(
        "return await workflow.parallel([()=>workflow.agent('Keep the accepted native slot occupied.',{{key:'held',label:'held'{schema}}}),()=>workflow.agent('ORIGINAL_QUEUED_PROMPT',{{key:'queued',label:'queued'{schema}}})]);"
    )),json!({})).await.map_err(anyhow::Error::msg)?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let snapshot = || {
        service.get(&started.id).map(|run| {
            let attempts: Vec<_> = run
                .steps
                .values()
                .map(|step| {
                    (
                        step.label.clone(),
                        step.attempts.last().map(|attempt| {
                            (
                                attempt.status,
                                attempt.admission,
                                attempt.child_thread_id.is_some(),
                            )
                        }),
                    )
                })
                .collect();
            (
                run.status,
                run.accepted_starts,
                run.lifecycle.terminal_ready,
                attempts,
            )
        })
    };
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        provider.wait_for_request_count(/*count*/ 1),
    )
    .await
    .with_context(|| {
        format!(
            "waiting for the first native request; state: {:?}",
            snapshot()
        )
    })?;
    // Parallel callbacks can acquire native capacity in either authored order.
    let reserved = observe(&service, &started.id, |run| {
        run.accepted_starts == 1
            && run.steps.values().any(|step| {
                step.attempts.last().is_some_and(|attempt| {
                    attempt.admission == codex_core::workflows::AdmissionStatus::Reserved
                        && attempt.child_thread_id.is_none()
                })
            })
    })
    .await
    .with_context(|| {
        format!(
            "waiting for unaccepted native work; state: {:?}",
            snapshot()
        )
    })?;
    let waiting = reserved
        .steps
        .values()
        .find(|step| {
            step.attempts.last().is_some_and(|attempt| {
                attempt.admission == codex_core::workflows::AdmissionStatus::Reserved
                    && attempt.child_thread_id.is_none()
            })
        })
        .context("the exact unaccepted native reservation")?;
    let waiting_index = match waiting.label.as_str() {
        "held" => 0,
        "queued" => 1,
        other => anyhow::bail!("unexpected reserved step: {other}"),
    };
    let original_prompt = waiting.options["message"]
        .as_str()
        .context("the reserved task prompt")?;
    let target = WorkflowControlTarget::Step {
        logical_id: waiting.logical_id.clone(),
        expected_attempt_id: Some(
            waiting
                .attempts
                .last()
                .context("the reserved attempt")?
                .id
                .clone(),
        ),
        expected_turn_id: None,
    };
    let mut state = test
        .codex
        .workflow_control_state(&started.id)
        .await
        .map_err(anyhow::Error::msg)?;
    if matches!(change, QueuedChange::EditPrompt) {
        let rejected = test
            .codex
            .workflow_control(
                &started.id,
                WorkflowControlRequest {
                    command_id: "oversized-combined-edit".into(),
                    expected_control_revision: state.control_revision,
                    target: target.clone(),
                    operation: WorkflowControlOperation::EditQueued {
                        prompt: "p".repeat(8192),
                    },
                },
            )
            .await
            .map_err(anyhow::Error::msg)?;
        assert_eq!(rejected.command.status, WorkflowControlStatus::Rejected);
        assert!(
            rejected
                .command
                .error
                .as_deref()
                .is_some_and(|error| error.contains("16384"))
        );
        let unchanged = service.get(&started.id).map_err(anyhow::Error::msg)?;
        assert_eq!(
            unchanged.steps.get(&waiting.id),
            Some(waiting),
            "an oversized combined replacement must be rejected before cancelling the existing native reservation"
        );
        assert_eq!(provider.requests().await.len(), 1);
        state = rejected.state;
    }
    let operation = match change {
        QueuedChange::SkipNull => WorkflowControlOperation::Skip {
            replacement: WorkflowResult::Absent,
        },
        QueuedChange::SkipValidatedValue => WorkflowControlOperation::Skip {
            replacement: WorkflowResult::Value {
                value: json!({"skipped":true}),
            },
        },
        QueuedChange::EditPrompt => WorkflowControlOperation::EditQueued {
            prompt: "EDITED_QUEUED_PROMPT".into(),
        },
    };
    let command = WorkflowControlRequest {
        command_id: "queued-change".into(),
        expected_control_revision: state.control_revision,
        target,
        operation,
    };
    let applied = test
        .codex
        .workflow_control(&started.id, command.clone())
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(applied.command.status, WorkflowControlStatus::Applied);
    let repeated = test
        .codex
        .workflow_control(&started.id, command.clone())
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        repeated, applied,
        "replaying the exact command must retain its durable disposition and control revision"
    );
    let mut conflicting = command.clone();
    conflicting.operation = WorkflowControlOperation::EditQueued {
        prompt: "CONFLICTING_REUSED_COMMAND_PAYLOAD".into(),
    };
    assert!(
        test.codex
            .workflow_control(&started.id, conflicting)
            .await
            .is_err(),
        "the same command ID cannot silently acquire a different mutation payload"
    );
    let mut stale = command;
    stale.command_id = "stale-revision".into();
    assert!(
        test.codex
            .workflow_control(&started.id, stale)
            .await
            .is_err(),
        "a new command cannot use the revision preceding the accepted mutation"
    );
    assert_eq!(
        test.codex
            .workflow_controls(&started.id, /*offset*/ 0, /*limit*/ 50)
            .await
            .map_err(anyhow::Error::msg)?
            .len(),
        if matches!(change, QueuedChange::EditPrompt) {
            2
        } else {
            1
        }
    );
    assert_eq!(
        provider.requests().await.len(),
        1,
        "editing or skipping queued work must not bypass the occupied native slot"
    );
    release
        .send(())
        .expect("finish only the already accepted native worker");
    let complete = observe(&service, &started.id, |run| {
        run.status == WorkflowStatus::Succeeded && run.lifecycle.terminal_ready
    })
    .await
    .with_context(|| {
        format!(
            "waiting for controlled work to settle; state: {:?}",
            snapshot()
        )
    })?;
    let (replacement, starts, tokens) = match change {
        QueuedChange::SkipNull => (Value::Null, 1, 3),
        QueuedChange::SkipValidatedValue => (json!({"skipped":true}), 1, 3),
        QueuedChange::EditPrompt => (json!({"edited":true}), 2, 8),
    };
    let mut expected = vec![held_result; 2];
    expected[waiting_index] = replacement;
    assert_eq!(complete.result, Some(Value::Array(expected)));
    assert_eq!(complete.accepted_starts, starts);
    assert_eq!(
        complete
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(tokens)
    );
    let requests = provider.requests().await;
    assert_eq!(requests.len(), usize::try_from(starts)?);
    if matches!(change, QueuedChange::EditPrompt) {
        let edited: Value = serde_json::from_slice(&requests[1])?;
        assert!(edited["input"].to_string().contains("EDITED_QUEUED_PROMPT"));
        assert!(!edited["input"].to_string().contains(original_prompt));
    } else {
        assert!(
            complete
                .steps
                .values()
                .find(|step| step.logical_id == waiting.logical_id)
                .context("retained skipped work")?
                .attempts
                .iter()
                .all(|attempt| attempt.child_thread_id.is_none())
        );
    }
    assert!(
        responses::received_responses_requests(&bootstrap)
            .await
            .is_empty()
    );
    test.codex.shutdown_and_wait().await?;
    provider.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paused_guidance_retains_the_accepted_attempt_and_original_caller_without_waking_inference()
-> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    use codex_dynamic_workflows::WorkflowControlOperation;
    use codex_dynamic_workflows::WorkflowControlRequest;
    use codex_dynamic_workflows::WorkflowControlStatus;
    use codex_dynamic_workflows::WorkflowControlTarget;
    use codex_protocol::ThreadId;
    use codex_protocol::protocol::EventMsg;
    use codex_protocol::protocol::ExecutionControlState;
    use codex_protocol::turn_input::TurnInputRequest;
    use codex_protocol::turn_input::TurnInputSubmission;
    use codex_protocol::user_input::UserInput;
    let (release, gate) = tokio::sync::oneshot::channel();
    let (release_human, human_gate) = tokio::sync::oneshot::channel();
    let (provider, _) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("original-owned-turn")]),
            },
            StreamingSseChunk {
                gate: Some(gate),
                body: answer(
                    "original-owned-turn",
                    json!({"original":true}),
                    /*tokens*/ 2,
                ),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_assistant_message("unrelated-root", "INDEPENDENT_ROOT_CONTINUES"),
                responses::ev_completed("unrelated-root"),
            ]),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: answer("guided-result", json!({"guided":true}), /*tokens*/ 5),
        }],
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("independent-human")]),
            },
            StreamingSseChunk {
                gate: Some(human_gate),
                body: responses::sse(vec![
                    responses::ev_assistant_message("independent-human", "LATER_HUMAN_SURVIVES"),
                    responses::ev_completed("independent-human"),
                ]),
            },
        ],
    ])
    .await;
    let bootstrap = responses::start_mock_server().await;
    let base_url = format!("{}/v1", provider.uri());
    let test = builder()
        .with_config(move |config| config.model_provider.base_url = Some(base_url))
        .build_with_auto_env(&bootstrap)
        .await?;
    let started=test.codex.start_workflow(source("return await workflow.agent('Wait for targeted guidance.',{key:'guided',label:'guided'});"),json!({})).await.map_err(anyhow::Error::msg)?;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        provider.wait_for_request_count(/*count*/ 1),
    )
    .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let active = observe(&service, &started.id, |run| {
        run.steps.values().any(|step| {
            step.attempts
                .last()
                .is_some_and(|attempt| attempt.child_thread_id.is_some())
        })
    })
    .await?;
    let step = active
        .steps
        .values()
        .find(|step| step.label == "guided")
        .context("the accepted guided step")?;
    let attempt = step
        .attempts
        .last()
        .context("the accepted native attempt")?;
    let child = test
        .thread_manager
        .get_thread(ThreadId::from_string(
            attempt
                .child_thread_id
                .as_deref()
                .context("the native worker identity")?,
        )?)
        .await?;
    let owned = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
        loop {
            if let Some(turn) = test
                .codex
                .active_workflow_owned_turn(&started.id, &step.logical_id, &attempt.id)
                .await
                .map_err(anyhow::Error::msg)?
            {
                break Ok::<_, anyhow::Error>(turn);
            }
            ticks.tick().await;
        }
    })
    .await??;
    let state = test
        .codex
        .workflow_control_state(&started.id)
        .await
        .map_err(anyhow::Error::msg)?;
    let request = WorkflowControlRequest {
        command_id: "paused-guide".into(),
        expected_control_revision: state.control_revision,
        target: WorkflowControlTarget::Step {
            logical_id: step.logical_id.clone(),
            expected_attempt_id: Some(attempt.id.clone()),
            expected_turn_id: Some(owned),
        },
        operation: WorkflowControlOperation::Guide {
            message: "TARGETED_GUIDANCE_PRIVATE".into(),
        },
    };
    assert!(
        child
            .workflow_control(&started.id, request.clone())
            .await
            .is_err(),
        "a native child cannot acquire the root launcher's mutation authority"
    );
    assert_eq!(
        test.codex
            .workflow_control_state(&started.id)
            .await
            .map_err(anyhow::Error::msg)?,
        state
    );
    assert_eq!(
        test.codex.pause_execution().await?.state,
        ExecutionControlState::Paused
    );
    let guided = test
        .codex
        .workflow_control(&started.id, request)
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(guided.command.status, WorkflowControlStatus::Applied);
    assert_eq!(
        test.codex.execution_state().await?.state,
        ExecutionControlState::Paused
    );
    assert_eq!(
        provider.requests().await.len(),
        1,
        "Agent-origin guidance cannot reopen the global inference gate"
    );
    let independent = test
        .thread_manager
        .start_thread(test.start_thread_options().await)
        .await?;
    independent
        .thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Keep the independent root working.".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        core_test_support::wait_for_event(&independent.thread, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        }),
    )
    .await?;
    assert_eq!(provider.requests().await.len(), 2);
    assert_eq!(
        test.codex.execution_state().await?.state,
        ExecutionControlState::Paused
    );
    let _ = release.send(());
    test.codex.unpause_execution().await?;
    let final_run = observe(&service, &started.id, |run| {
        run.status == WorkflowStatus::Succeeded && run.lifecycle.terminal_ready
    })
    .await?;
    assert_eq!(final_run.result, Some(json!({"guided":true})));
    assert_eq!(final_run.accepted_starts, 1);
    let final_step = final_run
        .steps
        .values()
        .find(|step| step.label == "guided")
        .context("the same guided logical step")?;
    assert_eq!(final_step.attempts.len(), 1);
    assert_eq!(final_step.attempts[0].id, attempt.id);
    assert_eq!(
        final_step.attempts[0].child_thread_id,
        attempt.child_thread_id
    );
    assert_eq!(
        test.codex
            .active_workflow_owned_turn(&started.id, &step.logical_id, &attempt.id)
            .await
            .map_err(anyhow::Error::msg)?,
        None
    );
    let requests = provider.requests().await;
    assert_eq!(requests.len(), 3);
    let guided_request: Value = serde_json::from_slice(&requests[2])?;
    let independent_request: Value = serde_json::from_slice(&requests[1])?;
    assert!(
        guided_request["input"]
            .to_string()
            .contains("TARGETED_GUIDANCE_PRIVATE")
    );
    assert!(
        !independent_request["input"]
            .to_string()
            .contains("TARGETED_GUIDANCE_PRIVATE")
    );
    let later = child
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Start a later independent human task.".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let TurnInputSubmission::Started {
        turn_id: later_turn,
        ..
    } = later
    else {
        anyhow::bail!("the independent human task must start a new turn: {later:?}");
    };
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        provider.wait_for_request_count(/*count*/ 4),
    )
    .await?;
    assert_eq!(
        test.codex
            .active_workflow_owned_turn(&started.id, &step.logical_id, &attempt.id)
            .await
            .map_err(anyhow::Error::msg)?,
        None
    );
    let stale = WorkflowControlRequest {
        command_id: "stale-old-turn".into(),
        expected_control_revision: guided.state.control_revision,
        target: guided.command.target.clone(),
        operation: WorkflowControlOperation::Interrupt,
    };
    let rejected = test
        .codex
        .workflow_control(&started.id, stale.clone())
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(rejected.command.status, WorkflowControlStatus::Rejected);
    assert!(rejected.command.error.is_some());
    assert_eq!(
        test.codex
            .workflow_control(&started.id, stale)
            .await
            .map_err(anyhow::Error::msg)?,
        rejected
    );
    let mut wrong_attempt = rejected.command.target.clone();
    if let WorkflowControlTarget::Step {
        expected_attempt_id,
        ..
    } = &mut wrong_attempt
    {
        *expected_attempt_id = Some(uuid::Uuid::new_v4().to_string());
    }
    let rejected_attempt = test
        .codex
        .workflow_control(
            &started.id,
            WorkflowControlRequest {
                command_id: "stale-old-attempt".into(),
                expected_control_revision: rejected.state.control_revision,
                target: wrong_attempt,
                operation: WorkflowControlOperation::Interrupt,
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        rejected_attempt.command.status,
        WorkflowControlStatus::Rejected
    );
    assert!(rejected_attempt.command.error.is_some());
    release_human
        .send(())
        .expect("the stale workflow controls leave the independent human stream open");
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30),core_test_support::wait_for_event(&child,|event|
        matches!(event,EventMsg::TurnComplete(done) if done.turn_id==later_turn && done.error.is_none()
            && done.last_agent_message.as_deref()==Some("LATER_HUMAN_SURVIVES"))
    )).await?;
    assert_eq!(provider.requests().await.len(), 4);
    let still_original = service.get(&started.id).map_err(anyhow::Error::msg)?;
    assert_eq!(
        (still_original.result, still_original.accepted_starts),
        (Some(json!({"guided":true})), 1)
    );
    test.codex.shutdown_and_wait().await?;
    independent.thread.shutdown_and_wait().await?;
    provider.shutdown().await;
    Ok(())
}
