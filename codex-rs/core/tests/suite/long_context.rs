use anyhow::Result;
use codex_history::RolloutItem;
use codex_protocol::config_types::AutoCompactTokenLimitScope;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::sse;
use core_test_support::submit_thread_settings;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use test_case::test_case;

#[test_case(AutoCompactTokenLimitScope::Total; "total")]
#[test_case(AutoCompactTokenLimitScope::BodyAfterPrefix; "body_after_prefix")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_context_scales_compaction_and_compacts_after_disable(
    scope: AutoCompactTokenLimitScope,
) -> Result<()> {
    let server = responses::start_mock_server().await;
    let first = responses::mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("m1", "first"),
            ev_completed_with_tokens("r1", /*total_tokens*/ 10_000),
        ]),
    )
    .await;
    let mut builder = test_codex()
        .with_model("gpt-5.5")
        .with_model_info_override("gpt-5.5", |model| {
            model.context_window = Some(100_000);
            model.max_context_window = Some(300_000);
            model.comp_hash = None;
        })
        .with_config(move |config| {
            config.long_context = true;
            config.model_auto_compact_token_limit = Some(30_000);
            config.model_auto_compact_token_limit_scope = scope;
            config.model_provider.name = "Long context test".to_string();
            config.model_provider.supports_websockets = false;
            config.compact_prompt = Some("compact the conversation".to_string());
        });
    let test = builder.build_with_auto_env(&server).await?;
    test.submit_turn("first").await?;
    assert_eq!(first.requests().len(), 1);

    let growth = responses::mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("m2", "second"),
            ev_completed_with_tokens("r2", /*total_tokens*/ 60_000),
        ]),
    )
    .await;
    test.submit_turn("second").await?;
    assert_eq!(growth.requests().len(), 1);
    let expanded = responses::mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("m3", "still expanded"),
            ev_completed_with_tokens("r3", /*total_tokens*/ 60_000),
        ]),
    )
    .await;
    test.submit_turn("third").await?;
    assert_eq!(
        expanded.requests().len(),
        1,
        "growth above 30k stays below the scaled 90k threshold"
    );

    submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            long_context: Some(false),
            ..Default::default()
        },
    )
    .await?;
    let normal = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("summary", "summary"),
                ev_completed_with_tokens("compact", /*total_tokens*/ 100),
            ]),
            sse(vec![
                ev_assistant_message("m4", "normal"),
                ev_completed_with_tokens("r4", /*total_tokens*/ 1_000),
            ]),
        ],
    )
    .await;
    test.submit_turn("fourth").await?;
    let requests = normal.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .input()
            .iter()
            .any(|item| item.to_string().contains("compact the conversation"))
    );
    assert!(!test.codex.config_snapshot().await.long_context);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_context_refreshes_limits_and_survives_resume() -> Result<()> {
    let server = responses::start_mock_server().await;
    let baseline = responses::mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("baseline", "baseline"),
            ev_completed_with_tokens("baseline", /*total_tokens*/ 1_000),
        ]),
    )
    .await;
    let mut builder =
        test_codex()
            .with_model("gpt-5.5")
            .with_model_info_override("gpt-5.5", |model| {
                model.context_window = Some(100_000);
                model.max_context_window = Some(300_000);
            });
    let test = builder.build_with_auto_env(&server).await?;
    test.submit_turn("materialize the thread").await?;
    baseline.single_request();
    let usage = TokenUsage {
        input_tokens: 1_000,
        total_tokens: 1_000,
        ..Default::default()
    };
    for (enabled, expected) in [(true, 285_000), (false, 95_000), (true, 285_000)] {
        submit_thread_settings(
            &test.codex,
            ThreadSettingsOverrides {
                long_context: Some(enabled),
                ..Default::default()
            },
        )
        .await?;
        let event = wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TokenCount(_))
        })
        .await;
        let EventMsg::TokenCount(event) = event else {
            unreachable!()
        };
        assert_eq!(
            event.info,
            Some(TokenUsageInfo {
                total_token_usage: usage.clone(),
                last_token_usage: usage.clone(),
                model_context_window: Some(expected),
            })
        );
    }
    let catalog = test.config.model_catalog.clone();
    let mut builder = builder.with_config(move |config| config.model_catalog = catalog);
    let resumed = builder.restart(&server, &test).await?;
    assert_eq!(
        (
            resumed.session_configured.long_context,
            resumed.codex.config_snapshot().await.long_context
        ),
        (true, true)
    );
    assert_eq!(
        resumed.codex.token_usage_info().await,
        Some(TokenUsageInfo {
            total_token_usage: usage.clone(),
            last_token_usage: usage,
            model_context_window: Some(285_000),
        })
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_context_legacy_checkpoint_inherits_configured_default() -> Result<()> {
    let server = responses::start_mock_server().await;
    let baseline = responses::mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("baseline", "baseline"),
            ev_completed_with_tokens("baseline", /*total_tokens*/ 1_000),
        ]),
    )
    .await;
    let mut builder = test_codex().with_model("gpt-5.5");
    let test = builder.build_with_auto_env(&server).await?;
    test.submit_turn("materialize the thread").await?;
    baseline.single_request();
    submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            long_context: Some(false),
            ..Default::default()
        },
    )
    .await?;
    test.codex.shutdown_and_wait().await?;
    let path = test.codex.rollout_path().expect("rollout path");
    let mut found_checkpoint = false;
    let legacy_lines = std::fs::read_to_string(&path)?
        .lines()
        .map(|line| {
            let mut line = codex_rollout::parse_rollout_line(line)?;
            if let RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(event)) = &mut line.item {
                event.thread_settings.long_context = None;
                found_checkpoint = true;
            }
            serde_json::to_string(&line)
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert!(found_checkpoint);
    std::fs::write(&path, format!("{}\n", legacy_lines.join("\n")))?;
    let mut builder = builder.with_config(|config| config.long_context = true);
    let resumed = builder
        .resume(&server, Arc::clone(&test.home), path)
        .await?;
    assert_eq!(
        (
            resumed.session_configured.long_context,
            resumed.codex.config_snapshot().await.long_context
        ),
        (true, true)
    );
    Ok(())
}
