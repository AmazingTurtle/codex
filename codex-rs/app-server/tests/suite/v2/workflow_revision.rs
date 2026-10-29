//! Public revision reads keep old source, typed results and owned conversations immutable.

use anyhow::Context;
use anyhow::Result;
use codex_app_server_protocol::ThreadLoadedListResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::WorkflowControlListResponse;
use codex_app_server_protocol::WorkflowExecutionReadResponse;
use codex_app_server_protocol::WorkflowResult;
use codex_app_server_protocol::WorkflowRevisionApplyResponse;
use codex_app_server_protocol::WorkflowRevisionListResponse;
use codex_app_server_protocol::WorkflowRevisionResumeResponse;
use codex_app_server_protocol::WorkflowRevisionStageResponse;
use codex_app_server_protocol::WorkflowRevisionStatus;
use codex_app_server_protocol::WorkflowStartResponse;
use codex_app_server_protocol::WorkflowStatus;
use codex_app_server_protocol::WorkflowTranscriptListResponse;
use core_test_support::responses;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;

use super::workflows::client;
use super::workflows::configure_agent_workflows;
use super::workflows::rpc;
use super::workflows::terminal;

async fn completed(
    app: &mut app_test_support::TestAppServer,
    run: &str,
    execution: &str,
) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        app.read_stream_until_matching_notification(
            "the selected native execution has a durable final result",
            |notice| {
                notice.method == "workflow/lifecycle/changed"
                    && notice.params.as_ref().is_some_and(|params| {
                        params["event"]["runId"] == run
                            && params["event"]["executionId"] == execution
                            && params["event"]["kind"]["type"] == "completed"
                    })
            },
        ),
    )
    .await??;
    Ok(())
}

async fn old_execution(
    app: &mut app_test_support::TestAppServer,
    owner: &str,
    run: &str,
    execution: &str,
) -> Result<WorkflowExecutionReadResponse> {
    rpc(
        app,
        "workflow/execution/read",
        json!({"threadId":owner,"workflowId":run,"executionId":execution}),
    )
    .await
}

async fn old_chat(
    app: &mut app_test_support::TestAppServer,
    owner: &str,
    run: &str,
    execution: &str,
) -> Result<WorkflowTranscriptListResponse> {
    rpc(app,"workflow/transcript/list",json!({"threadId":owner,"workflowId":run,"executionId":execution,"target":{"type":"run"},"limit":100})).await
}

async fn human_completed(
    app: &mut app_test_support::TestAppServer,
    thread: &str,
    turn: &str,
) -> Result<()> {
    let notice = tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        app.read_stream_until_matching_notification(
            "the exact newly started independent human turn completes",
            |notice| {
                notice.method == "turn/completed"
                    && notice.params.as_ref().is_some_and(|params| {
                        params["threadId"] == thread && params["turn"]["id"] == turn
                    })
            },
        ),
    )
    .await??;
    let finished: codex_app_server_protocol::TurnCompletedNotification = serde_json::from_value(
        notice
            .params
            .context("the exact completed human turn payload")?,
    )?;
    assert_eq!(
        (
            finished.thread_id,
            finished.turn.id,
            finished.turn.status,
            finished.turn.error
        ),
        (
            thread.to_owned(),
            turn.to_owned(),
            codex_app_server_protocol::TurnStatus::Completed,
            None
        )
    );
    Ok(())
}

async fn denied(
    app: &mut app_test_support::TestAppServer,
    method: &str,
    params: Value,
) -> Result<()> {
    let request = app.send_request(method, Some(params)).await?;
    let error = tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        app.read_stream_until_error_message(codex_app_server_protocol::RequestId::Integer(request)),
    )
    .await??;
    assert_eq!(error.error.code, -32600);
    Ok(())
}

#[tokio::test]
async fn appended_revision_keeps_prior_execution_and_owned_chat_readable_cold_without_resuming()
-> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let home = TempDir::new()?;
    let provider = responses::start_mock_server().await;
    let model = responses::mount_sse_sequence(
        &provider,
        vec![
            responses::sse(vec![
                responses::ev_assistant_message(
                    "base-original",
                    r#"{"base":"ORIGINAL_PRIVATE_RESULT"}"#,
                ),
                responses::ev_completed_with_tokens("base-original", /*total_tokens*/ 2),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message(
                    "extra-appended",
                    r#"{"extra":"APPENDED_PRIVATE_RESULT"}"#,
                ),
                responses::ev_completed_with_tokens("extra-appended", /*total_tokens*/ 3),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("later-human", "UNRELATED_LATER_HUMAN_RESULT"),
                responses::ev_completed_with_tokens("later-human", /*total_tokens*/ 13),
            ]),
        ],
    )
    .await;
    configure_agent_workflows(&home, &provider).await?;
    let mut app = client(&home).await?;
    let owner = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let foreign = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let _: Value = rpc(
        &mut app,
        "thread/name/set",
        json!({"threadId":foreign,"name":"Independent persisted owner"}),
    )
    .await?;
    let metadata = "workflow.meta({name:'append-history',description:'Append new work without rewriting accepted history',phases:[]});";
    let prefix =
        "const base = await workflow.agent('BASE_ORIGINAL_PROMPT',{key:'base',label:'base'});";
    let original_source = format!("{metadata}\n{prefix}\nreturn {{base}};");
    let revised_source = format!(
        "{metadata}\n{prefix}\nconst extra = await workflow.agent('EXTRA_APPENDED_PROMPT',{{key:'extra',label:'extra'}});\nreturn {{base,extra}};"
    );
    let started: WorkflowStartResponse = rpc(
        &mut app,
        "workflow/start",
        json!({"threadId":owner,"source":original_source,"input":{"retained":true}}),
    )
    .await?;
    let original = terminal(&mut app, &started.workflow.id).await?;
    assert_eq!(original.status, WorkflowStatus::Succeeded);
    completed(&mut app, &original.id, &original.execution_id).await?;
    let snapshot = old_execution(&mut app, &owner, &original.id, &original.execution_id).await?;
    assert_eq!(
        (
            &snapshot.execution.source,
            &snapshot.execution.input,
            &snapshot.execution.result
        ),
        (
            &original_source,
            &json!({"retained":true}),
            &WorkflowResult::Value {
                value: json!({"base":{"base":"ORIGINAL_PRIVATE_RESULT"}})
            }
        )
    );
    let chat = old_chat(&mut app, &owner, &original.id, &original.execution_id).await?;
    assert_eq!(chat.limitation, None);
    assert_eq!(chat.next_cursor, None);
    assert_eq!(chat.data.len(), 1);
    let base = &chat.data[0];
    assert_eq!(base.label, "base");
    assert!(
        !base.owned_turn_ids.is_empty()
            && base.final_turn_id.is_some()
            && base.final_item_id.is_some()
    );
    let state: WorkflowRevisionListResponse = rpc(
        &mut app,
        "workflow/revision/list",
        json!({"threadId":owner,"workflowId":original.id}),
    )
    .await?;
    let staged:WorkflowRevisionStageResponse=rpc(&mut app,"workflow/revision/stage",json!({
        "threadId":owner,"workflowId":original.id,"commandId":"append-stage","expectedControlRevision":state.state.control_revision,
        "source":revised_source,"input":{"type":"absent"},
    })).await?;
    assert_eq!(staged.revision.status, WorkflowRevisionStatus::Draft);
    assert!(staged.revision.reusable_statements >= 1);
    let _:WorkflowRevisionApplyResponse=rpc(&mut app,"workflow/revision/apply",json!({
        "threadId":owner,"workflowId":original.id,"commandId":"append-apply","expectedControlRevision":staged.state.control_revision,
        "revisionId":staged.revision.id,"mode":"drainAccepted",
    })).await?;
    let ready = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
        loop {
            let list: WorkflowRevisionListResponse = rpc(
                &mut app,
                "workflow/revision/list",
                json!({"threadId":owner,"workflowId":original.id}),
            )
            .await?;
            if list.state.status == WorkflowRevisionStatus::Ready {
                break Ok::<_, anyhow::Error>(list);
            }
            ticks.tick().await;
        }
    })
    .await??;
    assert_eq!(
        model.requests().len(),
        1,
        "staging and draining must not dispatch the new suffix"
    );
    let _:WorkflowRevisionResumeResponse=rpc(&mut app,"workflow/revision/resume",json!({
        "threadId":owner,"workflowId":original.id,"commandId":"append-resume","expectedControlRevision":ready.state.control_revision,"revisionId":staged.revision.id,
    })).await?;
    let revised = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        loop {
            let notice: codex_app_server_protocol::WorkflowUpdatedNotification =
                app.read_notification("workflow/updated").await?;
            if notice.workflow.id == original.id
                && notice.workflow.execution_id != original.execution_id
                && notice.workflow.status == WorkflowStatus::Succeeded
            {
                break Ok::<_, anyhow::Error>(notice.workflow);
            }
        }
    })
    .await??;
    completed(&mut app, &revised.id, &revised.execution_id).await?;
    assert_eq!(revised.source, revised_source);
    assert_eq!(
        revised.result,
        Some(
            json!({"base":{"base":"ORIGINAL_PRIVATE_RESULT"},"extra":{"extra":"APPENDED_PRIVATE_RESULT"}})
        )
    );
    assert_eq!(revised.accepted_starts, 2);
    assert_eq!(
        revised
            .metrics
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(5)
    );
    assert_eq!(
        model.requests().len(),
        2,
        "the accepted base must be reused rather than sampled again"
    );
    assert_eq!(
        old_execution(&mut app, &owner, &original.id, &original.execution_id).await?,
        snapshot
    );
    assert_eq!(
        old_chat(&mut app, &owner, &original.id, &original.execution_id).await?,
        chat
    );
    let revisions: WorkflowRevisionListResponse = rpc(
        &mut app,
        "workflow/revision/list",
        json!({"threadId":owner,"workflowId":original.id}),
    )
    .await?;
    assert!(
        revisions
            .data
            .iter()
            .any(|revision| revision.source == original_source
                && revision.execution_ids.contains(&original.execution_id))
    );
    assert!(
        revisions
            .data
            .iter()
            .any(|revision| revision.source == revised_source
                && revision.execution_ids.contains(&revised.execution_id))
    );
    let human:TurnStartResponse=rpc(&mut app,"turn/start",json!({"threadId":base.child_thread_id,"input":[{"type":"text","text":"Independent human task after the accepted result.","textElements":[]}]})).await?;
    human_completed(&mut app, &base.child_thread_id, &human.turn.id).await?;
    assert_eq!(
        old_chat(&mut app, &owner, &original.id, &original.execution_id).await?,
        chat
    );
    assert!(!serde_json::to_string(&chat)?.contains("UNRELATED_LATER_HUMAN_RESULT"));
    app.shutdown_gracefully().await?;
    let mut cold = client(&home).await?;
    let loaded: ThreadLoadedListResponse = rpc(&mut cold, "thread/loaded/list", json!({})).await?;
    assert!(loaded.data.is_empty());
    assert_eq!(
        old_execution(&mut cold, &owner, &original.id, &original.execution_id).await?,
        snapshot
    );
    assert_eq!(
        old_chat(&mut cold, &owner, &original.id, &original.execution_id).await?,
        chat
    );
    let current = old_execution(&mut cold, &owner, &revised.id, &revised.execution_id).await?;
    assert_eq!(
        (current.execution.source, current.execution.result),
        (
            revised_source,
            WorkflowResult::Value {
                value: revised.result.context("the revised full result")?
            }
        )
    );
    denied(
        &mut cold,
        "workflow/execution/read",
        json!({"threadId":owner,"workflowId":original.id,"executionId":"unknown-execution"}),
    )
    .await?;
    denied(&mut cold,"workflow/transcript/list",json!({"threadId":owner,"workflowId":original.id,"executionId":"unknown-execution","target":{"type":"run"}})).await?;
    denied(
        &mut cold,
        "workflow/execution/read",
        json!({"threadId":foreign,"workflowId":original.id,"executionId":original.execution_id}),
    )
    .await?;
    let loaded: ThreadLoadedListResponse = rpc(&mut cold, "thread/loaded/list", json!({})).await?;
    assert!(
        loaded.data.is_empty(),
        "immutable public reads must not load a runtime or resume either execution"
    );
    assert_eq!(model.requests().len(), 3);
    cold.shutdown_gracefully().await?;
    Ok(())
}

#[tokio::test]
async fn exact_active_target_is_not_limited_to_the_first_worker_and_never_adopts_a_later_human()
-> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let (release_first, first_gate) = tokio::sync::oneshot::channel();
    let (release_second, second_gate) = tokio::sync::oneshot::channel();
    let (release_human, human_gate) = tokio::sync::oneshot::channel();
    let tool_home = TempDir::new()?;
    let tool_gate = tool_home.path().join("release-native-tool");
    let emit_gate = tool_home.path().join("emit-native-tool-output");
    let gate_literal = tool_gate.to_string_lossy().replace('\'', "''");
    let emit_literal = emit_gate.to_string_lossy().replace('\'', "''");
    let tool_command = if cfg!(windows) {
        format!(
            "$emitted=$false; for ($i=0; $i -lt 600 -and !(Test-Path -LiteralPath '{gate_literal}'); $i++) {{ if (-not $emitted -and (Test-Path -LiteralPath '{emit_literal}')) {{ Write-Output 'PRESELECT_TOOL_OUTPUT'; $emitted=$true }}; Start-Sleep -Milliseconds 50 }}"
        )
    } else {
        format!(
            r#"i=0; while [ ! -e '{emit_literal}' ] && [ "$i" -lt 600 ]; do sleep 0.05; i=$((i+1)); done; if [ -e '{emit_literal}' ]; then printf 'PRESELECT_TOOL_OUTPUT\n'; fi; while [ ! -e '{gate_literal}' ] && [ "$i" -lt 600 ]; do sleep 0.05; i=$((i+1)); done"#
        )
    };
    let held = |id: &str, gate| {
        let message_id = format!("msg_{id}");
        let mut commentary = responses::ev_assistant_message(
            &format!("msg_commentary_{id}"),
            &format!("PRESELECT_COMMENTARY_{id}"),
        );
        commentary["item"]["phase"] = json!("commentary");
        let mut initial = vec![responses::ev_response_created(id)];
        if id != "later-human" {
            initial.push(responses::ev_function_call(
                &format!("call_{id}"),
                "exec_command",
                &json!({"cmd":tool_command,"yield_time_ms":1000,"max_output_tokens":1000})
                    .to_string(),
            ));
        }
        initial.extend([
            commentary,
            responses::ev_message_item_added(&message_id, ""),
            responses::ev_output_text_delta(r#"{"done":"#),
        ]);
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(initial),
            },
            StreamingSseChunk {
                gate: Some(gate),
                body: responses::sse(vec![
                    responses::ev_output_text_delta("true}"),
                    responses::ev_assistant_message(&message_id, r#"{"done":true}"#),
                    responses::ev_completed_with_tokens(id, /*total_tokens*/ 3),
                ]),
            },
        ]
    };
    let (provider, _) = start_streaming_sse_server(vec![
        held("first-native", first_gate),
        held("second-native", second_gate),
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_response_created("followup-one"),
                responses::ev_assistant_message("msg_followup_one", r#"{"done":true}"#),
                responses::ev_completed_with_tokens("followup-one", 3),
            ]),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_response_created("followup-two"),
                responses::ev_assistant_message("msg_followup_two", r#"{"done":true}"#),
                responses::ev_completed_with_tokens("followup-two", 3),
            ]),
        }],
        held("later-human", human_gate),
    ])
    .await;
    let home = TempDir::new()?;
    let bootstrap = responses::start_mock_server().await;
    configure_agent_workflows(&home, &bootstrap).await?;
    let path = home.path().join("config.toml");
    let mut config: toml::Value = toml::from_str(&std::fs::read_to_string(&path)?)?;
    *config
        .get_mut("model_providers")
        .and_then(toml::Value::as_table_mut)
        .and_then(|providers| providers.get_mut("mock_provider"))
        .and_then(toml::Value::as_table_mut)
        .and_then(|provider| provider.get_mut("base_url"))
        .context("the real mock provider URL")? =
        toml::Value::String(format!("{}/v1", provider.uri()));
    std::fs::write(path, toml::to_string(&config)?)?;
    let mut app = client(&home).await?;
    let owner = app
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let started:WorkflowStartResponse=rpc(&mut app,"workflow/start",json!({"threadId":owner,"source":
        "workflow.meta({name:'active-selection',description:'Select one exact live conversation from parallel native work',phases:[]}); return await workflow.parallel([()=>workflow.agent('Keep first native conversation active.',{key:'first',label:'first'}),()=>workflow.agent('Keep second native conversation active.',{key:'second',label:'second'})]);","input":{}})).await?;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        provider.wait_for_request_count(/*count*/ 2),
    )
    .await?;
    let both = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
        loop {
            let page: WorkflowControlListResponse = rpc(
                &mut app,
                "workflow/control/list",
                json!({"threadId":owner,"workflowId":started.workflow.id,"limit":50}),
            )
            .await?;
            if page.active_targets.len() == 2 {
                break Ok::<_, anyhow::Error>(page);
            }
            ticks.tick().await;
        }
    })
    .await??;
    assert!(both.data.is_empty());
    assert_eq!(both.next_cursor, None);
    let first: WorkflowControlListResponse = rpc(
        &mut app,
        "workflow/control/list",
        json!({"threadId":owner,"workflowId":started.workflow.id,"limit":1}),
    )
    .await?;
    assert_eq!(first.active_targets.len(), 1);
    let later = both
        .active_targets
        .iter()
        .find(|target| target.logical_id != first.active_targets[0].logical_id)
        .context("the live worker beyond the default one-entry active list")?
        .clone();
    let exact_target = json!({"type":"step","logicalId":later.logical_id,"expectedAttemptId":later.attempt_id,"expectedTurnId":null});
    let selected:WorkflowControlListResponse=rpc(&mut app,"workflow/control/list",json!({"threadId":owner,"workflowId":started.workflow.id,"limit":1,"activeTarget":exact_target})).await?;
    assert_eq!(
        selected.active_targets,
        vec![later.clone()],
        "exact hydration must select the requested later worker rather than truncate before matching"
    );
    assert_eq!(
        (&selected.data, &selected.next_cursor, &selected.state),
        (&first.data, &first.next_cursor, &first.state)
    );
    let stale:WorkflowControlListResponse=rpc(&mut app,"workflow/control/list",json!({"threadId":owner,"workflowId":started.workflow.id,"limit":1,
        "activeTarget":{"type":"step","logicalId":later.logical_id,"expectedAttemptId":uuid::Uuid::new_v4().to_string(),"expectedTurnId":null}})).await?;
    assert!(stale.active_targets.is_empty());
    denied(&mut app,"workflow/control/list",json!({"threadId":owner,"workflowId":started.workflow.id,"limit":1,
        "activeTarget":{"type":"step","logicalId":later.logical_id,"expectedAttemptId":null,"expectedTurnId":null}})).await?;
    assert_eq!(provider.requests().await.len(), 2);
    let current = rpc::<codex_app_server_protocol::WorkflowReadResponse>(
        &mut app,
        "workflow/read",
        json!({"threadId":owner,"workflowId":started.workflow.id}),
    )
    .await?
    .workflow;
    // Existing app-server clients automatically subscribe to every newly created
    // thread. Remove those ordinary subscriptions before testing what the scoped
    // inspector itself attaches. The initial per-child wake snapshot proves each
    // asynchronous auto-attachment has finished before it is unsubscribed.
    let mut registered_tools = std::collections::BTreeMap::new();
    for child in current.steps.values().filter_map(|step| {
        step.attempts
            .last()
            .and_then(|attempt| attempt.child_thread_id.as_ref())
    }) {
        tokio::time::timeout(
            Duration::from_secs(/*secs*/ 30),
            app.read_stream_until_matching_notification(
                "the existing child listener has attached this client",
                |notice| {
                    notice.method == "thread/wakeState/changed"
                        && notice
                            .params
                            .as_ref()
                            .is_some_and(|params| params["threadId"] == *child)
                },
            ),
        )
        .await??;
        // The process registry is populated only after unified exec has
        // subscribed its output receiver. A startup printf could precede that
        // subscription, so release real output only after this causal barrier.
        let item_id = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
            let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
            loop {
                let page: codex_app_server_protocol::ThreadBackgroundTerminalsListResponse = rpc(
                    &mut app,
                    "thread/backgroundTerminals/list",
                    json!({"threadId":child,"limit":10}),
                )
                .await?;
                if let Some(terminal) = page.data.first() {
                    assert_eq!(page.data.len(), 1);
                    assert!(matches!(
                        terminal.item_id.as_str(),
                        "call_first-native" | "call_second-native"
                    ));
                    break Ok::<_, anyhow::Error>(terminal.item_id.clone());
                }
                ticks.tick().await;
            }
        })
        .await??;
        registered_tools.insert(child.clone(), item_id);
    }
    assert_eq!(
        registered_tools
            .values()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from([
            "call_first-native".to_owned(),
            "call_second-native".to_owned()
        ])
    );
    std::fs::write(
        &emit_gate,
        "emit after both actual output receivers are registered",
    )?;
    for (child, item_id) in &registered_tools {
        tokio::time::timeout(
            Duration::from_secs(/*secs*/ 30),
            app.read_stream_until_matching_notification(
                "the actual native worker has already started streaming before scoped inspection",
                |notice| {
                    notice.method == "item/agentMessage/delta"
                        && notice.params.as_ref().is_some_and(|params| {
                            params["threadId"] == *child && params["delta"] == r#"{"done":"#
                        })
                },
            ),
        )
        .await??;
        tokio::time::timeout(
            Duration::from_secs(/*secs*/ 30),
            app.read_stream_until_matching_notification(
                "the actual native exec command has emitted output before scoped selection",
                |notice| {
                    notice.method == "item/commandExecution/outputDelta"
                        && notice.params.as_ref().is_some_and(|params| {
                            params["threadId"] == *child
                                && params["itemId"] == *item_id
                                && params["delta"]
                                    .as_str()
                                    .is_some_and(|text| text.contains("PRESELECT_TOOL_OUTPUT"))
                        })
                },
            ),
        )
        .await??;
        let unsubscribed: codex_app_server_protocol::ThreadUnsubscribeResponse =
            rpc(&mut app, "thread/unsubscribe", json!({"threadId":child})).await?;
        assert_eq!(
            unsubscribed.status,
            codex_app_server_protocol::ThreadUnsubscribeStatus::Unsubscribed
        );
    }
    // The initial native start and prefix are deliberately no longer buffered on
    // this connection. Inspection must recover them from exact active state.
    app.clear_message_buffer();
    assert_eq!(provider.requests().await.len(), 2);
    let selected_step = current
        .steps
        .values()
        .find(|step| step.logical_id == later.logical_id)
        .context("the exact current step behind the selected control target")?;
    let live:WorkflowTranscriptListResponse=rpc(&mut app,"workflow/transcript/list",json!({
        "threadId":owner,"workflowId":started.workflow.id,"target":{"type":"attempt","stepId":selected_step.id,"attemptId":later.attempt_id},"limit":100,
    })).await?;
    assert_eq!(live.limitation, None);
    assert_eq!(
        live.data.len(),
        1,
        "the selected real native task and already-streaming conversation are immediately readable"
    );
    let selected_chat = &live.data[0];
    assert_eq!(selected_chat.attempt_id, later.attempt_id);
    assert!(selected_chat.owned_turn_ids.contains(&later.turn_id));
    assert!(selected_chat.items.iter().any(|entry|matches!(&entry.item,codex_app_server_protocol::ThreadItem::AgentMessage {text,..} if text==r#"{"done":"#)));
    let mut sample = None;
    for (index, raw) in provider.requests().await.iter().enumerate() {
        let body: Value = serde_json::from_slice(raw)?;
        let metadata: Value = serde_json::from_str(
            body["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .context("actual native request turn metadata")?,
        )?;
        if metadata["thread_id"] == selected_chat.child_thread_id {
            sample = Some((index, body, metadata));
            break;
        }
    }
    let (sample_index, sampled, metadata) =
        sample.context("the selected actual native child request")?;
    assert_eq!(metadata["turn_id"], later.turn_id);
    assert!(
        sampled["input"]
            .as_array()
            .context("actual native model input")?
            .iter()
            .any(|item| item.to_string().contains("native conversation active.")),
        "provider compatibility conversion must retain the actual full task text"
    );
    let stored = rpc::<codex_app_server_protocol::ThreadReadResponse>(
        &mut app,
        "thread/read",
        json!({"threadId":selected_chat.child_thread_id,"includeTurns":false}),
    )
    .await?;
    let path = stored
        .thread
        .path
        .context("the real selected native child's persisted rollout path")?;
    let mut reader = codex_rollout::open_rollout_line_reader(&path).await?;
    let mut native = None;
    while let Some(line) = reader.next_line().await? {
        if let codex_rollout::RolloutItem::ResponseItem(envelope) =
            codex_rollout::parse_rollout_line(&line)?.item
            && matches!(
                &envelope.item,
                codex_protocol::models::ResponseItem::AgentMessage { .. }
            )
            && envelope.item.turn_id() == Some(later.turn_id.as_str())
        {
            let item = serde_json::to_value(&envelope.item)?;
            if item.to_string().contains("native conversation active.") {
                native = Some(item);
                break;
            }
        }
    }
    let native=native.context("the actual durable native task AgentMessage and owned-turn provenance, independent of provider-compatible wire shape")?;
    let native_id = native["id"]
        .as_str()
        .context("the stable native AgentMessage identity")?;
    assert!(native_id.starts_with("amsg_"));
    assert_eq!(
        native["internal_chat_message_metadata_passthrough"]["turn_id"],
        later.turn_id
    );
    let full_text=native["content"].as_array().context("full native task input content")?.iter().map(|part| {
        assert_eq!(part["type"],"input_text");
        let original=part["text"].as_str().context("the full original native task text")?;
        assert!(sampled["input"].as_array().context("actual provider-compatible native input")?.iter()
            .filter_map(|item|item["content"].as_array()).flatten()
            .any(|content|content["text"].as_str().is_some_and(|text|text.contains(original))),
            "provider compatibility must preserve the complete native task rather than only its preview");
        Ok(codex_app_server_protocol::UserInput::Text {text:part["text"].as_str().context("native plaintext task")?.to_owned(),text_elements:Vec::new()})
    }).collect::<Result<Vec<_>>>()?;
    let projected = selected_chat
        .items
        .iter()
        .find(|entry| entry.item.id() == native_id)
        .context("the actual native task projected into its owned conversation")?;
    assert_eq!(
        (&projected.turn_id, &projected.item),
        (
            &later.turn_id,
            &codex_app_server_protocol::ThreadItem::UserMessage {
                id: native_id.to_owned(),
                client_id: None,
                content: full_text,
            }
        )
    );
    let expected_message_id = format!(
        "msg_{}",
        if sample_index == 0 {
            "first-native"
        } else {
            "second-native"
        }
    );
    let expected_tool_id = format!(
        "call_{}",
        if sample_index == 0 {
            "first-native"
        } else {
            "second-native"
        }
    );
    assert!(selected_chat.items.iter().any(|entry|matches!(&entry.item,codex_app_server_protocol::ThreadItem::CommandExecution {id,status,aggregated_output:Some(output),..} if id==&expected_tool_id && *status==codex_app_server_protocol::CommandExecutionStatus::InProgress && output.contains("PRESELECT_TOOL_OUTPUT"))),"the same-ID durable command start must be enriched with its actual live output");
    assert!(
        selected_chat
            .items
            .iter()
            .find(|entry| entry.item.id() == expected_tool_id)
            .unwrap()
            .started_at_ms
            .is_some(),
        "known native start timestamps survive the live snapshot"
    );
    assert_eq!(
        selected_chat
            .items
            .iter()
            .filter(|entry| entry.item.id() == expected_message_id)
            .count(),
        1
    );
    let small:WorkflowTranscriptListResponse=rpc(&mut app,"workflow/transcript/list",json!({"threadId":owner,"workflowId":started.workflow.id,"target":{"type":"attempt","stepId":selected_step.id,"attemptId":later.attempt_id},"limit":1})).await?;
    assert!(
        small.data[0].items.len() > 1,
        "the initial live overlay supplements, rather than truncates, the requested durable page"
    );
    assert!(
        small.data[0]
            .items
            .iter()
            .any(|entry| entry.item.id() == expected_message_id)
    );
    let older:WorkflowTranscriptListResponse=rpc(&mut app,"workflow/transcript/list",json!({"threadId":owner,"workflowId":started.workflow.id,"target":{"type":"attempt","stepId":selected_step.id,"attemptId":later.attempt_id},"limit":100,"cursor":small.next_cursor.context("the durable cursor still reaches the native task before commentary")?})).await?;
    assert!(
        older.data[0]
            .items
            .iter()
            .any(|entry| entry.item.id() == native_id),
        "overlay insertion must not skip a durable task on the next page"
    );
    assert!(
        !older.data[0]
            .items
            .iter()
            .any(|entry| entry.item.id() == expected_message_id),
        "cursor pages retain their immutable durable-only contract"
    );
    // Keep completion injection ahead of follow-up sampling. Process exit alone
    // is too early: the exit watcher clears its wake only after queuing input.
    for (expected_pending_count, stage) in [
        (
            1,
            "command wakes armed before releasing the native commands",
        ),
        (
            0,
            "command completion input queued before releasing model responses",
        ),
    ] {
        tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
            let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
            loop {
                let mut all_reached = true;
                for child in registered_tools.keys() {
                    let snapshot: codex_app_server_protocol::ThreadReadResponse = rpc(
                        &mut app,
                        "thread/read",
                        json!({"threadId":child,"includeTurns":false}),
                    )
                    .await
                    .with_context(|| {
                        format!("read exact worker {child} while waiting for {stage}")
                    })?;
                    assert_eq!(snapshot.thread.id, *child);
                    let wake = snapshot.thread.wake_state.context(
                        "the exact native worker must remain loaded through command settlement",
                    )?;
                    if wake.pending_count != expected_pending_count {
                        all_reached = false;
                    }
                }
                if all_reached {
                    break Ok::<_, anyhow::Error>(());
                }
                ticks.tick().await;
            }
        })
        .await
        .with_context(|| format!("waiting for both exact native workers: {stage}"))??;
        if expected_pending_count == 1 {
            std::fs::write(&tool_gate, "release the two actual native commands")?;
        }
    }
    release_first
        .send(())
        .expect("finish the first actual native conversation");
    release_second
        .send(())
        .expect("finish the second actual native conversation");
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30),async {
        let mut item_completed=false;let mut delta=String::new();
        let mut child_completed=false;let mut workflow_completed=false;let mut final_proof=false;let mut accepted_item_id=None;
        while !child_completed || !workflow_completed || !final_proof {
            let message=app.read_next_message().await?;
            let codex_app_server_protocol::JSONRPCMessage::Notification(notice)=message else {anyhow::bail!("unexpected message while observing real native chat: {message:?}");};
            let params=notice.params.as_ref().context("a native lifecycle notification payload")?;
            match notice.method.as_str() {
                "item/started"|"item/completed" if params["threadId"]!=owner=>{
                    assert_eq!(params["threadId"],selected_chat.child_thread_id,"opening one workflow transcript must not subscribe to its unselected sibling");
                    assert_eq!(params["turnId"],later.turn_id);
                    if params["item"]["type"]=="agentMessage" {
                        let id=params["item"]["id"].as_str().context("the real native message identity")?;
                        assert!(id==expected_message_id || ["msg_followup_one","msg_followup_two"].contains(&id));
                        if notice.method=="item/completed" {item_completed=true;assert_eq!(params["item"]["text"],r#"{"done":true}"#);if id!=expected_message_id {accepted_item_id=Some(id.to_owned());}}
                    }
                }
                "item/agentMessage/delta"=>{
                    let update:codex_app_server_protocol::AgentMessageDeltaNotification=serde_json::from_value(params.clone())?;
                    assert_eq!((&update.thread_id,&update.turn_id,&update.item_id),(&selected_chat.child_thread_id,&later.turn_id,&expected_message_id));
                    delta.push_str(&update.delta);
                }
                "turn/completed" if params["threadId"]==selected_chat.child_thread_id && params["turn"]["id"]==later.turn_id=>{
                    assert_eq!(params["turn"]["status"],"completed");assert!(params["turn"]["error"].is_null());child_completed=true;
                }
                "workflow/lifecycle/changed" if params["event"]["runId"]==started.workflow.id && params["event"]["kind"]["type"]=="completed"=>workflow_completed=true,
                "workflow/lifecycle/changed" if params["event"]["runId"]==started.workflow.id && params["event"]["kind"]["type"]=="stepCompleted" && params["event"]["kind"]["childThreadId"]==selected_chat.child_thread_id=>{
                    assert!(!final_proof,"the proof enriches the original workflow fact instead of creating another completion");
                    let proof:codex_app_server_protocol::WorkflowAgentMessageProvenance=serde_json::from_value(params["finalMessage"].clone())?;
                    assert_eq!(proof,codex_app_server_protocol::WorkflowAgentMessageProvenance {
                        run_id:started.workflow.id.clone(),execution_id:started.workflow.execution_id.clone(),attempt_id:later.attempt_id.clone(),
                        child_thread_id:selected_chat.child_thread_id.clone(),turn_id:later.turn_id.clone(),item_id:accepted_item_id.clone().context("the exact accepted final arrived through the native completion event")?,
                        format:codex_app_server_protocol::WorkflowAgentMessageFormat::Json,
                    });
                    final_proof=true;
                }
                _=>{},
            }
        }
        assert!(item_completed,"the already-hydrated native item must settle through its original identity");
        assert_eq!(delta,"true}");
        Ok::<_,anyhow::Error>(())
    }).await.context("waiting for selected worker completion, final proof, and whole workflow settlement")??;
    let run = rpc::<codex_app_server_protocol::WorkflowReadResponse>(
        &mut app,
        "workflow/read",
        json!({"threadId":owner,"workflowId":started.workflow.id}),
    )
    .await?
    .workflow;
    assert_eq!(run.status, WorkflowStatus::Succeeded);
    assert_eq!(run.accepted_starts, 2);
    let settled:WorkflowTranscriptListResponse=rpc(&mut app,"workflow/transcript/list",json!({"threadId":owner,"workflowId":run.id,"target":{"type":"attempt","stepId":selected_step.id,"attemptId":later.attempt_id},"limit":100})).await?;
    assert_eq!(
        settled.data[0]
            .items
            .iter()
            .filter(|entry| entry.item.id() == expected_message_id)
            .count(),
        1,
        "the durable final replaces the same live item without duplication"
    );
    assert!(
        settled.data[0]
            .items
            .iter()
            .all(|entry| entry.turn_id == later.turn_id)
    );
    let child = run
        .steps
        .values()
        .find(|step| step.logical_id == later.logical_id)
        .and_then(|step| step.attempts.last())
        .and_then(|attempt| attempt.child_thread_id.clone())
        .context("the exact selected native child")?;
    let human:TurnStartResponse=rpc(&mut app,"turn/start",json!({"threadId":child,"input":[{"type":"text","text":"A later unrelated human task.","textElements":[]}]})).await?;
    tokio::time::timeout(
        Duration::from_secs(/*secs*/ 30),
        provider.wait_for_request_count(/*count*/ 5),
    )
    .await?;
    let independent: WorkflowControlListResponse = rpc(
        &mut app,
        "workflow/control/list",
        json!({"threadId":owner,"workflowId":run.id,"limit":1,"activeTarget":exact_target}),
    )
    .await?;
    assert!(
        independent.active_targets.is_empty(),
        "later independent human inference cannot become the old workflow target"
    );
    assert_eq!(independent.data, first.data);
    release_human
        .send(())
        .expect("the exact-target read leaves later human inference intact");
    human_completed(&mut app, &child, &human.turn.id).await?;
    assert_eq!(provider.requests().await.len(), 5);
    assert!(
        responses::received_responses_requests(&bootstrap)
            .await
            .is_empty()
    );
    app.shutdown_gracefully().await?;
    provider.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn pre_outbox_removed_worker_keeps_exact_native_presentation_from_its_pinned_execution()
-> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let home = TempDir::new()?;
    let server = responses::start_mock_server().await;
    let provider = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_assistant_message("old-final", r#"{"old":true}"#),
                responses::ev_completed_with_tokens("old-final", /*total_tokens*/ 2),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("later-human", r#"{"old":true}"#),
                responses::ev_completed_with_tokens("later-human", /*total_tokens*/ 11),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("replacement-final", r#"{"new":true}"#),
                responses::ev_completed_with_tokens("replacement-final", /*total_tokens*/ 3),
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
    let metadata = "workflow.meta({name:'old-presentation',description:'Keep an original worker readable after its task is replaced',phases:[]});";
    let started:WorkflowStartResponse=rpc(&mut app,"workflow/start",json!({"threadId":owner,"input":{},"source":format!("{metadata} return await workflow.agent('Original work',{{key:'old',label:'old'}});")})).await?;
    let original = terminal(&mut app, &started.workflow.id).await?;
    assert_eq!(original.status, WorkflowStatus::Succeeded);
    completed(&mut app, &original.id, &original.execution_id).await?;
    let original_chat = old_chat(&mut app, &owner, &original.id, &original.execution_id).await?;
    let old = original_chat
        .data
        .first()
        .context("the actual accepted original worker")?;
    let original_native: codex_app_server_protocol::ThreadReadResponse = rpc(
        &mut app,
        "thread/read",
        json!({"threadId":old.child_thread_id,"includeTurns":false}),
    )
    .await?;
    let native_path = original_native
        .thread
        .path
        .context("original native rollout")?;
    let human:TurnStartResponse=rpc(&mut app,"turn/start",json!({"threadId":old.child_thread_id,"input":[{"type":"text","text":"A later independent human task.","textElements":[]}]})).await?;
    human_completed(&mut app, &old.child_thread_id, &human.turn.id).await?;
    app.shutdown_gracefully().await?;
    let native_bytes = std::fs::read(&native_path)?;

    // Compatibility seed, not a fabricated native attempt: only remove the new
    // lifecycle outbox from an actually completed, already-presented run. Keep
    // every real worker, ownership record, source, result and immutable index.
    let manifest_path = home
        .path()
        .join("workflow-runs")
        .join(&owner)
        .join(format!("{}.json", original.id));
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    manifest["header"]["lifecycleVersion"] = json!(0);
    manifest["header"]["lifecycle"]["enabled"] = json!(false);
    manifest["header"]["lifecycle"]["delivered"] = json!([]);
    manifest["header"]["notificationDelivered"] = json!(true);
    manifest["lifecycleEvents"] = json!({});
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;

    let mut app = client(&home).await?;
    let _: codex_app_server_protocol::ThreadResumeResponse = rpc(
        &mut app,
        "thread/resume",
        json!({"threadId":owner,"excludeTurns":true}),
    )
    .await?;
    let state: WorkflowRevisionListResponse = rpc(
        &mut app,
        "workflow/revision/list",
        json!({"threadId":owner,"workflowId":original.id}),
    )
    .await?;
    let staged:WorkflowRevisionStageResponse=rpc(&mut app,"workflow/revision/stage",json!({
        "threadId":owner,"workflowId":original.id,"commandId":"replace-old-stage","expectedControlRevision":state.state.control_revision,
        "source":format!("{metadata} return await workflow.agent('Replacement work',{{key:'new',label:'new'}});"),"input":{"type":"absent"},
    })).await?;
    assert_eq!(staged.revision.reusable_statements, 0);
    let _:WorkflowRevisionApplyResponse=rpc(&mut app,"workflow/revision/apply",json!({"threadId":owner,"workflowId":original.id,"commandId":"replace-old-apply","expectedControlRevision":staged.state.control_revision,"revisionId":staged.revision.id})).await?;
    let ready = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        let mut ticks = tokio::time::interval(Duration::from_millis(/*millis*/ 10));
        loop {
            let state: WorkflowRevisionListResponse = rpc(
                &mut app,
                "workflow/revision/list",
                json!({"threadId":owner,"workflowId":original.id}),
            )
            .await?;
            if state.state.status == WorkflowRevisionStatus::Ready {
                break Ok::<_, anyhow::Error>(state);
            }
            ticks.tick().await;
        }
    })
    .await??;
    let resumed:WorkflowRevisionResumeResponse=rpc(&mut app,"workflow/revision/resume",json!({"threadId":owner,"workflowId":original.id,"commandId":"replace-old-resume","expectedControlRevision":ready.state.control_revision,"revisionId":staged.revision.id})).await?;
    let execution = resumed
        .revision
        .execution_ids
        .last()
        .context("the actual replacement execution")?;
    assert_ne!(execution, &original.execution_id);
    let revised = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        loop {
            let notice: codex_app_server_protocol::WorkflowUpdatedNotification =
                app.read_notification("workflow/updated").await?;
            if notice.workflow.execution_id == *execution
                && notice.workflow.status == WorkflowStatus::Succeeded
            {
                break Ok::<_, anyhow::Error>(notice.workflow);
            }
        }
    })
    .await??;
    assert!(
        revised
            .steps
            .values()
            .flat_map(|step| &step.attempts)
            .all(|attempt| attempt.child_thread_id.as_ref() != Some(&old.child_thread_id)),
        "the proof must not come from a retained current-plan step"
    );
    app.shutdown_gracefully().await?;

    let mut cold = client(&home).await?;
    let items: codex_app_server_protocol::ThreadItemsListResponse = rpc(
        &mut cold,
        "thread/items/list",
        json!({"threadId":old.child_thread_id,"limit":100}),
    )
    .await?;
    let final_item = items
        .data
        .iter()
        .find(|entry| Some(entry.item.id()) == old.final_item_id.as_deref())
        .context("the immutable original final message")?;
    assert_eq!(
        Some(final_item),
        old.items
            .iter()
            .find(|entry| entry.item.id() == final_item.item.id()),
        "direct cold native reads must match the scoped archived execution's exact proof and raw text"
    );
    assert!(items.data.iter().any(|entry|entry.turn_id==human.turn.id && matches!(&entry.item,codex_app_server_protocol::ThreadItem::AgentMessage{text,workflow:None,..} if text==r#"{"old":true}"#)),"identical JSON in a later human turn is never proof of a workflow result");
    let persisted = codex_core::workflows::read_persisted(
        &codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
            home.path().join("workflow-runs").join(&owner),
        )?,
        &original.id,
    )
    .map_err(anyhow::Error::msg)?;
    assert!(
        persisted.lifecycle.events.is_empty(),
        "compatibility inspection must not synthesize lifecycle outbox facts"
    );
    assert_eq!(
        old_chat(&mut cold, &owner, &original.id, &original.execution_id).await?,
        original_chat
    );
    let loaded: ThreadLoadedListResponse = rpc(&mut cold, "thread/loaded/list", json!({})).await?;
    assert!(loaded.data.is_empty());
    assert_eq!(
        std::fs::read(native_path)?,
        native_bytes,
        "presentation and revision never rewrite the original native conversation"
    );
    assert_eq!(
        provider.requests().len(),
        3,
        "cold reads do not activate services or infer"
    );
    cold.shutdown_gracefully().await?;
    Ok(())
}

#[tokio::test]
async fn legacy_fork_keeps_inherited_workflow_facts_and_ordinary_chat_readable_after_cold_restart()
-> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let home = TempDir::new()?;
    let provider = responses::start_mock_server().await;
    let model = responses::mount_sse_sequence(
        &provider,
        vec![
            responses::sse(vec![
                responses::ev_assistant_message(
                    "msg_before_fork_workflow",
                    "LEGACY_BEFORE_WORKFLOW",
                ),
                responses::ev_completed("before-workflow"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("msg_after_fork_workflow", "LEGACY_AFTER_WORKFLOW"),
                responses::ev_completed("after-workflow"),
            ]),
        ],
    )
    .await;
    configure_agent_workflows(&home, &provider).await?;
    let mut app = client(&home).await?;
    let started = app
        .start_thread(ThreadStartParams {
            history_mode: Some(codex_app_server_protocol::ThreadHistoryMode::Legacy),
            ..Default::default()
        })
        .await?;
    let owner = started.thread.id;
    assert_eq!(
        started.thread.history_mode,
        codex_app_server_protocol::ThreadHistoryMode::Legacy
    );
    let before:TurnStartResponse=rpc(&mut app,"turn/start",json!({"threadId":owner,"input":[{"type":"text","text":"Ordinary chat before a workflow on this legacy conversation.","textElements":[]}]})).await?;
    human_completed(&mut app, &owner, &before.turn.id).await?;
    let started:WorkflowStartResponse=rpc(&mut app,"workflow/start",json!({"threadId":owner,"source":
        "workflow.meta({name:'legacy-fork-report',description:'Keep an inherited human-visible report with its actual owner',phases:[]}); return {report:'LEGACY_FORK_REPORT'};","input":{}})).await?;
    let run = terminal(&mut app, &started.workflow.id).await?;
    completed(&mut app, &run.id, &run.execution_id).await?;
    assert_eq!(run.status, WorkflowStatus::Succeeded);
    let after:TurnStartResponse=rpc(&mut app,"turn/start",json!({"threadId":owner,"input":[{"type":"text","text":"Ordinary chat after the retained workflow report.","textElements":[]}]})).await?;
    human_completed(&mut app, &owner, &after.turn.id).await?;
    let parent: codex_app_server_protocol::ThreadTimelineListResponse = rpc(
        &mut app,
        "thread/timeline/list",
        json!({"threadId":owner,"limit":100}),
    )
    .await?;
    assert_eq!(parent.next_cursor, None);
    let facts = parent
        .data
        .iter()
        .filter_map(|entry| match entry {
            codex_app_server_protocol::ThreadTimelineEntry::Workflow { event, .. } => {
                Some(event.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(facts.len(), 2);
    assert!(
        facts
            .iter()
            .all(|event| event.owner_thread_id == owner && event.run_id == run.id)
    );
    let fork: codex_app_server_protocol::ThreadForkResponse =
        rpc(&mut app, "thread/fork", json!({"threadId":owner})).await?;
    let fork_id = fork.thread.id;
    assert_ne!(fork_id, owner);
    assert_eq!(fork.thread.forked_from_id.as_deref(), Some(owner.as_str()));
    assert_eq!(
        fork.thread.history_mode,
        codex_app_server_protocol::ThreadHistoryMode::Legacy
    );
    let inherited: codex_app_server_protocol::ThreadTimelineListResponse = rpc(
        &mut app,
        "thread/timeline/list",
        json!({"threadId":fork_id,"limit":100}),
    )
    .await?;
    assert_eq!(inherited.next_cursor, None);
    let inherited_facts = inherited
        .data
        .iter()
        .filter_map(|entry| match entry {
            codex_app_server_protocol::ThreadTimelineEntry::Workflow { event, .. } => {
                Some(event.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        inherited_facts, facts,
        "proven fork provenance must preserve each immutable fact's original owner and identity"
    );
    let text = serde_json::to_string(&inherited.data)?;
    let before = text
        .find("LEGACY_BEFORE_WORKFLOW")
        .context("the inherited ordinary answer before the workflow")?;
    let report = text
        .find("LEGACY_FORK_REPORT")
        .context("the inherited retained workflow result")?;
    let after = text
        .find("LEGACY_AFTER_WORKFLOW")
        .context("the inherited ordinary answer after the workflow")?;
    assert!(before < report && report < after);
    assert!(
        inherited_facts
            .iter()
            .all(|event| event.owner_thread_id != fork_id),
        "inherited facts cannot become new owned workflow executions"
    );
    let own: codex_app_server_protocol::WorkflowListResponse = rpc(
        &mut app,
        "workflow/list",
        json!({"threadId":fork_id,"scope":"current"}),
    )
    .await?;
    assert!(own.data.is_empty());
    denied(
        &mut app,
        "workflow/stop",
        json!({"threadId":fork_id,"workflowId":run.id}),
    )
    .await?;
    assert_eq!(
        model.requests().len(),
        2,
        "forking and reading inherited facts must not infer or rerun their source"
    );
    app.shutdown_gracefully().await?;
    let mut cold = client(&home).await?;
    let loaded: ThreadLoadedListResponse = rpc(&mut cold, "thread/loaded/list", json!({})).await?;
    assert!(loaded.data.is_empty());
    let reread: codex_app_server_protocol::ThreadTimelineListResponse = rpc(
        &mut cold,
        "thread/timeline/list",
        json!({"threadId":fork_id,"limit":100}),
    )
    .await?;
    assert_eq!(
        reread, inherited,
        "cold canonical reads must retain ordinary chat and inherited workflow chronology"
    );
    let loaded: ThreadLoadedListResponse = rpc(&mut cold, "thread/loaded/list", json!({})).await?;
    assert!(
        loaded.data.is_empty(),
        "a cold inherited timeline read cannot resume the fork or original workflow"
    );
    assert_eq!(model.requests().len(), 2);
    cold.shutdown_gracefully().await?;
    Ok(())
}
