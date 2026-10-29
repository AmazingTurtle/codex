//! Conversation pause keeps real producers alive while gating their next model request.

use anyhow::Result;
use codex_core::CodexThread;
use codex_core::ConversationExecutionState;
use codex_core::TurnInputRequest;
use codex_features::Feature;
use codex_protocol::AgentPath;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecutionControlState;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_sandbox;
use core_test_support::skip_if_target_windows;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use test_case::test_case;

fn input(text: &str) -> TurnInputRequest {
    TurnInputRequest::user_input(vec![UserInput::Text {
        text: text.to_owned(),
        text_elements: Vec::new(),
    }])
}

fn answer(id: &str) -> String {
    let mut message = responses::ev_assistant_message(&format!("message-{id}"), "Finished.");
    message["item"]["phase"] = json!("final_answer");
    responses::sse(vec![
        responses::ev_response_created(id),
        message,
        responses::ev_completed(id),
    ])
}

async fn complete(thread: &CodexThread) {
    let EventMsg::TurnComplete(completed) =
        wait_for_event(thread, |event| matches!(event, EventMsg::TurnComplete(_))).await
    else {
        unreachable!();
    };
    assert_eq!(completed.error, None);
}

async fn queue_before_pause(thread: &CodexThread, text: &str) -> Result<()> {
    assert!(
        !thread.enabled(Feature::WakeOnAgentMessages),
        "the fixture must retain queue-only work without sampling"
    );
    thread
        .submit(Op::InterAgentCommunication {
            communication: InterAgentCommunication::new(
                AgentPath::root().join("worker").expect("valid worker path"),
                AgentPath::root(),
                Vec::new(),
                text.to_owned(),
                /*trigger_turn*/ false,
            ),
            start_options: Default::default(),
        })
        .await?;
    thread
        .update_thread_settings(ThreadSettingsOverrides::default())
        .await?;
    assert!(
        thread.can_pause_execution().await?,
        "real queued work must make the conversation pauseable"
    );
    Ok(())
}

async fn assert_paused(test: &TestCodex) -> Result<()> {
    assert_eq!(
        test.codex.execution_state().await?,
        ConversationExecutionState {
            root_thread_id: test.session_configured.thread_id,
            state: ExecutionControlState::Paused,
        }
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(150), test.codex.wait_until_running())
            .await
            .is_err(),
        "the admission gate must stay closed until an explicit or human resume"
    );
    assert!(
        !test.codex.can_pause_execution().await?,
        "paused work must not advertise another pause transition"
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum Resume {
    Explicit,
    Human,
    Restart,
}

#[test_case(Resume::Explicit; "explicit_unpause")]
#[test_case(Resume::Human; "next_human_input")]
#[test_case(Resume::Restart; "restart_while_paused")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_background_process_waits_for_resume_and_is_delivered_once(
    resume: Resume,
) -> Result<()> {
    skip_if_target_windows!(Ok(()), "uses a POSIX process fixture");
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));
    let server = responses::start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;
    let args = json!({
        "cmd": "while [ ! -f pause-release ]; do sleep 0.05; done; printf PAUSED_PROCESS_RESULT; exit 7",
        "yield_time_ms": 250,
    });
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("launch"),
                responses::ev_function_call("background", "exec_command", &args.to_string()),
                responses::ev_completed("launch"),
            ]),
            answer("waiting"),
            answer("delivered"),
            answer("followup"),
        ],
    )
    .await;
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, test.config.cwd.as_path());
    test.codex
        .start_or_steer_turn(input("Run the background process.").with_thread_settings(
            ThreadSettingsOverrides {
                approval_policy: Some(AskForApproval::Never),
                sandbox_policy: Some(sandbox_policy),
                permission_profile,
                ..Default::default()
            },
        ))
        .await?;
    complete(&test.codex).await;
    assert_eq!(mock.requests().len(), 2);
    assert!(
        mock.function_call_output_text("background")
            .expect("real background process launch")
            .contains("Process running with session ID")
    );
    assert!(
        test.codex.can_pause_execution().await?,
        "a live background process must remain pauseable after the foreground turn completes"
    );
    test.codex.pause_execution().await?;
    assert_paused(&test).await?;
    test.fs()
        .write_file(
            &test.workspace_path_uri("pause-release")?,
            Vec::new(),
            Default::default(),
            /*sandbox*/ None,
        )
        .await?;
    let event = wait_for_event(
        &test.codex,
        |event| matches!(event, EventMsg::ExecCommandEnd(end) if end.call_id == "background"),
    )
    .await;
    let EventMsg::ExecCommandEnd(end) = event else {
        unreachable!();
    };
    assert_eq!(end.exit_code, 7);
    assert!(end.aggregated_output.contains("PAUSED_PROCESS_RESULT"));
    // Wait for the real completion producer to commit its queued signal before restarting.
    let journal = test
        .home
        .path()
        .join("execution-control")
        .join(format!("{}.json", test.session_configured.thread_id));
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut interval = tokio::time::interval(Duration::from_millis(10));
        loop {
            interval.tick().await;
            let stored: Value = serde_json::from_slice(&tokio::fs::read(&journal).await?)?;
            if stored["signals"]
                .as_array()
                .is_some_and(|signals| !signals.is_empty())
                && stored.to_string().contains("PAUSED_PROCESS_RESULT")
            {
                return Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await??;
    assert_paused(&test).await?;
    assert_eq!(
        mock.requests().len(),
        2,
        "completion must not wake inference"
    );
    let test = if matches!(resume, Resume::Restart) {
        builder.restart_with_auto_env(&server, &test).await?
    } else {
        test
    };
    assert_paused(&test).await?;
    match resume {
        Resume::Explicit => {
            test.codex.submit(Op::Unpause).await?;
        }
        Resume::Human => {
            test.codex
                .start_or_steer_turn(input("Use the completed result."))
                .await?;
        }
        Resume::Restart => {
            test.codex.unpause_execution().await?;
        }
    }
    complete(&test.codex).await;
    assert_eq!(
        test.codex.execution_state().await?.state,
        ExecutionControlState::Running
    );
    let delivered = mock.requests();
    assert_eq!(delivered.len(), 3);
    assert_eq!(
        serde_json::to_string(&delivered[2].input())?
            .matches("Run the background process.")
            .count(),
        1,
        "the first pause must preserve history acknowledged before journal activation"
    );
    assert!(delivered[2].body_contains_text("PAUSED_PROCESS_RESULT"));
    assert!(delivered[2].body_contains_text("exit-code=\"7\""));
    assert_eq!(
        serde_json::to_string(&delivered[2].input())?
            .matches("<exec-command-completed")
            .count(),
        1
    );
    // A repeated resume and a later turn must not append another copy of the signal.
    test.codex.unpause_execution().await?;
    test.submit_text_turn("Check the result again.").await?;
    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        serde_json::to_string(&requests[3].input())?
            .matches("<exec-command-completed")
            .count(),
        1
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[derive(Clone, Copy)]
enum PendingGuidance {
    AgentAndMail,
    MailOnly,
}

#[test_case(Resume::Human, PendingGuidance::AgentAndMail; "live_session")]
#[test_case(Resume::Restart, PendingGuidance::AgentAndMail; "restart_preserves_accepted_guidance")]
#[test_case(Resume::Human, PendingGuidance::MailOnly; "mail_only_reaches_next_human_request")]
#[test_case(Resume::Restart, PendingGuidance::MailOnly; "mail_only_survives_restart")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_input_and_mail_stay_paused_until_human_input(
    resume: Resume,
    guidance: PendingGuidance,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        for feature in [Feature::Collab, Feature::MultiAgentV2] {
            config
                .features
                .enable(feature)
                .expect("enable agent messages");
        }
        config
            .features
            .disable(Feature::WakeOnAgentMessages)
            .expect("retain queue-only setup work");
    });
    let test = builder.build_with_auto_env(&server).await?;
    let mock = responses::mount_sse_once(&server, answer("resumed")).await;
    queue_before_pause(
        &test.codex,
        "Worker report awaiting the next authorized turn.",
    )
    .await?;
    test.codex.submit(Op::Pause).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ExecutionControlChanged(state) if state.state == ExecutionControlState::Paused)
    }).await;
    if matches!(guidance, PendingGuidance::AgentAndMail) {
        test.codex
            .start_or_steer_turn(input("Accepted agent guidance.").from_agent())
            .await?;
    }
    test.codex
        .submit(Op::InterAgentCommunication {
            communication: InterAgentCommunication::new(
                AgentPath::root().join("worker").expect("valid worker path"),
                AgentPath::root(),
                Vec::new(),
                "Queued worker completion.".to_owned(),
                /*trigger_turn*/ true,
            ),
            start_options: Default::default(),
        })
        .await?;
    test.codex
        .update_thread_settings(ThreadSettingsOverrides::default())
        .await?;
    assert_paused(&test).await?;
    assert_eq!(mock.requests().len(), 0);
    let test = if matches!(resume, Resume::Restart) {
        builder.restart_with_auto_env(&server, &test).await?
    } else {
        test
    };
    assert_paused(&test).await?;
    test.codex
        .start_or_steer_turn(input("Human resumes the work."))
        .await?;
    complete(&test.codex).await;
    let request = mock.single_request();
    let mut expected = vec![
        "Worker report awaiting the next authorized turn.",
        "Queued worker completion.",
        "Human resumes the work.",
    ];
    if matches!(guidance, PendingGuidance::AgentAndMail) {
        expected.push("Accepted agent guidance.");
    }
    for text in expected {
        assert!(
            request.body_contains_text(text),
            "missing accepted input: {text}"
        );
        assert_eq!(
            serde_json::to_string(&request.input())?
                .matches(text)
                .count(),
            1,
            "accepted input must be delivered exactly once: {text}"
        );
    }
    assert_eq!(
        test.codex.execution_state().await?.state,
        ExecutionControlState::Running
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_resume_after_restart_preserves_pending_user_order_before_mail() -> Result<()> {
    use codex_history::RolloutItem;
    use codex_protocol::models::AgentMessageInputContent;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;

    skip_if_no_network!(Ok(()));
    const GUIDANCE: &str = "Accepted guidance before restart without another human input.";
    const MAIL: &str = "Worker mail restored before the pending guidance.";
    let server = responses::start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        for feature in [Feature::Collab, Feature::MultiAgentV2] {
            config
                .features
                .enable(feature)
                .expect("enable agent messages");
        }
        config
            .features
            .disable(Feature::WakeOnAgentMessages)
            .expect("use trigger-turn mail");
    });
    let test = builder.build_with_auto_env(&server).await?;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            answer("explicit-restored"),
            answer("second-restart-followup"),
        ],
    )
    .await;
    queue_before_pause(&test.codex, "Worker report awaiting explicit resume.").await?;
    test.codex.pause_execution().await?;
    test.codex
        .start_or_steer_turn(input(GUIDANCE).from_agent())
        .await?;
    test.codex
        .submit(Op::InterAgentCommunication {
            communication: InterAgentCommunication::new(
                AgentPath::root().join("worker").expect("valid worker path"),
                AgentPath::root(),
                Vec::new(),
                MAIL.to_owned(),
                /*trigger_turn*/ true,
            ),
            start_options: Default::default(),
        })
        .await?;
    test.codex
        .update_thread_settings(ThreadSettingsOverrides::default())
        .await?;
    assert_paused(&test).await?;
    let journal = test
        .home
        .path()
        .join("execution-control")
        .join(format!("{}.json", test.session_configured.thread_id));
    let stored: Value = serde_json::from_slice(&tokio::fs::read(&journal).await?)?;
    let pending_order = stored["signals"]
        .as_array()
        .expect("queued signals")
        .iter()
        .flat_map(|signal| signal["input"].as_array().expect("signal input"))
        .find_map(|input| {
            input
                .pointer("/User/metadata/acceptance_order")
                .and_then(Value::as_u64)
        })
        .expect("the accepted guidance reserves an order before restart");
    assert_eq!(mock.requests().len(), 0);
    let test = builder.restart_with_auto_env(&server, &test).await?;
    assert_paused(&test).await?;
    // No new user-input reservation is allowed to repair the restored allocator.
    test.codex.unpause_execution().await?;
    complete(&test.codex).await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    for text in [GUIDANCE, MAIL] {
        assert_eq!(
            serde_json::to_string(&requests[0].input())?
                .matches(text)
                .count(),
            1
        );
    }
    test.codex.flush_rollout().await?;
    let mut user_orders = Vec::new();
    let mut mail_orders = Vec::new();
    for item in test
        .codex
        .load_history(/*include_archived*/ false)
        .await?
        .items
    {
        if let RolloutItem::ResponseItem(envelope) = item {
            let order = envelope
                .metadata
                .and_then(|metadata| metadata.user_input_order);
            match envelope.item {
                ResponseItem::Message { role, content, .. } if role == "user"
                    && content.iter().any(|part| matches!(part, ContentItem::InputText { text } if text == GUIDANCE)) => {
                    user_orders.push(order.expect("accepted guidance keeps its order"));
                }
                ResponseItem::AgentMessage { content, .. } if content.iter()
                    .any(|part| matches!(part, AgentMessageInputContent::InputText { text } if text == MAIL)) => {
                    mail_orders.push(order.expect("restored mail has a global order"));
                }
                _ => {}
            }
        }
    }
    assert_eq!(
        user_orders,
        vec![pending_order],
        "the original accepted input must be recorded exactly once"
    );
    assert_eq!(mail_orders.len(), 1);
    assert!(
        mail_orders[0] > pending_order,
        "restored mail must not reuse the pending user receipt identity"
    );
    let stored: Value = serde_json::from_slice(&tokio::fs::read(&journal).await?)?;
    assert_eq!(stored["signals"], json!([]));
    let test = builder.restart_with_auto_env(&server, &test).await?;
    test.codex.unpause_execution().await?;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(150),
            wait_for_event(&test.codex, |event| matches!(
                event,
                EventMsg::TurnComplete(_)
            ))
        )
        .await
        .is_err(),
        "a second resume must not redeliver acknowledged input"
    );
    assert_eq!(mock.requests().len(), 1);
    test.submit_text_turn("Read the previously restored work.")
        .await?;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    for text in [GUIDANCE, MAIL] {
        assert_eq!(
            serde_json::to_string(&requests[1].input())?
                .matches(text)
                .count(),
            1,
            "second restart must retain the original input without duplication"
        );
    }
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpause_delivers_more_than_two_mail_batches_without_another_human_wake() -> Result<()> {
    use codex_history::RolloutItem;
    use codex_protocol::models::AgentMessageInputContent;
    use codex_protocol::models::ResponseItem;
    use std::collections::BTreeSet;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = test_codex()
        .with_config(|config| {
            for feature in [Feature::Collab, Feature::MultiAgentV2] {
                config
                    .features
                    .enable(feature)
                    .expect("enable agent messages");
            }
            config
                .features
                .disable(Feature::WakeOnAgentMessages)
                .expect("use explicit trigger-turn mail");
        })
        .build_with_auto_env(&server)
        .await?;
    const FINAL_REPORT: &str = "PAUSED_BATCH_REPORT_128";
    const FINAL_ANSWER: &str = "All paused mail batches were sampled.";
    let sequence = AtomicUsize::new(0);
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/responses"))
        .respond_with(move |request: &wiremock::Request| {
            let index = sequence.fetch_add(1, Ordering::SeqCst);
            let body = if request
                .headers
                .get("content-encoding")
                .is_some_and(|encoding| encoding == "zstd")
            {
                zstd::stream::decode_all(std::io::Cursor::new(&request.body))
                    .expect("decode model request")
            } else {
                request.body.clone()
            };
            let body: Value = serde_json::from_slice(&body).expect("model request JSON");
            let sampled_final_report =
                body["input"]
                    .as_array()
                    .expect("model input")
                    .iter()
                    .any(|item| {
                        item["type"] == "agent_message"
                            && item["content"].as_array().is_some_and(|parts| {
                                parts.iter().any(|part| {
                                    part["type"] == "input_text" && part["text"] == FINAL_REPORT
                                })
                            })
                    });
            let response = if sampled_final_report {
                let mut message = responses::ev_assistant_message("all-batches", FINAL_ANSWER);
                message["item"]["phase"] = json!("final_answer");
                responses::sse(vec![message, responses::ev_completed("all-batches")])
            } else {
                answer(&format!("mail-batch-{index}"))
            };
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(response)
        })
        .up_to_n_times(3)
        .expect(1..=3)
        .mount(&server)
        .await;
    let expected = (0..129)
        .map(|index| format!("PAUSED_BATCH_REPORT_{index:03}"))
        .collect::<Vec<_>>();
    queue_before_pause(&test.codex, &expected[0]).await?;
    test.codex.pause_execution().await?;
    for content in expected.iter().skip(1) {
        test.codex
            .submit(Op::InterAgentCommunication {
                communication: InterAgentCommunication::new(
                    AgentPath::root().join("worker").expect("valid worker path"),
                    AgentPath::root(),
                    Vec::new(),
                    content.clone(),
                    /*trigger_turn*/ true,
                ),
                start_options: Default::default(),
            })
            .await?;
    }
    test.codex
        .update_thread_settings(ThreadSettingsOverrides::default())
        .await?;
    assert_paused(&test).await?;
    assert!(
        responses::received_responses_requests(&server)
            .await
            .is_empty()
    );
    let journal = test
        .home
        .path()
        .join("execution-control")
        .join(format!("{}.json", test.session_configured.thread_id));
    let stored: Value = serde_json::from_slice(&tokio::fs::read(&journal).await?)?;
    assert_eq!(
        stored["signals"]
            .as_array()
            .expect("queued signal array")
            .len(),
        expected.len()
    );
    test.codex.unpause_execution().await?;
    // Acknowledgement precedes sampling, and an earlier batch can finish its own turn.
    // Wait for the provider answer that requires the final report in an actual request.
    // No additional human submission is available to rescue a stranded third batch.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let EventMsg::TurnComplete(completed) = wait_for_event(&test.codex, |event| {
                matches!(event, EventMsg::TurnComplete(_))
            })
            .await
            else {
                unreachable!();
            };
            assert_eq!(completed.error, None);
            if completed.last_agent_message.as_deref() == Some(FINAL_ANSWER) {
                break;
            }
        }
        let mut interval = tokio::time::interval(Duration::from_millis(10));
        loop {
            interval.tick().await;
            let stored: Value = serde_json::from_slice(&tokio::fs::read(&journal).await?)?;
            if stored["signals"].as_array().is_some_and(Vec::is_empty) {
                return Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await??;
    test.codex.flush_rollout().await?;
    let mut persisted = Vec::new();
    let mut persisted_ids = BTreeSet::new();
    for item in test
        .codex
        .load_history(/*include_archived*/ false)
        .await?
        .items
    {
        if let RolloutItem::ResponseItem(envelope) = item
            && let ResponseItem::AgentMessage {
                id,
                author,
                recipient,
                content,
                ..
            } = envelope.item
        {
            assert_eq!(
                (author.as_str(), recipient.as_str()),
                ("/root/worker", "/root")
            );
            assert!(
                persisted_ids.insert(id.expect("durable mail delivery has an ID")),
                "a mail delivery must not be persisted more than once"
            );
            for part in content {
                if let AgentMessageInputContent::InputText { text } = part {
                    persisted.push(text);
                }
            }
        }
    }
    assert_eq!(persisted_ids.len(), expected.len());
    persisted.sort();
    assert_eq!(
        persisted, expected,
        "each report must become durable exactly once"
    );
    let requests = responses::received_responses_requests(&server).await;
    assert!((1..=3).contains(&requests.len()));
    let mut delivered = BTreeSet::new();
    for request in &requests {
        let mut request_mail = Vec::new();
        for message in request.inputs_of_type("agent_message") {
            for part in message["content"].as_array().expect("mail content") {
                request_mail.push(part["text"].as_str().expect("mail text").to_owned());
            }
        }
        let unique = request_mail.iter().cloned().collect::<BTreeSet<_>>();
        assert_eq!(
            unique.len(),
            request_mail.len(),
            "a request must not contain duplicate reports"
        );
        delivered.extend(unique);
    }
    assert_eq!(
        delivered,
        expected.into_iter().collect::<BTreeSet<_>>(),
        "all three queued batches must actually reach model inference"
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(150),
            wait_for_event(&test.codex, |event| matches!(
                event,
                EventMsg::TurnComplete(_)
            ))
        )
        .await
        .is_err(),
        "an empty journal must not generate another turn"
    );
    assert_eq!(
        responses::received_responses_requests(&server).await.len(),
        requests.len()
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpause_preserves_queue_only_mail_without_starting_an_idle_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = test_codex()
        .with_config(|config| {
            for feature in [Feature::Collab, Feature::MultiAgentV2] {
                config
                    .features
                    .enable(feature)
                    .expect("enable agent messages");
            }
            config
                .features
                .disable(Feature::WakeOnAgentMessages)
                .expect("queue-only mail never wakes");
        })
        .build_with_auto_env(&server)
        .await?;
    let mock = responses::mount_sse_once(&server, answer("read-queued-mail")).await;
    queue_before_pause(&test.codex, "Queue-only worker report.").await?;
    test.codex.pause_execution().await?;
    assert_paused(&test).await?;
    test.codex.unpause_execution().await?;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(150),
            wait_for_event(&test.codex, |event| matches!(
                event,
                EventMsg::TurnComplete(_)
            ))
        )
        .await
        .is_err(),
        "unpause must respect queue-only delivery"
    );
    assert_eq!(
        mock.requests().len(),
        0,
        "idle queue-only mail must not start inference"
    );
    test.submit_text_turn("Read the pending report.").await?;
    let request = mock.single_request();
    assert!(request.body_contains_text("Queue-only worker report."));
    assert!(request.body_contains_text("Read the pending report."));
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_idle_admission_cannot_unpause_or_consume_queued_input() -> Result<()> {
    use codex_core::NotSubmittedReason;
    use codex_core::StartIfIdleSubmission;
    use codex_core::TurnInput;
    use codex_core::TurnInputSubmission;
    use codex_core::context::ContextualUserFragment;
    use codex_core::context::InternalContextSource;
    use codex_core::context::InternalModelContextFragment;

    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = test_codex()
        .with_config(|config| {
            config
                .features
                .disable(Feature::WakeOnAgentMessages)
                .expect("retain queue-only setup work");
        })
        .build_with_auto_env(&server)
        .await?;
    let mock =
        responses::mount_sse_sequence(&server, vec![answer("initial"), answer("human-only")]).await;
    let TurnInputSubmission::Started { turn_id, .. } = test
        .codex
        .start_or_steer_turn(input("Prepare an idle conversation."))
        .await?
    else {
        anyhow::bail!("initial human input must start a turn");
    };
    complete(&test.codex).await;
    queue_before_pause(&test.codex, "Worker report awaiting a human turn.").await?;
    test.codex.pause_execution().await?;
    for request in [
        input("Automatic queue input with default provenance."),
        input("Automatic queue input from an agent.").from_agent(),
    ] {
        assert_eq!(
            test.codex.start_turn_if_idle(request).await?,
            StartIfIdleSubmission::NotSubmitted {
                reason: NotSubmittedReason::ConversationPaused
            },
        );
        assert_paused(&test).await?;
        assert_eq!(mock.requests().len(), 1);
    }
    let continuation = ContextualUserFragment::into(InternalModelContextFragment::new(
        InternalContextSource::from_static("daemon_recovery"),
        "Automatic queue input from an internal continuation.",
    ));
    assert_eq!(
        test.codex
            .continue_turn_if_idle(
                TurnInputRequest::new(TurnInput::ResponseItem(continuation)),
                turn_id,
            )
            .await?,
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::ConversationPaused
        }
    );
    assert_paused(&test).await?;
    assert_eq!(mock.requests().len(), 1);
    test.submit_text_turn("Human input resumes normally.")
        .await?;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let request = &requests[1];
    assert!(request.body_contains_text("Worker report awaiting a human turn."));
    assert!(request.body_contains_text("Human input resumes normally."));
    assert!(
        !request.body_contains_text("Automatic queue input"),
        "declined automatic input must remain with its caller for retry"
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_human_input_resumes_owning_tree_but_pause_does_not_block_another_root() -> Result<()>
{
    use core_test_support::streaming_sse::StreamingSseChunk;
    use core_test_support::streaming_sse::start_streaming_sse_server;

    skip_if_no_network!(Ok(()));
    let (root_release, root_gate) = tokio::sync::oneshot::channel();
    let (worker_release, worker_gate) = tokio::sync::oneshot::channel();
    let (streaming, _) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk { gate: None, body: responses::sse(vec![
                responses::ev_response_created("spawn"),
                responses::ev_function_call_with_namespace(
                    "spawn-worker", "collaboration", "spawn_agent",
                    &json!({"message": "Initial worker task.", "task_name": "worker", "fork_turns": "none"}).to_string(),
                ),
            ]) },
            StreamingSseChunk { gate: Some(root_gate), body: responses::sse(vec![responses::ev_completed("spawn")]) },
        ],
        vec![
            StreamingSseChunk { gate: None, body: responses::sse(vec![responses::ev_response_created("worker-open")]) },
            StreamingSseChunk { gate: Some(worker_gate), body: responses::sse(vec![responses::ev_completed("worker-open")]) },
        ],
        vec![StreamingSseChunk { gate: None, body: answer("independent") }],
        vec![StreamingSseChunk { gate: None, body: answer("resumed-first") }],
        vec![StreamingSseChunk { gate: None, body: answer("resumed-second") }],
    ]).await;
    let server = responses::start_mock_server().await;
    let base_url = format!("{}/v1", streaming.uri());
    let test = test_codex()
        .with_model("gpt-5.6-sol")
        .with_config(move |config| {
            config.model_provider.base_url = Some(base_url);
            config.model_provider.supports_websockets = false;
            for feature in [Feature::Collab, Feature::MultiAgentV2] {
                config
                    .features
                    .enable(feature)
                    .expect("enable normal workers");
            }
            config
                .features
                .disable(Feature::WakeOnAgentMessages)
                .expect("queue completion mail");
        })
        .build_with_auto_env(&server)
        .await?;
    test.codex
        .start_or_steer_turn(input("Spawn a normal worker."))
        .await?;
    tokio::time::timeout(
        Duration::from_secs(10),
        streaming.wait_for_request_count(/*count*/ 2),
    )
    .await?;
    let root = test.session_configured.thread_id;
    let worker_id = test
        .thread_manager
        .list_thread_ids()
        .await
        .into_iter()
        .find(|id| *id != root)
        .expect("the normal worker was created");
    let worker = test.thread_manager.get_thread(worker_id).await?;
    let other = test
        .thread_manager
        .start_thread(test.start_thread_options().await)
        .await?
        .thread;
    // Both streams are held before response.completed. Returning Paused requires both
    // inferences to settle; a root-only cancellation cannot satisfy this assertion.
    let paused =
        tokio::time::timeout(Duration::from_secs(10), test.codex.pause_execution()).await??;
    assert_eq!(
        paused,
        ConversationExecutionState {
            root_thread_id: root,
            state: ExecutionControlState::Paused
        }
    );
    assert_paused(&test).await?;
    assert_eq!(worker.execution_state().await?, paused);
    assert_eq!(
        other.execution_state().await?.state,
        ExecutionControlState::Running
    );
    other
        .start_or_steer_turn(input("Independent root keeps working."))
        .await?;
    complete(&other).await;
    let requests = streaming.requests().await;
    assert_eq!(requests.len(), 3);
    let independent: Value = serde_json::from_slice(&requests[2])?;
    assert!(
        independent
            .to_string()
            .contains("Independent root keeps working.")
    );
    assert_paused(&test).await?;
    worker
        .start_or_steer_turn(input("Internal worker instruction.").from_agent())
        .await?;
    assert_paused(&test).await?;
    assert_eq!(
        streaming.requests().await.len(),
        3,
        "agent-origin worker input must not unpause the tree"
    );
    worker
        .start_or_steer_turn(input("Human resumes this worker."))
        .await?;
    for thread in [&test.codex, &worker] {
        let event = wait_for_event(thread, |event| {
            matches!(event, EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_))
        })
        .await;
        let EventMsg::TurnComplete(completed) = event else {
            anyhow::bail!("pausing must preserve both registered turns");
        };
        assert_eq!(completed.error, None);
    }
    let requests = streaming.requests().await;
    assert_eq!(
        requests.len(),
        5,
        "each paused inference must resume exactly once"
    );
    let resumed = requests[3..]
        .iter()
        .map(|body| serde_json::from_slice::<Value>(body))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let worker_request = resumed
        .iter()
        .find(|body| body.to_string().contains("Human resumes this worker."))
        .expect("the accepted human input must reach worker inference");
    assert!(
        worker_request
            .to_string()
            .contains("Internal worker instruction.")
    );
    assert!(
        resumed
            .iter()
            .any(|body| body.to_string().contains("Spawn a normal worker."))
    );
    assert_eq!(
        test.codex.execution_state().await?.state,
        ExecutionControlState::Running
    );
    assert_eq!(
        worker.execution_state().await?,
        test.codex.execution_state().await?
    );
    let _ = root_release.send(());
    let _ = worker_release.send(());
    other.shutdown_and_wait().await?;
    test.codex.shutdown_and_wait().await?;
    streaming.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_preserves_registered_tool_waiter_and_accepts_its_real_response() -> Result<()> {
    use codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem;
    use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
    use codex_protocol::dynamic_tools::DynamicToolResponse;
    use codex_protocol::dynamic_tools::DynamicToolSpec;

    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = test_codex().build_with_auto_env(&server).await?;
    let mut options = test.start_thread_options().await;
    options.dynamic_tools = vec![DynamicToolSpec::Function(DynamicToolFunctionSpec {
        name: "external_result".to_owned(),
        description: "Wait for a host-produced result.".to_owned(),
        input_schema: json!({"type": "object"}),
        defer_loading: false,
    })];
    let thread = test.thread_manager.start_thread(options).await?.thread;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("tool"),
                responses::ev_function_call("tool-waiter", "external_result", "{}"),
                responses::ev_completed("tool"),
            ]),
            answer("tool-finished"),
        ],
    )
    .await;
    thread
        .start_or_steer_turn(input("Wait for the external result."))
        .await?;
    wait_for_event(&thread, |event| {
        matches!(event, EventMsg::DynamicToolCallRequest(_))
    })
    .await;
    thread.pause_execution().await?;
    thread
        .submit(Op::DynamicToolResponse {
            id: "tool-waiter".to_owned(),
            response: DynamicToolResponse {
                content_items: vec![DynamicToolCallOutputContentItem::InputText {
                    text: "HOST_RESULT_WHILE_PAUSED".to_owned(),
                }],
                success: true,
            },
        })
        .await?;
    wait_for_event(&thread, |event| matches!(event, EventMsg::ItemCompleted(event)
        if matches!(&event.item, codex_protocol::items::TurnItem::DynamicToolCall(item) if item.id == "tool-waiter"))).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), thread.wait_until_running())
            .await
            .is_err()
    );
    assert_eq!(
        mock.requests().len(),
        1,
        "tool settlement must not restart paused inference"
    );
    thread.unpause_execution().await?;
    complete(&thread).await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].body_contains_text("HOST_RESULT_WHILE_PAUSED"));
    thread.shutdown_and_wait().await?;
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_exec_approval_can_complete_while_inference_stays_paused() -> Result<()> {
    use codex_config::types::ApprovalsReviewer;
    use codex_protocol::protocol::ReviewDecision;

    skip_if_target_windows!(Ok(()), "uses a POSIX command fixture");
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = test_codex().build_with_auto_env(&server).await?;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("approval"),
                responses::ev_function_call(
                    "approval-waiter",
                    "exec_command",
                    &json!({
                        "cmd": "printf APPROVED_WHILE_PAUSED",
                        "yield_time_ms": 1000,
                    })
                    .to_string(),
                ),
                responses::ev_completed("approval"),
            ]),
            answer("approved-result"),
        ],
    )
    .await;
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, test.config.cwd.as_path());
    test.codex
        .start_or_steer_turn(
            input("Run the command after approval.").with_thread_settings(
                ThreadSettingsOverrides {
                    approval_policy: Some(AskForApproval::UnlessTrusted),
                    approvals_reviewer: Some(ApprovalsReviewer::User),
                    sandbox_policy: Some(sandbox_policy),
                    permission_profile,
                    ..Default::default()
                },
            ),
        )
        .await?;
    let EventMsg::ExecApprovalRequest(approval) = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ExecApprovalRequest(_))
    })
    .await
    else {
        unreachable!();
    };
    test.codex.pause_execution().await?;
    assert_paused(&test).await?;
    test.codex
        .submit(Op::ExecApproval {
            id: approval.effective_approval_id(),
            turn_id: None,
            decision: ReviewDecision::Approved,
        })
        .await?;
    let EventMsg::ExecCommandEnd(end) = wait_for_event(
        &test.codex,
        |event| matches!(event, EventMsg::ExecCommandEnd(end) if end.call_id == "approval-waiter"),
    )
    .await
    else {
        unreachable!();
    };
    assert_eq!(
        (end.exit_code, end.aggregated_output),
        (0, "APPROVED_WHILE_PAUSED".to_owned())
    );
    assert_paused(&test).await?;
    assert_eq!(
        mock.requests().len(),
        1,
        "accepting approval must not resume inference"
    );
    test.codex.unpause_execution().await?;
    complete(&test.codex).await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .function_call_output("approval-waiter")
            .to_string()
            .contains("APPROVED_WHILE_PAUSED")
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_settles_open_inference_and_resumes_the_same_turn_with_accepted_guidance()
-> Result<()> {
    use codex_core::TurnInputSubmission;
    use core_test_support::streaming_sse::StreamingSseChunk;
    use core_test_support::streaming_sse::start_streaming_sse_server;

    skip_if_no_network!(Ok(()));
    let (release, gate) = tokio::sync::oneshot::channel();
    let (streaming, _) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("open")]),
            },
            StreamingSseChunk {
                gate: Some(gate),
                body: responses::sse(vec![responses::ev_completed("open")]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: answer("continued"),
        }],
    ])
    .await;
    let bootstrap = responses::start_mock_server().await;
    let base_url = format!("{}/v1", streaming.uri());
    let test = test_codex()
        .with_config(move |config| {
            config.model_provider.base_url = Some(base_url);
            config.model_provider.supports_websockets = false;
        })
        .build_with_auto_env(&bootstrap)
        .await?;
    let TurnInputSubmission::Started { turn_id, .. } = test
        .codex
        .start_or_steer_turn(input("Start the original task."))
        .await?
    else {
        anyhow::bail!("the initial input must start a turn");
    };
    tokio::time::timeout(
        Duration::from_secs(10),
        streaming.wait_for_request_count(/*count*/ 1),
    )
    .await?;
    // The fixture still holds response.completed: pause must close the inference itself.
    tokio::time::timeout(Duration::from_secs(10), test.codex.pause_execution()).await??;
    test.codex
        .start_or_steer_turn(input("Keep this accepted guidance.").from_agent())
        .await?;
    assert_paused(&test).await?;
    assert_eq!(streaming.requests().await.len(), 1);
    test.codex.unpause_execution().await?;
    let event = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_))
    })
    .await;
    let EventMsg::TurnComplete(completed) = event else {
        anyhow::bail!("pause must retain the registered turn instead of aborting it");
    };
    assert_eq!((completed.turn_id, completed.error), (turn_id, None));
    let requests = streaming.requests().await;
    assert_eq!(requests.len(), 2);
    let continued: Value = serde_json::from_slice(&requests[1])?;
    assert!(continued.to_string().contains("Start the original task."));
    assert!(
        continued
            .to_string()
            .contains("Keep this accepted guidance.")
    );
    let _ = release.send(());
    test.codex.shutdown_and_wait().await?;
    streaming.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ephemeral_conversation_pauses_and_delivers_input_without_persisting_payloads() -> Result<()>
{
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = test_codex()
        .with_config(|config| {
            config.ephemeral = true;
            config
                .features
                .disable(Feature::WakeOnAgentMessages)
                .expect("retain queue-only setup work");
        })
        .build_with_auto_env(&server)
        .await?;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![answer("ephemeral-first"), answer("ephemeral-resumed")],
    )
    .await;
    let journal = test
        .home
        .path()
        .join("execution-control")
        .join(format!("{}.json", test.session_configured.thread_id));
    test.submit_text_turn("Private ordinary ephemeral input.")
        .await?;
    assert!(
        !tokio::fs::try_exists(&journal).await?,
        "ordinary ephemeral input must not be journaled"
    );
    queue_before_pause(&test.codex, "Private ephemeral worker report.").await?;
    test.codex.pause_execution().await?;
    test.codex
        .start_or_steer_turn(input("Private paused ephemeral guidance.").from_agent())
        .await?;
    assert_paused(&test).await?;
    assert!(
        !tokio::fs::try_exists(&journal).await?,
        "paused ephemeral input must remain in memory"
    );
    assert_eq!(mock.requests().len(), 1);
    test.codex.unpause_execution().await?;
    complete(&test.codex).await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].body_contains_text("Private ephemeral worker report."));
    assert!(requests[1].body_contains_text("Private paused ephemeral guidance."));
    assert!(!tokio::fs::try_exists(&journal).await?);
    test.codex.shutdown_and_wait().await?;
    assert!(!tokio::fs::try_exists(&journal).await?);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_root_persists_pause_without_ephemeral_child_payloads() -> Result<()> {
    use codex_protocol::protocol::InternalSessionSource;
    use codex_protocol::protocol::SessionSource;

    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let test = test_codex()
        .with_config(|config| {
            config
                .features
                .disable(Feature::WakeOnAgentMessages)
                .expect("retain queue-only setup work");
        })
        .build_with_auto_env(&server)
        .await?;
    let mut options = test.start_thread_options().await;
    options.config.ephemeral = true;
    options.session_source = Some(SessionSource::Internal(InternalSessionSource::Guardian));
    let child = test
        .thread_manager
        .spawn_internal_session(test.session_configured.thread_id, options)
        .await?
        .thread;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            answer("private-child-first"),
            answer("private-child-resumed"),
        ],
    )
    .await;
    child
        .start_or_steer_turn(input("EPHEMERAL_CHILD_ORDINARY_SECRET").from_agent())
        .await?;
    complete(&child).await;
    queue_before_pause(&child, "EPHEMERAL_CHILD_QUEUED_SECRET").await?;
    assert!(
        test.codex.can_pause_execution().await?,
        "queued child work must make its owning root pauseable"
    );
    test.codex.pause_execution().await?;
    child
        .start_or_steer_turn(input("EPHEMERAL_CHILD_PAUSED_SECRET").from_agent())
        .await?;
    assert_paused(&test).await?;
    assert_eq!(
        child.execution_state().await?,
        test.codex.execution_state().await?
    );
    let journal = test
        .home
        .path()
        .join("execution-control")
        .join(format!("{}.json", test.session_configured.thread_id));
    let stored = tokio::fs::read_to_string(&journal).await?;
    let state: Value = serde_json::from_str(&stored)?;
    assert_eq!(
        state["paused"],
        json!(true),
        "the durable owning root must retain its pause state"
    );
    assert!(
        !stored.contains("EPHEMERAL_CHILD_"),
        "ephemeral child input must be excluded from the durable root's journal"
    );
    test.codex.unpause_execution().await?;
    complete(&child).await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].body_contains_text("EPHEMERAL_CHILD_QUEUED_SECRET"));
    assert!(requests[1].body_contains_text("EPHEMERAL_CHILD_PAUSED_SECRET"));
    assert!(
        !tokio::fs::read_to_string(&journal)
            .await?
            .contains("EPHEMERAL_CHILD_")
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
