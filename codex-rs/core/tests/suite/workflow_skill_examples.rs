//! Installed examples execute through V8 and the actual native child service.
use super::*;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::ToolLifecycleContributor;
use codex_extension_api::ToolLifecycleFuture;
use codex_extension_api::ToolStartInput;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::Mutex;
use wiremock::matchers::header;

#[derive(Default)]
struct ObservedTools(Mutex<Vec<String>>);

impl ToolLifecycleContributor for ObservedTools {
    fn on_tool_start<'a>(&'a self, input: ToolStartInput<'a>) -> ToolLifecycleFuture<'a> {
        Box::pin(async move {
            self.0
                .lock()
                .expect("tool observation lock")
                .push(input.tool_name.name.clone());
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_authoring_example_launches_inline_through_code_mode_and_returns_markdown()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let provider = responses::start_mock_server().await;
    let tools = Arc::new(ObservedTools::default());
    let mut extensions = ExtensionRegistryBuilder::<codex_core::config::Config>::new();
    extensions.tool_lifecycle_contributor(tools.clone());
    let test = builder()
        .with_extensions(Arc::new(extensions.build()))
        .with_model_info_override("gpt-5.6-sol", |model| {
            model.tool_mode = Some(ToolMode::CodeMode);
            model.multi_agent_version = Some(MultiAgentVersion::V2);
        })
        .with_config(|config| {
            config
                .features
                .enable(Feature::CodeMode)
                .expect("enable the advertised code-mode tool");
        })
        .build_with_auto_env(&provider)
        .await?;
    // Execute the installed launch call, including its inline source and args.
    // The external model is controlled; native tools, V8 and persistence stay real.
    codex_skills::install_system_skills(&test.config.codex_home)?;
    let reference = tokio::fs::read_to_string(
        test.config
            .codex_home
            .join("skills/.system/dynamic-workflow/references/authoring.md"),
    )
    .await?;
    let wrapper = reference
        .split_once("```javascript\n")
        .context("the installed launch example")?
        .1
        .split_once("```")
        .context("the complete launch example")?
        .0;
    let source = wrapper
        .split_once("const source = `")
        .context("inline workflow source")?
        .1
        .split_once("`;\ntext(")
        .context("the complete inline source")?
        .0;
    let root = test.session_configured.thread_id.to_string();
    responses::mount_sse_once_match(
        &provider,
        header("thread-id", root.as_str()),
        responses::sse(vec![
            responses::ev_response_created("launch-inline"),
            responses::ev_custom_tool_call("inline-workflow", "exec", wrapper),
            responses::ev_completed("launch-inline"),
        ]),
    )
    .await;
    let parent_completion = responses::mount_sse_once_match(
        &provider,
        header("thread-id", root.as_str()),
        answer(
            "parent-finished",
            "The workflow is running.",
            /*tokens*/ 1,
        ),
    )
    .await;
    let mut children = Vec::new();
    for (id, text, tokens) in [
        (
            "review-example",
            r#"{"findings":["Retain the existing permission denial."]}"#,
            3,
        ),
        (
            "summary-example",
            "## Review\n\nPreserve the caller's denied paths.",
            5,
        ),
    ] {
        let parent = root.clone();
        children.push(
            responses::mount_sse_once_match(
                &provider,
                move |request: &wiremock::Request| {
                    request
                        .headers
                        .get("thread-id")
                        .is_some_and(|id| id != parent.as_str())
                },
                answer(id, text, tokens),
            )
            .await,
        );
    }
    test.submit_text_turn("Run the installed inline review example for the requested change.")
        .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let runs = service
        .list(/*offset*/ 0, /*limit*/ 5)
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        runs.len(),
        1,
        "the inline call must create exactly one workflow"
    );
    let finished = observe(&service, &runs[0].id, |run| {
        run.status == WorkflowStatus::Succeeded
    })
    .await?;
    assert_eq!(
        (
            finished.source.as_str(),
            &finished.input,
            &finished.result,
            finished.accepted_starts
        ),
        (
            source,
            &json!({"target":"the requested change"}),
            &Some(json!("## Review\n\nPreserve the caller's denied paths.")),
            2
        )
    );
    assert_eq!(
        finished.metadata.as_ref().map(|meta| meta.name.as_str()),
        Some("review-change")
    );
    assert_eq!(
        finished
            .phase_progress
            .iter()
            .map(|phase| (
                phase.title.as_str(),
                phase.steps_discovered,
                phase.steps_completed
            ))
            .collect::<Vec<_>>(),
        vec![("Review", 1, 1), ("Summarize", 1, 1)]
    );
    assert!(
        finished
            .steps
            .values()
            .all(|step| step.options.get("schema").is_none()),
        "native workers may return structured data or Markdown without a schema"
    );
    let review = children[0].single_request();
    let summary = children[1].single_request();
    assert!(
        review.body_contains_text(
            "Review the requested change: {\"target\":\"the requested change\"}"
        )
    );
    assert!(summary.body_contains_text("Summarize actionable findings as Markdown: {\"findings\":[\"Retain the existing permission denial.\"]}"));
    assert_ne!(
        review.header("thread-id"),
        summary.header("thread-id"),
        "both authored stages must use real native children"
    );
    assert_eq!(
        *tools.0.lock().expect("observed native calls"),
        vec!["exec".to_owned(), "workflow_start".to_owned()],
        "the installed inline launch must not stage or read back a script through filesystem tools"
    );
    let output = parent_completion
        .single_request()
        .custom_tool_call_output("inline-workflow");
    let receipt = output.to_string();
    assert!(
        receipt.contains(&finished.id),
        "the code-mode call returns the run receipt"
    );
    assert!(
        !receipt.contains("workflow.meta") && !receipt.contains("Summarize actionable"),
        "launch output must not dump its authored source into the conversation"
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_repair_example_rechecks_changed_files_and_rejects_aliased_ownership()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let provider = responses::start_mock_server().await;
    let report = json!({
        "complete":true,"blocked":"","checks":["Targeted checks passed"],"findings":[]
    });
    let finding = json!({
        "complete":false,"blocked":"","checks":["Checked empty input"],
        "findings":[{"path":"src/lib.rs","issue":"Handle empty input","check":"Run the empty-input test"}]
    });
    let model = responses::mount_sse_sequence(
        &provider,
        vec![
            answer(
                "implement",
                r#"{"summary":"Implementation summary."}"#,
                /*tokens*/ 1,
            ),
            answer("initial-qa", &finding.to_string(), /*tokens*/ 1),
            answer("initial-review", &report.to_string(), /*tokens*/ 1),
            answer("repair", "Repaired src/lib.rs.", /*tokens*/ 1),
            answer("fresh-qa", &report.to_string(), /*tokens*/ 1),
            answer("fresh-review", &report.to_string(), /*tokens*/ 1),
        ],
    )
    .await;
    let test = builder()
        .with_config(|config| {
            config.dynamic_workflows.max_agents = 8;
            config.dynamic_workflows.max_tokens = std::num::NonZeroU64::new(100);
        })
        .build_with_auto_env(&provider)
        .await?;
    codex_skills::install_system_skills(&test.config.codex_home)?;
    let source = tokio::fs::read_to_string(
        test.config
            .codex_home
            .join("skills/.system/dynamic-workflow/references/repair-loop.js"),
    )
    .await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let aliased = test
        .codex
        .start_workflow(
            source.clone(),
            json!({
                "task":"Handle empty input",
                "files":{"implementation":["src/lib.rs"],"tests":["src/./lib.rs"]}
            }),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let mut events = service.subscribe();
    let rejected = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let run = service.get(&aliased.id).map_err(anyhow::Error::msg)?;
            if run.status == WorkflowStatus::Failed {
                break Ok::<_, anyhow::Error>(run);
            }
            events.recv().await?;
        }
    })
    .await??;
    assert_eq!((rejected.accepted_starts, model.requests().len()), (0, 0));
    assert!(
        rejected
            .error
            .as_deref()
            .is_some_and(|error| error.contains("canonical repository-relative"))
    );

    let run = test
        .codex
        .start_workflow(
            source,
            json!({
                "task":"Handle empty input",
                "files":{"implementation":["src/lib.rs"],"tests":["tests/empty.rs"]}
            }),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let finished = observe(&service, &run.id, |run| {
        run.status == WorkflowStatus::Succeeded
    })
    .await?;
    assert_eq!(
        finished.result,
        Some(json!({
            "status":"verified","checks":["Targeted checks passed","Targeted checks passed"],
            "summary":"Implementation summary."
        }))
    );
    assert_eq!(
        (
            finished.accepted_starts,
            finished.limits.max_agents,
            finished.limits.max_tokens
        ),
        (6, 8, Some(100))
    );
    assert_eq!(
        finished
            .phase_progress
            .iter()
            .map(|phase| (
                phase.title.as_str(),
                phase.steps_discovered,
                phase.steps_completed
            ))
            .collect::<Vec<_>>(),
        vec![("Implement", 1, 1), ("Verify", 4, 4), ("Repair", 1, 1)]
    );
    let requests = model.requests();
    assert_eq!(requests.len(), 6);
    assert!(requests[3].body_contains_text("Repair only the listed files for implementation"));
    assert!(requests[3].body_contains_text("Handle empty input"));
    assert!(
        !requests[3].body_contains_text("tests/empty.rs"),
        "the repair receives only its owner's files and findings"
    );
    for (round, pair) in requests[1..3].iter().zip(&requests[4..6]) {
        assert_ne!(
            round.header("thread-id"),
            pair.header("thread-id"),
            "changed files require fresh verification agents"
        );
    }
    assert!(
        requests[4..6]
            .iter()
            .any(|request| request.body_contains_text("QA round 1"))
    );
    assert!(
        requests[4..6]
            .iter()
            .any(|request| request.body_contains_text("Review round 1"))
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
