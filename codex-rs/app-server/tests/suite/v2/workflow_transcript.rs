//! Workflow chat is actual worker history, scoped to durable owned turns.

use anyhow::Context;
use anyhow::Result;
use codex_app_server_protocol::ThreadLoadedListResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadTimelineEntry;
use codex_app_server_protocol::ThreadTimelineListResponse;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::WorkflowLifecycleEvent;
use codex_app_server_protocol::WorkflowLifecycleKind;
use codex_app_server_protocol::WorkflowResult;
use codex_app_server_protocol::WorkflowResultDisplayResponse;
use codex_app_server_protocol::WorkflowStartResponse;
use codex_app_server_protocol::WorkflowStatus;
use codex_app_server_protocol::WorkflowTranscriptListResponse;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeSet;
use std::time::Duration;
use tempfile::TempDir;

use super::workflows::client;
use super::workflows::configure_agent_workflows;
use super::workflows::rpc;
use super::workflows::terminal;

async fn rejected_transcript(
    app: &mut app_test_support::TestAppServer,
    params: Value,
) -> Result<()> {
    let request = app
        .send_request("workflow/transcript/list", Some(params))
        .await?;
    let error = tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        app.read_stream_until_error_message(codex_app_server_protocol::RequestId::Integer(request)),
    )
    .await??;
    assert_eq!(error.error.code, -32600);
    Ok(())
}

async fn turn_finished(app: &mut app_test_support::TestAppServer, thread: &str) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        app.read_stream_until_matching_notification(
            "the expected thread's normal human turn completes",
            |notice| {
                notice.method == "turn/completed"
                    && notice
                        .params
                        .as_ref()
                        .is_some_and(|params| params["threadId"] == thread)
            },
        ),
    )
    .await??;
    Ok(())
}

async fn lifecycle_finished(app: &mut app_test_support::TestAppServer, run: &str) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        app.read_stream_until_matching_notification(
            "the expected workflow's durable completion is presented",
            |notice| {
                notice.method == "workflow/lifecycle/changed"
                    && notice.params.as_ref().is_some_and(|params| {
                        params["event"]["runId"] == run
                            && params["event"]["kind"]["type"] == "completed"
                    })
            },
        ),
    )
    .await??;
    Ok(())
}

async fn transcript(
    app: &mut app_test_support::TestAppServer,
    owner: &str,
    run: &str,
    target: Value,
) -> Result<WorkflowTranscriptListResponse> {
    rpc(
        app,
        "workflow/transcript/list",
        json!({
            "threadId":owner,"workflowId":run,"target":target,"limit":100,
        }),
    )
    .await
}

async fn lifecycle(
    app: &mut app_test_support::TestAppServer,
    owner: &str,
) -> Result<Vec<WorkflowLifecycleEvent>> {
    let mut cursor: Option<String> = None;
    let mut events = Vec::new();
    loop {
        let page: ThreadTimelineListResponse = rpc(
            app,
            "thread/timeline/list",
            json!({
                "threadId":owner,"cursor":cursor,"limit":100,
            }),
        )
        .await?;
        events.extend(page.data.into_iter().filter_map(|entry| match entry {
            ThreadTimelineEntry::Workflow { event, .. } => Some(*event),
            _ => None,
        }));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(events)
}

async fn native_presentation(
    app: &mut app_test_support::TestAppServer,
    group: &codex_app_server_protocol::WorkflowTranscriptGroup,
) -> Result<codex_app_server_protocol::WorkflowAgentMessageProvenance> {
    use codex_app_server_protocol::ThreadItem;
    use codex_app_server_protocol::WorkflowAgentMessageFormat;
    use codex_app_server_protocol::WorkflowAgentMessageProvenance;
    let proof = WorkflowAgentMessageProvenance {
        run_id: group.run_id.clone(),
        execution_id: group.execution_id.clone(),
        attempt_id: group.attempt_id.clone(),
        child_thread_id: group.child_thread_id.clone(),
        turn_id: group.final_turn_id.clone().context("owned final turn")?,
        item_id: group
            .final_item_id
            .clone()
            .context("accepted final native item")?,
        format: match &group.final_result {
            Some(codex_app_server_protocol::WorkflowTranscriptResult::Value { value })
                if value.is_string() =>
            {
                WorkflowAgentMessageFormat::Text
            }
            _ => WorkflowAgentMessageFormat::Json,
        },
    };
    let expected = group
        .items
        .iter()
        .find(|entry| entry.item.id() == proof.item_id)
        .context("scoped native final item")?;
    assert!(
        matches!(&expected.item,ThreadItem::AgentMessage{workflow:Some(actual),..} if actual==&proof)
    );
    let direct: codex_app_server_protocol::ThreadItemsListResponse = rpc(
        app,
        "thread/items/list",
        json!({"threadId":group.child_thread_id,"turnId":proof.turn_id,"limit":100}),
    )
    .await?;
    assert_eq!(
        direct
            .data
            .iter()
            .find(|entry| entry.item.id() == proof.item_id),
        Some(expected),
        "direct worker history uses the same exact proof and unchanged native text"
    );
    let page: ThreadTimelineListResponse = rpc(
        app,
        "thread/timeline/list",
        json!({"threadId":group.child_thread_id,"limit":100}),
    )
    .await?;
    let native = page.data.iter().find_map(|entry| match entry {
        ThreadTimelineEntry::Item { turn_id, item, .. }
            if turn_id == &proof.turn_id && item.id() == proof.item_id =>
        {
            Some(item.as_ref())
        }
        _ => None,
    });
    assert_eq!(native, Some(&expected.item));
    let read: codex_app_server_protocol::ThreadReadResponse = rpc(
        app,
        "thread/read",
        json!({"threadId":group.child_thread_id,"includeTurns":true}),
    )
    .await?;
    assert_eq!(
        read.thread
            .turns
            .iter()
            .find(|turn| turn.id == proof.turn_id)
            .and_then(|turn| turn.items.iter().find(|item| item.id() == proof.item_id)),
        Some(&expected.item)
    );
    Ok(proof)
}

#[tokio::test]
async fn native_workflow_finals_keep_null_and_json_looking_strings_distinct_on_cold_read()
-> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let home = TempDir::new()?;
    let server = responses::start_mock_server().await;
    let provider = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_assistant_message("null-final", "null"),
                responses::ev_completed_with_tokens("null-final", /*total_tokens*/ 3),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("text-final", r#""{\"typed\":true}""#),
                responses::ev_completed_with_tokens("text-final", /*total_tokens*/ 5),
            ]),
        ],
    )
    .await;
    configure_agent_workflows(&home, &server).await?;
    let mut app = client(&home).await?;
    let owner = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let started:WorkflowStartResponse=rpc(&mut app,"workflow/start",json!({"threadId":owner,"input":{},"source":
        "workflow.meta({name:'typed-finals',description:'Keep JSON null distinct from text that looks like JSON',phases:[]}); const empty=await workflow.agent('Return JSON null.',{label:'null'}); const text=await workflow.agent('Return a JSON string.',{label:'text'}); return {empty,text};"
    })).await?;
    let run = terminal(&mut app, &started.workflow.id).await?;
    assert_eq!(run.status, WorkflowStatus::Succeeded);
    lifecycle_finished(&mut app, &run.id).await?;
    let grouped = transcript(&mut app, &owner, &run.id, json!({"type":"run"})).await?;
    assert_eq!(grouped.data.len(), 2);
    let empty = grouped
        .data
        .iter()
        .find(|group| group.label == "null")
        .context("null worker")?;
    let text = grouped
        .data
        .iter()
        .find(|group| group.label == "text")
        .context("text worker")?;
    assert_eq!(
        empty.final_result,
        Some(codex_app_server_protocol::WorkflowTranscriptResult::Value { value: Value::Null })
    );
    assert_eq!(
        text.final_result,
        Some(codex_app_server_protocol::WorkflowTranscriptResult::Value {
            value: json!(r#"{"typed":true}"#)
        })
    );
    app.shutdown_gracefully().await?;
    let mut cold = client(&home).await?;
    assert_eq!(
        native_presentation(&mut cold, empty).await?.format,
        codex_app_server_protocol::WorkflowAgentMessageFormat::Json
    );
    assert_eq!(
        native_presentation(&mut cold, text).await?.format,
        codex_app_server_protocol::WorkflowAgentMessageFormat::Text
    );
    let loaded: ThreadLoadedListResponse = rpc(&mut cold, "thread/loaded/list", json!({})).await?;
    assert!(loaded.data.is_empty());
    assert_eq!(
        provider.requests().len(),
        2,
        "cold presentation reads never run a model or restore a workflow service"
    );
    cold.shutdown_gracefully().await?;
    let mut stable = app_test_support::TestAppServer::builder()
        .with_codex_home(home.path())
        .without_managed_config()
        .build()
        .await?;
    let initialized = stable
        .initialize_with_capabilities(
            codex_app_server_protocol::ClientInfo {
                name: "stable-history-client".into(),
                title: None,
                version: "1".into(),
            },
            Some(codex_app_server_protocol::InitializeCapabilities::default()),
        )
        .await?;
    assert!(matches!(
        initialized,
        codex_app_server_protocol::JSONRPCMessage::Response(_)
    ));
    for method in ["thread/read", "thread/resume"] {
        if method == "thread/resume" {
            let loaded: ThreadLoadedListResponse =
                rpc(&mut stable, "thread/loaded/list", json!({})).await?;
            assert!(
                loaded.data.is_empty(),
                "stable cold reads must not restore any runtime"
            );
            // V2 children retain the normal requirement to resume their parent
            // first. Explicit activation follows all read-only isolation checks.
            let _: Value = rpc(
                &mut stable,
                "thread/resume",
                json!({"threadId":owner,"excludeTurns":true}),
            )
            .await?;
        }
        for group in [empty, text] {
            let params = if method == "thread/read" {
                json!({"threadId":group.child_thread_id,"includeTurns":true})
            } else {
                json!({"threadId":group.child_thread_id,"excludeTurns":false})
            };
            let response: Value = rpc(&mut stable, method, params).await?;
            let final_item = response["thread"]["turns"]
                .as_array()
                .context("native turns on the actual stable wire")?
                .iter()
                .flat_map(|turn| turn["items"].as_array().into_iter().flatten())
                .find(|item| {
                    Some(item["id"].as_str().unwrap_or_default()) == group.final_item_id.as_deref()
                })
                .context("the accepted native final on the actual stable wire")?;
            assert!(
                final_item.get("workflow").is_none(),
                "nonexperimental native history must omit the field, not serialize null"
            );
            let original =
                group
                    .items
                    .iter()
                    .find_map(|entry| match &entry.item {
                        codex_app_server_protocol::ThreadItem::AgentMessage {
                            id, text, ..
                        } if Some(id.as_str()) == group.final_item_id.as_deref() => Some(text),
                        _ => None,
                    })
                    .context("unchanged native text")?;
            assert_eq!(final_item["text"], *original);
        }
    }
    assert_eq!(
        provider.requests().len(),
        2,
        "stable presentation and resume must not infer"
    );
    stable.shutdown_gracefully().await?;
    Ok(())
}

#[tokio::test]
async fn grouped_transcripts_exclude_other_runs_and_later_human_turns_and_read_cold_without_resume()
-> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let home = TempDir::new()?;
    let server = responses::start_mock_server().await;
    let provider = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_assistant_message("unrelated", "UNRELATED_ROOT_SENTINEL"),
                responses::ev_completed("unrelated"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("left", r#"{"finding":"LEFT_RESULT_PRIVATE"}"#),
                responses::ev_completed_with_tokens("left", /*total_tokens*/ 3),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("right", r#"{"finding":"RIGHT_RESULT_PRIVATE"}"#),
                responses::ev_completed_with_tokens("right", /*total_tokens*/ 5),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("other-run", r#"{"other":"SECOND_RUN_PRIVATE"}"#),
                responses::ev_completed_with_tokens("other-run", /*total_tokens*/ 7),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("later-human", "LATER_INDEPENDENT_HUMAN_RESULT"),
                responses::ev_completed_with_tokens("later-human", /*total_tokens*/ 11),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message(
                    "owner-human",
                    "The workflow reports are available.",
                ),
                responses::ev_completed("owner-human"),
            ]),
        ],
    )
    .await;
    configure_agent_workflows(&home, &server).await?;
    let mut app = client(&home).await?;
    let owner = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let unrelated = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let _: TurnStartResponse = rpc(&mut app, "turn/start", json!({"threadId":unrelated,"input":[{"type":"text","text":"UNRELATED_ROOT_PROMPT","textElements":[]}]})).await?;
    turn_finished(&mut app, &unrelated).await?;
    let source = "workflow.meta({name:'paired-review',description:'Review both requested files',phases:[{title:'Review'},{title:'Finish'}]}); workflow.phase('Review'); const left=await workflow.agent('REVIEW_LEFT_PROMPT',{label:'left'}); const right=await workflow.agent('REVIEW_RIGHT_PROMPT',{label:'right'}); workflow.phase('Finish'); return {left,right};";
    let started: WorkflowStartResponse = rpc(
        &mut app,
        "workflow/start",
        json!({"threadId":owner,"source":source,"input":{}}),
    )
    .await?;
    let run = terminal(&mut app, &started.workflow.id).await?;
    assert_eq!(run.status, WorkflowStatus::Succeeded);
    lifecycle_finished(&mut app, &run.id).await?;
    let second: WorkflowStartResponse = rpc(&mut app, "workflow/start", json!({"threadId":owner,"source":"workflow.meta({name:'other-run',description:'An independent run on the same owner',phases:[]}); return await workflow.agent('SECOND_RUN_PROMPT',{label:'other'});","input":{}})).await?;
    let other = terminal(&mut app, &second.workflow.id).await?;
    assert_eq!(other.status, WorkflowStatus::Succeeded);
    lifecycle_finished(&mut app, &other.id).await?;
    assert_eq!(
        provider.requests().len(),
        4,
        "display completion must not manufacture a parent model request"
    );
    let grouped = transcript(&mut app, &owner, &run.id, json!({"type":"run"})).await?;
    assert_eq!(grouped.limitation, None);
    assert_eq!(grouped.next_cursor, None);
    assert_eq!(grouped.data.len(), 2);
    let labels = grouped
        .data
        .iter()
        .map(|group| group.label.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(labels, BTreeSet::from(["left", "right"]));
    for group in &grouped.data {
        assert_eq!(
            (&group.run_id, &group.execution_id, group.phase.as_deref()),
            (&run.id, &run.execution_id, Some("Review"))
        );
        assert_eq!(group.resolved_model.as_deref(), Some("gpt-5.6-sol"));
        assert!(!group.owned_turn_ids.is_empty() && !group.items.is_empty());
        assert!(
            group
                .items
                .iter()
                .all(|item| group.owned_turn_ids.contains(&item.turn_id))
        );
        let final_turn = group
            .final_turn_id
            .as_deref()
            .context("the authoritative final turn identity")?;
        assert!(group.owned_turn_ids.iter().any(|turn| turn == final_turn));
        let final_message = group
            .items
            .iter()
            .rev()
            .find_map(|entry| match &entry.item {
                codex_app_server_protocol::ThreadItem::AgentMessage { id, text, .. }
                    if entry.turn_id == final_turn =>
                {
                    Some((id, text))
                }
                _ => None,
            })
            .context("the normal final assistant item in its actual owned turn")?;
        assert_eq!(
            group.final_item_id.as_deref(),
            Some(final_message.0.as_str())
        );
        assert_eq!(
            group.final_result,
            Some(codex_app_server_protocol::WorkflowTranscriptResult::Value {
                value: serde_json::from_str(final_message.1)?
            }),
            "highlight the authoritative typed final reply, not any earlier assistant item"
        );
        let text = serde_json::to_string(&group.items)?;
        assert!(text.contains(if group.label == "left" {
            "LEFT_RESULT_PRIVATE"
        } else {
            "RIGHT_RESULT_PRIVATE"
        }));
        assert!(!text.contains("SECOND_RUN_PRIVATE") && !text.contains("UNRELATED_ROOT_SENTINEL"));
    }
    let phase = transcript(
        &mut app,
        &owner,
        &run.id,
        json!({"type":"phase","phase":"Review"}),
    )
    .await?;
    assert_eq!(
        phase, grouped,
        "a phase groups both actual children rather than opening its first child only"
    );
    let left = grouped
        .data
        .iter()
        .find(|group| group.label == "left")
        .context("left native conversation")?;
    let single = transcript(
        &mut app,
        &owner,
        &run.id,
        json!({"type":"step","stepId":left.step_id}),
    )
    .await?;
    assert_eq!(single.data, vec![left.clone()]);
    let attempt = transcript(
        &mut app,
        &owner,
        &run.id,
        json!({"type":"attempt","stepId":left.step_id,"attemptId":left.attempt_id}),
    )
    .await?;
    assert_eq!(attempt, single);
    let expected_items = grouped
        .data
        .iter()
        .flat_map(|group| {
            group.items.iter().map(|item| {
                (
                    group.child_thread_id.clone(),
                    item.turn_id.clone(),
                    serde_json::to_string(&item.item).expect("serialize the actual item"),
                )
            })
        })
        .collect::<BTreeSet<_>>();
    let mut cursor: Option<String> = None;
    let mut paged_items = BTreeSet::new();
    let mut first_cursor = None;
    for _ in 0..32 {
        let page: WorkflowTranscriptListResponse = rpc(&mut app,"workflow/transcript/list",json!({
            "threadId":owner,"workflowId":run.id,"target":{"type":"run"},"cursor":cursor,"limit":1,
        })).await?;
        assert_eq!(page.limitation, None);
        assert_eq!(
            page.data
                .iter()
                .map(|group| group.items.len())
                .sum::<usize>(),
            1
        );
        for group in page.data {
            for item in group.items {
                assert!(
                    paged_items.insert((
                        group.child_thread_id.clone(),
                        item.turn_id,
                        serde_json::to_string(&item.item)?
                    )),
                    "an owned transcript item must be delivered once across page boundaries"
                );
            }
        }
        match page.next_cursor {
            Some(next) => {
                if first_cursor.is_none() {
                    first_cursor = Some(next.clone());
                }
                cursor = Some(next);
            }
            None => break,
        }
    }
    assert_eq!(
        paged_items, expected_items,
        "bounded pages must retain every owned worker item across child boundaries"
    );
    let foreign = transcript(&mut app, &owner, &other.id, json!({"type":"run"})).await?;
    assert_eq!(foreign.data.len(), 1);
    assert_eq!(foreign.data[0].label, "other");
    let first_cursor = first_cursor
        .context("a multi-item transcript must provide a bounded continuation cursor")?;
    rejected_transcript(&mut app,json!({"threadId":owner,"workflowId":other.id,"target":{"type":"run"},"cursor":first_cursor})).await?;
    rejected_transcript(&mut app,json!({"threadId":owner,"workflowId":run.id,"target":{"type":"phase","phase":"Review"},"cursor":first_cursor})).await?;
    rejected_transcript(&mut app,json!({"threadId":owner,"workflowId":run.id,"target":{"type":"run"},"executionId":"stale-execution"})).await?;
    rejected_transcript(
        &mut app,
        json!({"threadId":unrelated,"workflowId":run.id,"target":{"type":"run"}}),
    )
    .await?;
    let before = lifecycle(&mut app, &owner).await?;
    let run_events = before
        .iter()
        .filter(|event| event.run_id == run.id)
        .collect::<Vec<_>>();
    assert_eq!(
        run_events
            .iter()
            .filter(|event| matches!(event.kind, WorkflowLifecycleKind::Started { .. }))
            .count(),
        1
    );
    assert_eq!(
        run_events
            .iter()
            .filter(|event| matches!(event.kind, WorkflowLifecycleKind::StepCompleted { .. }))
            .count(),
        2
    );
    let completed = run_events
        .iter()
        .filter_map(|event| match &event.kind {
            WorkflowLifecycleKind::Completed { result, .. } => Some(result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        completed,
        vec![&WorkflowResult::Value {
            value: run.result.clone().context("original full result")?
        }]
    );
    assert_eq!(
        before
            .iter()
            .map(|event| &event.id)
            .collect::<BTreeSet<_>>()
            .len(),
        before.len()
    );
    let displayed: WorkflowResultDisplayResponse = rpc(
        &mut app,
        "workflow/result/display",
        json!({
            "threadId":owner,"workflowId":run.id,"executionId":run.execution_id,
        }),
    )
    .await?;
    let automatic = run_events
        .iter()
        .find(|event| matches!(event.kind, WorkflowLifecycleKind::Completed { .. }))
        .context("durable automatic run completion")?;
    assert_ne!(
        displayed.event.id, automatic.id,
        "explicit result display has its own identity, separate from automatic completion"
    );
    assert_eq!(
        (&displayed.event.run_id, &displayed.event.execution_id),
        (&run.id, &run.execution_id)
    );
    assert_eq!(
        displayed.event.kind,
        WorkflowLifecycleKind::ResultShown {
            name: "paired-review".into(),
            status: WorkflowStatus::Succeeded,
            result: WorkflowResult::Value {
                value: run.result.clone().context("retained full result")?
            },
            error: None,
        }
    );
    let repeated: WorkflowResultDisplayResponse = rpc(
        &mut app,
        "workflow/result/display",
        json!({
            "threadId":owner,"workflowId":run.id,"executionId":run.execution_id,
        }),
    )
    .await?;
    assert_eq!(
        repeated.event, displayed.event,
        "repeated explicit display must reuse the same immutable event identity and value"
    );
    let displayed_history = lifecycle(&mut app, &owner).await?;
    assert_eq!(displayed_history.len(), before.len() + 1);
    assert_eq!(displayed_history[..before.len()], before);
    assert_eq!(displayed_history.last(), Some(&displayed.event));
    assert_eq!(
        provider.requests().len(),
        4,
        "displaying a retained result must not invoke the model"
    );
    let _: TurnStartResponse = rpc(&mut app, "turn/start", json!({"threadId":left.child_thread_id,"input":[{"type":"text","text":"LATER_INDEPENDENT_HUMAN_PROMPT","textElements":[]}]})).await?;
    turn_finished(&mut app, &left.child_thread_id).await?;
    let after_human = transcript(&mut app, &owner, &run.id, json!({"type":"run"})).await?;
    assert_eq!(
        after_human, grouped,
        "a later independent human turn must not expand the workflow's worker history"
    );
    let _: TurnStartResponse = rpc(&mut app, "turn/start", json!({"threadId":owner,"input":[{"type":"text","text":"Read the workflow summaries without opening their results.","textElements":[]}]})).await?;
    turn_finished(&mut app, &owner).await?;
    let requests = provider.requests();
    assert_eq!(requests.len(), 6);
    assert!(
        !requests[5].body_contains_text("LEFT_RESULT_PRIVATE")
            && !requests[5].body_contains_text("RIGHT_RESULT_PRIVATE")
            && !requests[5].body_contains_text("SECOND_RUN_PRIVATE"),
        "full human-visible lifecycle results must not become model context"
    );
    app.shutdown_gracefully().await?;
    let mut cold = client(&home).await?;
    let loaded: ThreadLoadedListResponse = rpc(&mut cold, "thread/loaded/list", json!({})).await?;
    assert!(loaded.data.is_empty());
    // Native history must work before opening the workflow center or restoring
    // either the worker or its owning workflow runtime.
    let native_proof = native_presentation(&mut cold, left).await?;
    let native_history: codex_app_server_protocol::ThreadItemsListResponse = rpc(
        &mut cold,
        "thread/items/list",
        json!({"threadId":left.child_thread_id,"limit":100}),
    )
    .await?;
    let later = native_history
        .data
        .iter()
        .find_map(|entry| match &entry.item {
            codex_app_server_protocol::ThreadItem::AgentMessage { text, workflow, .. }
                if text == "LATER_INDEPENDENT_HUMAN_RESULT" =>
            {
                Some(workflow)
            }
            _ => None,
        })
        .context("the later independent HUMAN reply remains in normal native history")?;
    assert_eq!(
        later, &None,
        "a later HUMAN reply must never inherit workflow formatting"
    );
    assert_eq!(
        transcript(&mut cold, &owner, &run.id, json!({"type":"run"})).await?,
        grouped
    );
    assert_eq!(lifecycle(&mut cold, &owner).await?, displayed_history);
    let loaded: ThreadLoadedListResponse = rpc(&mut cold, "thread/loaded/list", json!({})).await?;
    assert!(
        loaded.data.is_empty(),
        "cold transcript and timeline reads must not resume any runtime"
    );
    assert_eq!(
        provider.requests().len(),
        6,
        "reconnection and durable history reads must not wake inference"
    );
    let _: codex_app_server_protocol::ThreadResumeResponse = rpc(
        &mut cold,
        "thread/resume",
        json!({"threadId":owner,"excludeTurns":true}),
    )
    .await?;
    let resumed: codex_app_server_protocol::ThreadResumeResponse = rpc(
        &mut cold,
        "thread/resume",
        json!({"threadId":left.child_thread_id}),
    )
    .await?;
    assert!(resumed.thread.turns.iter().flat_map(|turn|&turn.items).any(|item|matches!(item,codex_app_server_protocol::ThreadItem::AgentMessage{workflow:Some(proof),..} if proof==&native_proof)),"direct cold worker resume must retain exact formatting without opening the center");
    assert_eq!(
        provider.requests().len(),
        6,
        "presentation-only resume must not infer"
    );
    cold.shutdown_gracefully().await?;
    Ok(())
}
