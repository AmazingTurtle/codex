//! V2 scripts cross the real JavaScript, native child, and persistence boundaries.

use anyhow::Context;
use anyhow::Result;
use codex_core::workflows::DynamicWorkflowService;
use codex_core::workflows::WorkflowRun;
use codex_core::workflows::WorkflowScope;
use codex_core::workflows::WorkflowStatus;
use codex_features::Feature;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::MultiAgentVersion;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_remote;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use test_case::test_case;

#[path = "workflow_skill_examples.rs"]
mod workflow_skill_examples;

fn builder() -> TestCodexBuilder {
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
                    .expect("enable native V2 workflow integration");
            }
            config.multi_agent_v2.non_code_mode_only = false;
            config.multi_agent_v2.max_concurrent_threads_per_session = 4;
            config
                .features
                .disable(Feature::WakeOnAgentMessages)
                .expect("API launches do not wake model turns");
        })
}

pub(super) fn script(body: &str) -> String {
    script_with_phases(&[], body)
}

pub(super) fn script_with_phases(phases: &[&str], body: &str) -> String {
    named_script("core_integration", phases, body)
}

fn named_script(name: &str, phases: &[&str], body: &str) -> String {
    let metadata = json!({
        "name": name,
        "description": "Exercise the production workflow service and native child lifecycle.",
        "phases": phases.iter().map(|title| json!({"title": title})).collect::<Vec<_>>(),
    });
    format!("workflow.meta({metadata});\n{body}")
}

fn answer(id: &str, text: &str, tokens: i64) -> String {
    responses::sse(vec![
        responses::ev_response_created(id),
        responses::ev_assistant_message(id, text),
        responses::ev_completed_with_tokens(id, tokens),
    ])
}

pub(super) async fn observe(
    service: &DynamicWorkflowService,
    id: &str,
    expected: impl Fn(&WorkflowRun) -> bool,
) -> Result<WorkflowRun> {
    let mut events = service.subscribe();
    let mut progress = service.subscribe_progress();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let run = service.get(id).map_err(anyhow::Error::msg)?;
            if expected(&run) {
                return Ok(run);
            }
            anyhow::ensure!(
                !matches!(
                    run.status,
                    WorkflowStatus::Failed | WorkflowStatus::Stopped | WorkflowStatus::Interrupted
                ),
                "workflow ended before the expected condition: {:?}",
                run.error
            );
            let received = tokio::select! {
                event = events.recv() => event.map(|_| ()),
                update = progress.recv() => update.map(|_| ()),
            };
            match received {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_native_children_keep_authored_order_when_completion_order_reverses() -> Result<()>
{
    skip_if_no_network!(Ok(()));
    let (release, gate) = tokio::sync::oneshot::channel();
    let (provider, _) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("held")]),
            },
            StreamingSseChunk {
                gate: Some(gate),
                body: responses::sse(vec![
                    responses::ev_assistant_message("held-result", r#""held""#),
                    responses::ev_completed_with_tokens("held", /*total_tokens*/ 7),
                ]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: answer("fast-result", r#""fast""#, /*tokens*/ 11),
        }],
    ])
    .await;
    let bootstrap = responses::start_mock_server().await;
    let base_url = format!("{}/v1", provider.uri());
    let test = builder()
        .with_config(move |config| {
            config.model_provider.base_url = Some(base_url);
            config.model_provider.supports_websockets = false;
            config.dynamic_workflows.max_tokens = std::num::NonZeroU64::new(100);
        })
        .build_with_auto_env(&bootstrap)
        .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let args = json!({"items":["alpha","beta"],"context":{"retained":true}});
    let run = service.start(script_with_phases(&["Review"], r#"
workflow.phase("Review");
workflow.log("Review both requested inputs.");
const values = await workflow.parallel(workflow.args.items.map(label => () => workflow.agent("parallel-" + label, {label, phase:"Review"})));
return {values, args:workflow.args, budget:{total:workflow.budget.total, spent:await workflow.budget.spent(), remaining:await workflow.budget.remaining()}};
"#), args.clone()).await.map_err(anyhow::Error::msg)?;
    tokio::time::timeout(
        Duration::from_secs(30),
        provider.wait_for_request_count(/*count*/ 2),
    )
    .await?;
    let requests = provider.requests().await;
    let held: Value = serde_json::from_slice(&requests[0])?;
    let held_alpha = held["input"].to_string().contains("parallel-alpha");
    let fast_label = if held_alpha { "beta" } else { "alpha" };
    let in_progress = observe(&service, &run.id, |run| {
        run.steps
            .values()
            .any(|step| step.label == fast_label && step.result == Some(json!("fast")))
    })
    .await?;
    assert_eq!(in_progress.status, WorkflowStatus::Running);
    assert_eq!(in_progress.result, None);
    assert_eq!(in_progress.accepted_starts, 2);
    release
        .send(())
        .expect("release the first accepted provider request after the other completes");
    let completed = observe(&service, &run.id, |run| {
        run.status == WorkflowStatus::Succeeded
    })
    .await?;
    let values = if held_alpha {
        json!(["held", "fast"])
    } else {
        json!(["fast", "held"])
    };
    assert_eq!(
        completed.result,
        Some(json!({"values":values,"args":args,"budget":{"total":100,"spent":18,"remaining":82}}))
    );
    assert_eq!(
        completed
            .logs
            .iter()
            .map(|entry| (entry.message.as_str(), entry.phase.as_deref()))
            .collect::<Vec<_>>(),
        vec![("Review both requested inputs.", Some("Review"))]
    );
    assert!(
        completed
            .steps
            .values()
            .all(|step| step.phase.as_deref() == Some("Review") && step.attempts.len() == 1)
    );
    assert_eq!(completed.accepted_starts, 2);
    assert_eq!(provider.requests().await.len(), 2);
    assert_eq!(
        serde_json::to_value(&completed.metrics.usage)?,
        json!({"inputTokens":18,"outputTokens":0,"cachedInputTokens":0,"totalTokens":18})
    );
    assert!(completed.metrics.usage_complete);
    test.codex.shutdown_and_wait().await?;
    provider.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipeline_starts_downstream_children_before_the_slowest_upstream_finishes() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let (release, gate) = tokio::sync::oneshot::channel();
    let (provider, _) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("held-upstream")]),
            },
            StreamingSseChunk {
                gate: Some(gate),
                body: responses::sse(vec![
                    responses::ev_assistant_message("held-upstream-result", r#""held-up""#),
                    responses::ev_completed_with_tokens("held-upstream", /*total_tokens*/ 3),
                ]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: answer("fast-upstream-result", r#""fast-up""#, /*tokens*/ 5),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: answer(
                "fast-downstream-result",
                r#""fast-down""#,
                /*tokens*/ 7,
            ),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: answer(
                "held-downstream-result",
                r#""held-down""#,
                /*tokens*/ 11,
            ),
        }],
    ])
    .await;
    let bootstrap = responses::start_mock_server().await;
    let base_url = format!("{}/v1", provider.uri());
    let test = builder()
        .with_config(move |config| {
            config.model_provider.base_url = Some(base_url);
            config.model_provider.supports_websockets = false;
        })
        .build_with_auto_env(&bootstrap)
        .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let run = service
        .start(
            script_with_phases(
                &["Read", "Write"],
                r#"
workflow.phase("Read");
const values = await workflow.pipeline(workflow.args.items,
  item => workflow.agent("upstream-" + item, {label:"up-" + item, phase:"Read"}),
  value => workflow.agent("downstream-" + value, {label:"down-" + value, phase:"Write"}));
workflow.phase("Write");
return values;
"#,
            ),
            json!({"items":["alpha","beta"]}),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    tokio::time::timeout(
        Duration::from_secs(30),
        provider.wait_for_request_count(/*count*/ 3),
    )
    .await
    .context("the completed upstream item never started its downstream child")?;
    let requests = provider.requests().await;
    let held: Value = serde_json::from_slice(&requests[0])?;
    let held_alpha = held["input"].to_string().contains("upstream-alpha");
    let downstream: Value = serde_json::from_slice(&requests[2])?;
    assert!(
        downstream["input"]
            .to_string()
            .contains("downstream-fast-up")
    );
    let streaming = observe(&service, &run.id, |run| {
        run.steps
            .values()
            .any(|step| step.label == "down-fast-up" && step.result == Some(json!("fast-down")))
    })
    .await?;
    assert_eq!(streaming.status, WorkflowStatus::Running);
    assert!(
        !streaming
            .steps
            .values()
            .any(|step| step.label == "down-held-up"),
        "an unfinished upstream item must not manufacture downstream input"
    );
    release
        .send(())
        .expect("release the slow upstream after a downstream result is already durable");
    let completed = observe(&service, &run.id, |run| {
        run.status == WorkflowStatus::Succeeded
    })
    .await?;
    assert_eq!(
        completed.result,
        Some(if held_alpha {
            json!(["held-down", "fast-down"])
        } else {
            json!(["fast-down", "held-down"])
        })
    );
    assert_eq!(completed.accepted_starts, 4);
    assert_eq!(completed.steps.len(), 4);
    assert!(completed.steps.values().all(|step| step.attempts.len() == 1
        && step.phase.as_deref()
            == Some(if step.label.starts_with("up-") {
                "Read"
            } else {
                "Write"
            })));
    assert_eq!(
        completed
            .phase_progress
            .iter()
            .map(|phase| (
                phase.title.as_str(),
                phase.steps_discovered,
                phase.steps_completed
            ))
            .collect::<Vec<_>>(),
        vec![("Read", 2, 2), ("Write", 2, 2)]
    );
    assert_eq!(provider.requests().await.len(), 4);
    assert_eq!(
        completed
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(26)
    );
    test.codex.shutdown_and_wait().await?;
    provider.shutdown().await;
    Ok(())
}

#[derive(Clone, Copy)]
enum InvalidSchema {
    ExternalReference,
    Contradiction,
    Oversized,
}

#[test_case(InvalidSchema::ExternalReference; "external_reference")]
#[test_case(InvalidSchema::Contradiction; "contradictory_required_property")]
#[test_case(InvalidSchema::Oversized; "bounded_schema")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_structured_output_schemas_fail_before_any_native_child_is_accepted(
    invalid: InvalidSchema,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let (schema, reason) = match invalid {
        InvalidSchema::ExternalReference => (
            json!({"$ref":"https://example.invalid/schema.json"}),
            "external",
        ),
        InvalidSchema::Contradiction => (
            json!({"type":"object","required":["forbidden"],"additionalProperties":false}),
            "contradiction",
        ),
        InvalidSchema::Oversized => (json!({"description":"x".repeat(16 * 1024)}), "16384"),
    };
    let provider = responses::start_mock_server().await;
    let test = builder().build_with_auto_env(&provider).await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let run = service.start(script(&format!("return await workflow.agent('Never accept this invalid-schema child.', {{label:'invalid',schema:{schema}}});")), json!({}))
        .await.map_err(anyhow::Error::msg)?;
    let failed = observe(&service, &run.id, |run| {
        run.status == WorkflowStatus::Failed
    })
    .await?;
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains(reason)),
        "schema rejection must explain its actual guard: {:?}",
        failed.error
    );
    assert_eq!(
        (failed.accepted_starts, failed.steps.len(), failed.result),
        (0, 0, None)
    );
    assert_eq!(
        test.thread_manager.list_thread_ids().await,
        vec![test.session_configured.thread_id]
    );
    assert!(
        responses::received_responses_requests(&provider)
            .await
            .is_empty()
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[derive(Clone, Copy)]
enum RepairOutcome {
    ValidThirdAttempt,
    Exhausted,
}

#[test_case(RepairOutcome::ValidThirdAttempt; "valid_repair")]
#[test_case(RepairOutcome::Exhausted; "five_attempt_hard_cap")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structured_output_repairs_use_distinct_native_children_and_forward_the_schema(
    outcome: RepairOutcome,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let provider = responses::start_mock_server().await;
    let count = match outcome {
        RepairOutcome::ValidThirdAttempt => 3,
        RepairOutcome::Exhausted => 5,
    };
    let bodies = (0..count)
        .map(|index| {
            answer(
                &format!("schema-{index}"),
                if matches!(outcome, RepairOutcome::ValidThirdAttempt) && index == 2 {
                    r#"{"ok":true}"#
                } else {
                    r#"{"ok":"invalid"}"#
                },
                /*tokens*/ (index + 1) as i64,
            )
        })
        .collect();
    let model = responses::mount_sse_sequence(&provider, bodies).await;
    let test = builder().build_with_auto_env(&provider).await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let schema = json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false});
    let run = service.start(script(&format!("return await workflow.agent('Return a validated object.',{{label:'validated',reasoningEffort:'low',schema:{schema}}});")), json!({}))
        .await.map_err(anyhow::Error::msg)?;
    let completed = observe(&service, &run.id, |run| {
        matches!(
            run.status,
            WorkflowStatus::Succeeded | WorkflowStatus::Failed
        )
    })
    .await?;
    let step = completed
        .steps
        .values()
        .find(|step| step.label == "validated")
        .context("authored schema step")?;
    assert_eq!(step.attempts.len(), count);
    assert_eq!(completed.accepted_starts, count as u32);
    let children = step
        .attempts
        .iter()
        .map(|attempt| {
            attempt
                .child_thread_id
                .clone()
                .expect("native child was accepted")
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        children.len(),
        count,
        "a schema repair must be a separately charged native attempt"
    );
    let requests = model.requests();
    assert_eq!(requests.len(), count);
    for (index, request) in requests.iter().enumerate() {
        assert_eq!(request.body_json()["text"]["format"]["schema"], schema);
        assert_eq!(request.body_json()["reasoning"]["effort"], json!("low"));
        assert!(request.body_contains_text("Return a validated object."));
        if index != 0 {
            assert!(
                request.body_contains_text("structured output validation failed"),
                "repair feedback must reach the actual next child"
            );
        }
    }
    match outcome {
        RepairOutcome::ValidThirdAttempt => {
            assert_eq!(
                (completed.status, completed.result),
                (WorkflowStatus::Succeeded, Some(json!({"ok":true})))
            );
            assert!(
                step.attempts[..2]
                    .iter()
                    .all(|attempt| attempt.schema_error.is_some())
            );
            assert_eq!(step.attempts[2].schema_error, None);
        }
        RepairOutcome::Exhausted => {
            assert_eq!(
                (completed.status, completed.result),
                (WorkflowStatus::Failed, None)
            );
            assert!(
                step.attempts
                    .iter()
                    .all(|attempt| attempt.schema_error.is_some())
            );
            assert!(
                completed
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("five attempts"))
            );
        }
    }
    assert_eq!(
        completed
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some((count * (count + 1) / 2) as u64)
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unicode_schema_feedback_fits_the_native_task_without_truncating_its_original_prompt()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let provider = responses::start_mock_server().await;
    let invalid = json!({"ok":"😀".repeat(600)}).to_string();
    assert!(invalid.len() < 16 * 1024);
    let model = responses::mount_sse_sequence(
        &provider,
        vec![
            answer("unicode-invalid", &invalid, /*tokens*/ 3),
            answer("unicode-repaired", r#"{"ok":true}"#, /*tokens*/ 5),
        ],
    )
    .await;
    let test = builder().build_with_auto_env(&provider).await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let prompt = format!(
        "{} Preserve the complete original task marker.",
        "p".repeat(7900)
    );
    let schema = json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false});
    let run = service
        .start(
            script(&format!(
                "return await workflow.agent({},{{label:'unicode-repair',schema:{schema}}});",
                serde_json::to_string(&prompt)?
            )),
            json!({}),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let completed = observe(&service, &run.id, |run| {
        matches!(
            run.status,
            WorkflowStatus::Succeeded | WorkflowStatus::Failed
        )
    })
    .await?;
    assert_eq!(
        (
            completed.status,
            &completed.result,
            completed.accepted_starts
        ),
        (WorkflowStatus::Succeeded, &Some(json!({"ok":true})), 2),
        "{:?}",
        completed.error
    );
    let step = completed
        .steps
        .values()
        .find(|step| step.label == "unicode-repair")
        .context("schema repair step")?;
    assert_eq!(step.attempts.len(), 2);
    let error = step.attempts[0]
        .schema_error
        .as_deref()
        .context("actual Unicode validator feedback")?;
    assert!(
        error.len() > 2048 && error.contains('😀'),
        "the native boundary must receive genuinely oversized multibyte feedback"
    );
    let feedback = step.attempts[1]
        .validation_feedback
        .as_deref()
        .context("retained corrective feedback")?;
    assert!(!feedback.is_empty() && feedback.len() <= 2048);
    assert!(
        error.starts_with(feedback),
        "byte truncation must preserve a valid UTF-8 prefix of the real error"
    );
    assert_ne!(
        step.attempts[0].child_thread_id,
        step.attempts[1].child_thread_id
    );
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    let mut tasks = Vec::new();
    for request in &requests {
        let task = super::workflow_task_transport::native_task_text(&request.body_json(), &prompt)?;
        assert!(
            task.contains(&prompt),
            "every native attempt must retain the full original task"
        );
        tasks.push(task);
    }
    let corrected = &tasks[1];
    assert!(corrected.contains("Correct these validation errors:"));
    assert!(
        corrected.contains(feedback),
        "the complete bounded UTF-8 feedback must survive transport alongside the full original task"
    );
    assert_eq!(
        step.options["message"], prompt,
        "transport must not edit the original call options"
    );
    assert_eq!(requests[1].body_json()["text"]["format"]["schema"], schema);
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[test_case(1; "nested_start_is_refused_after_root_spends_shared_limit")]
#[test_case(2; "one_level_nested_workflow_uses_remaining_start")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nested_workflows_share_configured_start_limits_and_preserve_their_arguments(
    max_agents: usize,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let provider = responses::start_mock_server().await;
    let bodies = if max_agents == 1 {
        vec![answer("root", r#"{"root":true}"#, /*tokens*/ 5)]
    } else {
        vec![
            answer("root", r#"{"root":true}"#, /*tokens*/ 5),
            answer("nested", r#"{"nested":true}"#, /*tokens*/ 7),
        ]
    };
    let model = responses::mount_sse_sequence(&provider, bodies).await;
    let test = builder()
        .with_config(move |config| {
            config.dynamic_workflows.max_agents = max_agents;
            config.dynamic_workflows.max_tokens = std::num::NonZeroU64::new(100);
        })
        .build_with_auto_env(&provider)
        .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    service
        .library()
        .save(
            "nested-child",
            &named_script(
                "nested_child",
                &[],
                r#"
const result = await workflow.agent("nested-budget-child", {label:"nested-worker"});
return {args:workflow.args, result};
"#,
            ),
            WorkflowScope::User,
        )
        .map_err(anyhow::Error::msg)?;
    let run = service
        .start(
            script(
                r#"
const root = await workflow.agent("root-budget-child", {label:"root-worker"});
const nested = await workflow("nested_child", {sent:42});
return {root, nested};
"#,
            ),
            json!({}),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let completed = observe(&service, &run.id, |run| {
        matches!(
            run.status,
            WorkflowStatus::Succeeded | WorkflowStatus::Failed
        )
    })
    .await?;
    assert_eq!(
        serde_json::to_value(&completed.limits)?,
        json!({"maxAgents":max_agents,"maxTokens":100})
    );
    assert_eq!(completed.accepted_starts, max_agents as u32);
    assert_eq!(completed.nested_workflows.len(), 1);
    let nested = completed
        .nested_workflows
        .values()
        .next()
        .expect("resolved nested snapshot");
    assert_eq!(
        (nested.name.as_str(), &nested.input),
        ("nested_child", &json!({"sent":42}))
    );
    if max_agents == 1 {
        assert_eq!(
            (completed.status, completed.result),
            (WorkflowStatus::Failed, None)
        );
        assert!(
            completed
                .error
                .as_deref()
                .is_some_and(|error| error.contains("agent-start limit exhausted (1)"))
        );
        let preserved = completed.steps.clone();
        let first_execution = completed.execution_id.clone();
        service.retry(&run.id).await.map_err(anyhow::Error::msg)?;
        let retried = observe(&service, &run.id, |run| {
            run.status == WorkflowStatus::Failed && run.execution_id != first_execution
        })
        .await?;
        assert_eq!(
            (retried.accepted_starts, retried.steps),
            (1, preserved),
            "retry must reuse the successful root child without bypassing the shared limit"
        );
        assert_eq!(
            retried
                .metrics
                .usage
                .as_ref()
                .map(|usage| usage.total_tokens),
            Some(5),
            "a cached child must not charge its tokens twice"
        );
    } else {
        assert_eq!(
            (completed.status, completed.result),
            (
                WorkflowStatus::Succeeded,
                Some(
                    json!({"root":{"root":true},"nested":{"args":{"sent":42},"result":{"nested":true}}})
                )
            )
        );
        assert_eq!(completed.steps.len(), 2);
        assert_eq!(
            completed
                .metrics
                .usage
                .as_ref()
                .map(|usage| usage.total_tokens),
            Some(12)
        );
    }
    assert_eq!(model.requests().len(), max_agents);
    assert_eq!(
        test.thread_manager.list_thread_ids().await.len(),
        max_agents + 1
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_nested_level_is_rejected_before_any_native_child_is_accepted() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let provider = responses::start_mock_server().await;
    let test = builder().build_with_auto_env(&provider).await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    service
        .library()
        .save(
            "level-one",
            &named_script("level_one", &[], "return await workflow('level_two', {});"),
            WorkflowScope::User,
        )
        .map_err(anyhow::Error::msg)?;
    service.library().save("level-two", &named_script("level_two", &[], "return await workflow.agent('A forbidden second nested level.',{label:'forbidden'});"), WorkflowScope::User)
        .map_err(anyhow::Error::msg)?;
    let run = service
        .start(script("return await workflow('level_one', {});"), json!({}))
        .await
        .map_err(anyhow::Error::msg)?;
    let failed = observe(&service, &run.id, |run| {
        run.status == WorkflowStatus::Failed
    })
    .await?;
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains("nest")),
        "nested depth must be the actual rejection: {:?}",
        failed.error
    );
    assert_eq!(
        (failed.accepted_starts, failed.steps.len(), failed.result),
        (0, 0, None)
    );
    assert_eq!(failed.nested_workflows.len(), 1);
    assert!(
        responses::received_responses_requests(&provider)
            .await
            .is_empty()
    );
    assert_eq!(
        test.thread_manager.list_thread_ids().await,
        vec![test.session_configured.thread_id]
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reserved_parallel_child_rechecks_token_budget_when_native_capacity_opens() -> Result<()>
{
    skip_if_no_network!(Ok(()));
    let (release, gate) = tokio::sync::oneshot::channel();
    let (provider, _) = start_streaming_sse_server(vec![vec![
        StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![responses::ev_response_created("spend-budget")]),
        },
        StreamingSseChunk {
            gate: Some(gate),
            body: responses::sse(vec![
                responses::ev_assistant_message("spent-result", r#"{"spent":true}"#),
                responses::ev_completed_with_tokens("spend-budget", /*total_tokens*/ 5),
            ]),
        },
    ]])
    .await;
    let bootstrap = responses::start_mock_server().await;
    let base_url = format!("{}/v1", provider.uri());
    let test = builder()
        .with_config(move |config| {
            config.model_provider.base_url = Some(base_url);
            config.model_provider.supports_websockets = false;
            config.dynamic_workflows.max_tokens = std::num::NonZeroU64::new(5);
            config.multi_agent_v2.max_concurrent_threads_per_session = 2;
        })
        .build_with_auto_env(&bootstrap)
        .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let run = service
        .start(
            script(
                r#"
return await workflow.parallel([
  () => workflow.agent("First authored parallel task.", {label:"first"}),
  () => workflow.agent("Second authored parallel task.", {label:"second"})
]);
"#,
            ),
            json!({}),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    tokio::time::timeout(
        Duration::from_secs(30),
        provider.wait_for_request_count(/*count*/ 1),
    )
    .await?;
    let reserved = observe(&service, &run.id, |run| {
        run.accepted_starts == 1
            && run.steps.len() == 2
            && run
                .steps
                .values()
                .filter(|step| {
                    step.attempts.last().is_some_and(|attempt| {
                        attempt.admission == codex_core::workflows::AdmissionStatus::Reserved
                            && attempt.child_thread_id.is_none()
                    })
                })
                .count()
                == 1
    })
    .await?;
    let waiting = reserved
        .steps
        .values()
        .find(|step| step.attempts[0].child_thread_id.is_none())
        .context("a real second reservation must be waiting for the occupied native slot")?;
    let waiting_label = waiting.label.clone();
    let accepted = reserved
        .steps
        .values()
        .find_map(|step| step.attempts[0].child_thread_id.clone())
        .context("one real native child occupies the entire capacity")?;
    assert_eq!(test.thread_manager.list_thread_ids().await.len(), 2);
    assert_eq!(provider.requests().await.len(), 1);
    release
        .send(())
        .expect("the occupied child completes and spends the whole shared token budget");
    let failed = observe(&service, &run.id, |run| {
        matches!(
            run.status,
            WorkflowStatus::Failed | WorkflowStatus::Succeeded
        )
    })
    .await?;
    assert_eq!(
        (failed.status, failed.result, failed.accepted_starts),
        (WorkflowStatus::Failed, None, 1),
        "exhausting a global budget must reject the whole parallel call, rather than return a null slot"
    );
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains("token budget exhausted (5)")),
        "{:?}",
        failed.error
    );
    assert_eq!(
        failed
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(5)
    );
    let refused = failed
        .steps
        .values()
        .find(|step| step.label == waiting_label)
        .context("retained refused reservation")?;
    assert_eq!(
        (
            refused.attempts.len(),
            refused.attempts[0].child_thread_id.as_deref(),
            refused.attempts[0].admission
        ),
        (1, None, codex_core::workflows::AdmissionStatus::Rejected)
    );
    let accepted_children = failed
        .steps
        .values()
        .flat_map(|step| &step.attempts)
        .filter_map(|attempt| attempt.child_thread_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(accepted_children, vec![accepted]);
    assert_eq!(
        test.thread_manager.list_thread_ids().await.len(),
        2,
        "the waiting reservation must never create another native child"
    );
    assert_eq!(
        provider.requests().await.len(),
        1,
        "the exhausted child must never issue a second provider request"
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_token_budget_blocks_the_next_native_start_after_observed_usage() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let provider = responses::start_mock_server().await;
    let model = responses::mount_sse_once(&provider, answer("spent", "{}", /*tokens*/ 5)).await;
    let test = builder()
        .with_config(|config| {
            config.dynamic_workflows.max_tokens = std::num::NonZeroU64::new(5);
        })
        .build_with_auto_env(&provider)
        .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let run = service.start(script("await workflow.agent('Spend the configured budget.',{label:'spent'}); return await workflow.agent('Must not be accepted after the budget.',{label:'blocked'});"), json!({}))
        .await.map_err(anyhow::Error::msg)?;
    let failed = observe(&service, &run.id, |run| {
        run.status == WorkflowStatus::Failed
    })
    .await?;
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains("token budget exhausted (5)"))
    );
    assert_eq!(
        (failed.accepted_starts, failed.steps.len(), failed.result),
        (1, 1, None)
    );
    assert_eq!(
        failed
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(5)
    );
    assert_eq!(model.requests().len(), 1);
    assert_eq!(test.thread_manager.list_thread_ids().await.len(), 2);
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

fn git(cwd: &std::path::Path, arguments: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(arguments)
        .current_dir(cwd)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "Git fixture command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worktree_native_tool_changes_are_isolated_and_same_child_recovery_keeps_the_checkout()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_remote!(
        Ok(()),
        "managed worktrees and the fixture repository are host-local"
    );
    let (release, gate) = tokio::sync::oneshot::channel();
    let (provider, _) =
        start_streaming_sse_server(vec![
            vec![StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![
            responses::ev_function_call("isolated-index-edit", "exec_command", &json!({
                "cmd":"git read-tree --prefix=isolated-marker/ HEAD", "yield_time_ms":1000,
            }).to_string()),
            responses::ev_completed("isolated-tool"),
        ]),
            }],
            vec![
                StreamingSseChunk {
                    gate: None,
                    body: responses::sse(vec![responses::ev_response_created(
                        "held-after-index-edit",
                    )]),
                },
                StreamingSseChunk {
                    gate: Some(gate),
                    body: responses::sse(vec![responses::ev_completed("held-after-index-edit")]),
                },
            ],
        ])
        .await;
    // Build the root before creating its repository so normal environment selection
    // owns the cwd. Git runs only inside this test's disposable local workspace.
    let bootstrap = responses::start_mock_server().await;
    let base_url = format!("{}/v1", provider.uri());
    let mut root_builder = builder().with_config(move |config| {
        config.model_provider.base_url = Some(base_url);
        config.model_provider.supports_websockets = false;
        config
            .permissions
            .set_permission_profile(PermissionProfile::Disabled)
            .expect("allow the fixture's native Git command");
    });
    let test = root_builder.build_with_auto_env(&bootstrap).await?;
    test.codex.ensure_rollout_materialized().await;
    git(test.cwd.path(), &["init", "-q"])?;
    std::fs::write(test.cwd.path().join("README.md"), "initial fixture\n")?;
    git(test.cwd.path(), &["add", "README.md"])?;
    git(
        test.cwd.path(),
        &[
            "-c",
            "user.name=Workflow QA",
            "-c",
            "user.email=workflow-qa@example.invalid",
            "commit",
            "-q",
            "-m",
            "fixture",
        ],
    )?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let run = service.start(script("return await workflow.agent('Perform the isolated Git index edit.',{label:'isolated',isolation:'worktree'});"), json!({}))
        .await.map_err(anyhow::Error::msg)?;
    tokio::time::timeout(
        Duration::from_secs(30),
        provider.wait_for_request_count(/*count*/ 2),
    )
    .await?;
    let running = observe(&service, &run.id, |run| {
        run.steps.values().any(|step| {
            step.label == "isolated"
                && step
                    .attempts
                    .last()
                    .is_some_and(|attempt| attempt.child_thread_id.is_some())
        })
    })
    .await?;
    let step = running
        .steps
        .values()
        .find(|step| step.label == "isolated")
        .context("isolated native step")?;
    let child_id = codex_protocol::ThreadId::from_string(
        step.attempts[0]
            .child_thread_id
            .as_deref()
            .context("accepted isolated child")?,
    )?;
    let child = test.thread_manager.get_thread(child_id).await?;
    let checkout = child.config_snapshot().await.cwd().to_path_buf();
    assert_ne!(
        std::fs::canonicalize(&checkout)?,
        std::fs::canonicalize(test.cwd.path())?
    );
    assert_eq!(
        git(&checkout, &["ls-files", "isolated-marker/README.md"])?,
        "isolated-marker/README.md"
    );
    assert_eq!(
        git(test.cwd.path(), &["ls-files", "isolated-marker/README.md"])?,
        "",
        "the real child tool must not alter the source checkout's index"
    );
    let worktrees = git(test.cwd.path(), &["worktree", "list", "--porcelain"])?;
    assert_eq!(
        worktrees
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
        2
    );
    let shutdown = test
        .thread_manager
        .request_agent_tree_shutdown(test.session_configured.thread_id)
        .await?;
    tokio::time::timeout(Duration::from_secs(30), shutdown.wait()).await??;
    drop(service);
    let resumed = builder().restart_with_auto_env(&bootstrap, &test).await?;
    let service = resumed
        .codex
        .workflows()
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        service.get(&run.id).map_err(anyhow::Error::msg)?.status,
        WorkflowStatus::Interrupted
    );
    service.recover(&run.id).await.map_err(anyhow::Error::msg)?;
    let reconciled = observe(&service, &run.id, |run| {
        run.status == WorkflowStatus::Failed
    })
    .await?;
    let recovered = reconciled
        .steps
        .values()
        .find(|step| step.label == "isolated")
        .context("reconciled isolated step")?;
    assert_eq!(
        (
            reconciled.accepted_starts,
            recovered.attempts.len(),
            recovered.attempts[0].child_thread_id.as_deref()
        ),
        (1, 1, Some(child_id.to_string().as_str()))
    );
    assert!(
        checkout.is_dir(),
        "a failed accepted child must retain its checkout for inspection"
    );
    assert_eq!(
        git(&checkout, &["ls-files", "isolated-marker/README.md"])?,
        "isolated-marker/README.md"
    );
    assert_eq!(
        git(test.cwd.path(), &["worktree", "list", "--porcelain"])?,
        worktrees,
        "recovering the same accepted child must not allocate another checkout"
    );
    assert_eq!(
        provider.requests().await.len(),
        2,
        "same-child reconciliation must not sample another model request"
    );
    assert!(
        responses::received_responses_requests(&bootstrap)
            .await
            .is_empty()
    );
    let _ = release.send(());
    resumed.codex.shutdown_and_wait().await?;
    provider.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worktree_isolation_rejects_a_non_repository_before_accepting_a_child() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_remote!(
        Ok(()),
        "managed worktrees require a host-local Git repository"
    );
    let provider = responses::start_mock_server().await;
    let test = builder().build_with_auto_env(&provider).await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let run = service.start(script("return await workflow.agent('Do not create a shared-cwd substitute.',{label:'isolated',isolation:'worktree'});"), json!({}))
        .await.map_err(anyhow::Error::msg)?;
    let failed = observe(&service, &run.id, |run| {
        run.status == WorkflowStatus::Failed
    })
    .await?;
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Git repository"))
    );
    assert_eq!(failed.accepted_starts, 0);
    assert_eq!(
        test.thread_manager.list_thread_ids().await,
        vec![test.session_configured.thread_id]
    );
    assert!(
        responses::received_responses_requests(&provider)
            .await
            .is_empty()
    );
    assert!(!test.home.path().join("worktrees").exists());
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_usage_separates_cached_tokens_and_paused_time_without_counting_a_resample_twice()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let (report_usage, usage_gate) = tokio::sync::oneshot::channel();
    let (release_cancelled, cancelled_gate) = tokio::sync::oneshot::channel();
    let mut completed_usage =
        responses::ev_completed_with_tokens("opening", /*total_tokens*/ 15);
    completed_usage["response"]["usage"] = json!({
        "input_tokens":10,"input_tokens_details":{"cached_tokens":3},
        "output_tokens":5,"output_tokens_details":null,"total_tokens":15,
    });
    let (provider, _) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("opening")]),
            },
            StreamingSseChunk {
                gate: Some(usage_gate),
                body: responses::sse(vec![
                    responses::ev_function_call("usage-tool", "test_sync_tool", "{}"),
                    completed_usage,
                ]),
            },
        ],
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("pause-this-response")]),
            },
            StreamingSseChunk {
                gate: Some(cancelled_gate),
                body: responses::sse(vec![responses::ev_completed("pause-this-response")]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: answer("resumed", r#"{"done":true}"#, /*tokens*/ 7),
        }],
    ])
    .await;
    let bootstrap = responses::start_mock_server().await;
    let base_url = format!("{}/v1", provider.uri());
    let test = builder()
        .with_model_info_override("gpt-5.6-sol", |model| {
            model
                .experimental_supported_tools
                .push("test_sync_tool".to_owned());
        })
        .with_config(move |config| {
            config.model_provider.base_url = Some(base_url);
            config.model_provider.supports_websockets = false;
        })
        .build_with_auto_env(&bootstrap)
        .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let run = service.start(script("return await workflow.agent('Observe live usage through a tool and a paused model response.',{label:'metered'});"), json!({}))
        .await.map_err(anyhow::Error::msg)?;
    tokio::time::timeout(
        Duration::from_secs(30),
        provider.wait_for_request_count(/*count*/ 1),
    )
    .await?;
    let unknown = service.get(&run.id).map_err(anyhow::Error::msg)?;
    assert_eq!(
        unknown.metrics.usage, None,
        "absence of usage is not a fabricated zero"
    );
    assert!(!unknown.metrics.usage_complete);
    report_usage
        .send(())
        .expect("report the first real response's usage");
    tokio::time::timeout(
        Duration::from_secs(30),
        provider.wait_for_request_count(/*count*/ 2),
    )
    .await?;
    let live = observe(&service, &run.id, |run| {
        run.metrics
            .usage
            .as_ref()
            .is_some_and(|usage| usage.total_tokens == 15)
    })
    .await?;
    assert_eq!(
        serde_json::to_value(&live.metrics.usage)?,
        json!({"inputTokens":10,"outputTokens":5,"cachedInputTokens":3,"totalTokens":15})
    );
    assert_eq!(live.status, WorkflowStatus::Running);
    assert!(!live.metrics.usage_complete);
    let paused = test.codex.pause_execution().await?;
    assert_eq!(
        paused.state,
        codex_protocol::protocol::ExecutionControlState::Paused
    );
    let pause_settled = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )?;
    let stopped_clock = observe(&service, &run.id, |run| {
        run.activity
            .checkpoint_at_ms
            .is_some_and(|checkpoint| checkpoint >= pause_settled)
    })
    .await?;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(150),
            provider.wait_for_request_count(/*count*/ 3)
        )
        .await
        .is_err(),
        "the paused child must not reopen inference to finish its workflow"
    );
    let still_paused = service.get(&run.id).map_err(anyhow::Error::msg)?;
    assert_eq!(
        still_paused.metrics.active_duration_ms, stopped_clock.metrics.active_duration_ms,
        "active time must remain fixed throughout an observed paused interval"
    );
    assert!(still_paused.metrics.wall_duration_ms > stopped_clock.metrics.wall_duration_ms);
    assert_eq!(still_paused.metrics.usage, live.metrics.usage);
    test.codex.unpause_execution().await?;
    let finished = observe(&service, &run.id, |run| {
        run.status == WorkflowStatus::Succeeded && run.metrics.usage_complete
    })
    .await?;
    assert_eq!(finished.result, Some(json!({"done":true})));
    assert_eq!(finished.accepted_starts, 1);
    assert_eq!(
        serde_json::to_value(&finished.metrics.usage)?,
        json!({"inputTokens":17,"outputTokens":5,"cachedInputTokens":3,"totalTokens":22})
    );
    let requests = provider
        .requests()
        .await
        .into_iter()
        .map(|body| serde_json::from_slice::<Value>(&body))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| request["client_metadata"]["thread_id"]
                == requests[0]["client_metadata"]["thread_id"])
    );
    let _ = release_cancelled.send(());
    test.codex.shutdown_and_wait().await?;
    provider.shutdown().await;
    Ok(())
}

#[derive(Clone, Copy)]
enum DeniedRead {
    Repository,
    IsolatedFile,
    SymlinkedFile,
}

#[test_case(DeniedRead::Repository; "repository_read_denied_before_checkout_or_child")]
#[test_case(DeniedRead::IsolatedFile; "source_file_denial_follows_the_real_isolated_tool")]
#[test_case(DeniedRead::SymlinkedFile; "symlinked_caller_file_denial_follows_the_real_isolated_tool")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worktree_preflight_and_actual_child_tools_preserve_the_callers_read_denials(
    denied: DeniedRead,
) -> Result<()> {
    use codex_core::config::Constrained;
    use codex_protocol::permissions::FileSystemAccessMode;
    use codex_protocol::permissions::FileSystemSandboxEntry;
    use codex_protocol::permissions::NetworkSandboxPolicy;
    use codex_protocol::protocol::AskForApproval;

    skip_if_no_network!(Ok(()));
    skip_if_remote!(
        Ok(()),
        "managed worktree permission fixtures are host-local"
    );
    core_test_support::skip_if_target_windows!(
        Ok(()),
        "uses the POSIX cat command to exercise denied file reads"
    );
    let provider = responses::start_mock_server().await;
    let test = builder()
        .with_config(move |config| {
            if matches!(denied, DeniedRead::SymlinkedFile) {
                let physical = config.cwd.join("physical-repository");
                let alias = config.cwd.join("caller-alias");
                std::fs::create_dir_all(&physical).expect("create the real fixture repository");
                core_test_support::create_directory_symlink(physical.as_path(), alias.as_path());
                config.cwd = alias;
            }
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::Never);
            let mut file_system = PermissionProfile::workspace_write_with(
                &[],
                NetworkSandboxPolicy::Restricted,
                /*exclude_tmpdir_env_var*/ true,
                /*exclude_slash_tmp*/ true,
            )
            .file_system_sandbox_policy();
            let path = match denied {
                DeniedRead::Repository => config.cwd.join(".git"),
                DeniedRead::IsolatedFile | DeniedRead::SymlinkedFile => {
                    config.cwd.join("secret.txt")
                }
            };
            file_system.entries.push(FileSystemSandboxEntry::new(
                path.into(),
                FileSystemAccessMode::Deny,
            ));
            config
                .permissions
                .set_permission_profile(PermissionProfile::from_runtime_permissions(
                    &file_system,
                    NetworkSandboxPolicy::Restricted,
                ))
                .expect("install the caller's actual read denial");
        })
        .build_with_auto_env(&provider)
        .await?;
    let source = test.config.cwd.as_path();
    if matches!(denied, DeniedRead::SymlinkedFile) {
        assert_ne!(
            source,
            std::fs::canonicalize(source)?.as_path(),
            "the caller must retain a lexical cwd alias of the canonical repository"
        );
    }
    git(source, &["init", "-q"])?;
    std::fs::write(source.join("secret.txt"), "WORKTREE_PRIVATE_CONTENT")?;
    git(source, &["add", "secret.txt"])?;
    git(
        source,
        &[
            "-c",
            "user.name=Workflow QA",
            "-c",
            "user.email=workflow-qa@example.invalid",
            "commit",
            "-q",
            "-m",
            "fixture",
        ],
    )?;
    let model = if matches!(denied, DeniedRead::IsolatedFile | DeniedRead::SymlinkedFile) {
        Some(
            responses::mount_sse_sequence(
                &provider,
                vec![
                    responses::sse(vec![
                        responses::ev_function_call(
                            "denied-read",
                            "exec_command",
                            &json!({"cmd":"cat secret.txt","yield_time_ms":1000}).to_string(),
                        ),
                        responses::ev_completed("read-attempt"),
                    ]),
                    answer("report-denial", r#"{"readDenied":true}"#, /*tokens*/ 1),
                ],
            )
            .await,
        )
    } else {
        None
    };
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let run = service.start(script("return await workflow.agent('Check whether secret.txt is readable without escalation.',{label:'read-check',isolation:'worktree'});"), json!({}))
        .await.map_err(anyhow::Error::msg)?;
    let terminal = observe(&service, &run.id, |run| {
        matches!(
            run.status,
            WorkflowStatus::Failed | WorkflowStatus::Succeeded
        )
    })
    .await?;
    match denied {
        DeniedRead::Repository => {
            assert_eq!(terminal.accepted_starts, 0);
            assert!(
                terminal
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("does not permit reading this repository"))
            );
            assert_eq!(
                test.thread_manager.list_thread_ids().await,
                vec![test.session_configured.thread_id]
            );
            assert!(!test.home.path().join("worktrees").exists());
            assert!(
                responses::received_responses_requests(&provider)
                    .await
                    .is_empty()
            );
        }
        DeniedRead::IsolatedFile | DeniedRead::SymlinkedFile => {
            assert_eq!(
                (terminal.status, terminal.result, terminal.accepted_starts),
                (
                    WorkflowStatus::Succeeded,
                    Some(json!({"readDenied":true})),
                    1
                )
            );
            let step = terminal
                .steps
                .values()
                .find(|step| step.label == "read-check")
                .context("read-check child")?;
            let child = test
                .thread_manager
                .get_thread(codex_protocol::ThreadId::from_string(
                    step.attempts[0]
                        .child_thread_id
                        .as_deref()
                        .context("accepted isolated child")?,
                )?)
                .await?;
            let checkout = child.config_snapshot().await.cwd().to_path_buf();
            assert_ne!(
                std::fs::canonicalize(&checkout)?,
                std::fs::canonicalize(source)?
            );
            assert_eq!(
                std::fs::read_to_string(checkout.join("secret.txt"))?,
                "WORKTREE_PRIVATE_CONTENT",
                "the tracked file must really exist in the checkout before testing the tool's denial"
            );
            let requests = model.expect("native denial fixture").requests();
            assert_eq!(requests.len(), 2);
            let output = requests[1].function_call_output("denied-read").to_string();
            assert!(output.contains("secret.txt"));
            assert!(
                !output.contains("WORKTREE_PRIVATE_CONTENT"),
                "the actual isolated tool must not bypass the inherited denial"
            );
            assert!(
                output.contains("Permission denied")
                    || output.contains("Operation not permitted")
                    || output.contains("sandbox"),
                "the output must prove a permission rejection rather than a missing file: {output}"
            );
        }
    }
    assert_eq!(
        std::fs::read_to_string(source.join("secret.txt"))?,
        "WORKTREE_PRIVATE_CONTENT"
    );
    assert_eq!(
        git(source, &["status", "--porcelain"])?,
        "",
        "an isolated read must not mutate the shared source repository"
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[test_case(WorkflowStatus::Succeeded; "completed_legacy_record")]
#[test_case(WorkflowStatus::Interrupted; "unfinished_legacy_record")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_records_are_read_only_and_cannot_retry_recover_or_start_an_adapter(
    status: WorkflowStatus,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = builder().build_with_auto_env(&server).await?;
    test.codex.ensure_rollout_materialized().await;
    let root = test.session_configured.thread_id;
    let directory = test
        .home
        .path()
        .join("workflow-runs")
        .join(root.to_string());
    tokio::fs::create_dir_all(&directory).await?;
    let source = "await workflow.publish(await workflow.agent('legacy-step',{message:'Legacy source must never execute.'}));";
    // Historical V1 fields are deliberately retained without a version or V2 metadata.
    let expected: WorkflowRun = serde_json::from_value(json!({
        "id":"53eb59ac-6176-42bb-a3fb-10eff40c5ad3",
        "executionId":"bf190e59-05ed-45f1-bf98-43a95406b9c3",
        "notificationDelivered":true,
        "ownerThreadId":root.to_string(), "source":source, "input":{"legacy":true},
        "status":status, "phase":null, "steps":{}, "error":null,
        "createdAt":1, "updatedAt":2,
    }))?;
    let path = directory.join(format!("{}.json", expected.id));
    let original = serde_json::to_vec(&json!({
        "id":expected.id, "executionId":expected.execution_id,
        "notificationDelivered":true, "ownerThreadId":root.to_string(),
        "source":source, "input":{"legacy":true}, "status":status,
        "phase":null, "steps":{}, "error":null, "createdAt":1, "updatedAt":2,
    }))?;
    tokio::fs::write(&path, &original).await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    assert_eq!(
        service.get(&expected.id).map_err(anyhow::Error::msg)?,
        expected
    );
    assert_eq!(
        service
            .list(/*offset*/ 0, /*limit*/ 50)
            .map_err(anyhow::Error::msg)?,
        vec![expected.clone()]
    );
    for result in [
        service.retry(&expected.id).await,
        service.recover(&expected.id).await,
    ] {
        let error = result.expect_err("legacy records must not execute through retry or recovery");
        assert!(
            error.to_lowercase().contains("read-only"),
            "expected a read-only rejection, got: {error}"
        );
    }
    let error = service
        .start(source.to_owned(), json!({}))
        .await
        .expect_err("legacy source must not be adapted into a new runnable workflow");
    assert!(
        error.to_lowercase().contains("read-only"),
        "expected a legacy-source rejection, got: {error}"
    );
    assert_eq!(
        tokio::fs::read(&path).await?,
        original,
        "opening or rejecting execution must not rewrite old records"
    );
    assert_eq!(
        service
            .list(/*offset*/ 0, /*limit*/ 50)
            .map_err(anyhow::Error::msg)?,
        vec![expected]
    );
    assert_eq!(test.thread_manager.list_thread_ids().await, vec![root]);
    assert!(
        responses::received_responses_requests(&server)
            .await
            .is_empty()
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
