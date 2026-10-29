//! Checks Guardian persistence boundaries without blocking unrelated turn progress.

use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_core::config::Constrained;
use codex_history::RolloutItem;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::ThreadSource;
use codex_protocol::turn_input::TurnInput;
use codex_protocol::user_input::UserInput;
use codex_thread_store::AppendThreadItemsParams;
use codex_thread_store::ArchiveThreadParams;
use codex_thread_store::CreateThreadParams;
use codex_thread_store::DeleteThreadParams;
use codex_thread_store::FindPersistedInputIdsParams;
use codex_thread_store::InMemoryThreadStore;
use codex_thread_store::ListThreadsParams;
use codex_thread_store::LoadThreadHistoryParams;
use codex_thread_store::PersistContext;
use codex_thread_store::PersistedInputId;
use codex_thread_store::ReadThreadByRolloutPathParams;
use codex_thread_store::ReadThreadParams;
use codex_thread_store::ResumeThreadParams;
use codex_thread_store::StoredModelContext;
use codex_thread_store::StoredThread;
use codex_thread_store::StoredThreadHistory;
use codex_thread_store::ThreadPage;
use codex_thread_store::ThreadStore;
use codex_thread_store::ThreadStoreFuture;
use codex_thread_store::UpdateThreadMetadataParams;
use core_test_support::responses;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::any::Any;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::timeout;

struct PendingSave {
    history: StoredModelContext,
    complete: oneshot::Sender<()>,
}

struct PendingReviewerStart {
    thread_id: ThreadId,
    complete: oneshot::Sender<()>,
    resumed: oneshot::Receiver<()>,
}

#[derive(Default)]
struct ReviewerSaves {
    thread_id: Option<ThreadId>,
    reviews: usize,
    persists: usize,
    flushes: usize,
}

struct GatedReviewerStore {
    inner: InMemoryThreadStore,
    reviewer: Mutex<ReviewerSaves>,
    saves: mpsc::UnboundedSender<PendingSave>,
    reads: Option<mpsc::UnboundedSender<oneshot::Sender<()>>>,
    starts: Option<mpsc::UnboundedSender<PendingReviewerStart>>,
}

macro_rules! delegate_store_methods {
    ($(fn $name:ident($param:ident: $params:ty) -> $result:ty;)*) => {
        $(fn $name(&self, $param: $params) -> ThreadStoreFuture<'_, $result> {
            ThreadStore::$name(&self.inner, $param)
        })*
    };
}

impl ThreadStore for GatedReviewerStore {
    fn as_any(&self) -> &dyn Any {
        self
    }

    delegate_store_methods! {
        fn resume_thread(params: ResumeThreadParams) -> Arc<Vec<RolloutItem>>;
        fn discard_thread(thread_id: ThreadId) -> ();
        fn load_history(params: LoadThreadHistoryParams) -> StoredThreadHistory;
        fn find_persisted_input_ids(params: FindPersistedInputIdsParams) -> BTreeSet<PersistedInputId>;
        fn read_thread(params: ReadThreadParams) -> StoredThread;
        fn read_thread_by_rollout_path(params: ReadThreadByRolloutPathParams) -> StoredThread;
        fn list_threads(params: ListThreadsParams) -> ThreadPage;
        fn update_thread_metadata(params: UpdateThreadMetadataParams) -> Option<StoredThread>;
        fn archive_thread(params: ArchiveThreadParams) -> ();
        fn unarchive_thread(params: ArchiveThreadParams) -> StoredThread;
        fn delete_thread(params: DeleteThreadParams) -> ();
        fn shutdown_thread(thread_id: ThreadId) -> ();
    }

    fn append_items(&self, params: AppendThreadItemsParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            let is_reviewer = self.reviewer.lock().await.thread_id == Some(params.thread_id);
            let resume = if is_reviewer
                && params
                    .items
                    .iter()
                    .any(|item| matches!(item, RolloutItem::EventMsg(EventMsg::TurnStarted(_))))
                && let Some(starts) = &self.starts
            {
                let (complete, completed) = oneshot::channel();
                let (resume, resumed) = oneshot::channel();
                starts
                    .send(PendingReviewerStart {
                        thread_id: params.thread_id,
                        complete,
                        resumed,
                    })
                    .expect("reviewer start receiver");
                completed.await.expect("release the reviewer start");
                Some(resume)
            } else {
                None
            };
            ThreadStore::append_items(&self.inner, params).await?;
            if let Some(resume) = resume {
                resume.send(()).expect("reviewer resumed receiver");
            }
            Ok(())
        })
    }

    fn create_thread(&self, params: CreateThreadParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            if params.thread_source == Some(ThreadSource::GuardianReview) {
                self.reviewer.lock().await.thread_id = Some(params.thread_id);
            }
            ThreadStore::create_thread(&self.inner, params).await
        })
    }

    fn persist_thread(
        &self,
        thread_id: ThreadId,
        context: PersistContext,
    ) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            {
                let mut reviewer = self.reviewer.lock().await;
                if reviewer.thread_id == Some(thread_id) {
                    reviewer.persists += 1;
                }
            }
            self.inner.persist_thread(thread_id, context).await
        })
    }

    fn load_latest_model_context(
        &self,
        params: LoadThreadHistoryParams,
    ) -> ThreadStoreFuture<'_, StoredModelContext> {
        Box::pin(async move {
            if let Some(reads) = &self.reads {
                let (complete, completed) = oneshot::channel();
                reads.send(complete).expect("read receiver");
                completed.await.expect("release the sender read");
            }
            ThreadStore::load_latest_model_context(&self.inner, params).await
        })
    }

    fn flush_thread(&self, thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            let is_reviewer = {
                let mut reviewer = self.reviewer.lock().await;
                let is_reviewer = reviewer.thread_id == Some(thread_id);
                if is_reviewer {
                    reviewer.flushes += 1;
                }
                is_reviewer
            };
            let pending = if is_reviewer {
                let history = ThreadStore::load_latest_model_context(
                    &self.inner,
                    LoadThreadHistoryParams {
                        thread_id,
                        include_archived: true,
                    },
                )
                .await?;
                let reviews = history.items.iter().filter(|item| matches!(
                    item, RolloutItem::ResponseItem(item)
                        if matches!(&item.item, ResponseItem::Message { role, .. } if role == "assistant")
                )).count();
                let mut reviewer = self.reviewer.lock().await;
                if reviews > reviewer.reviews {
                    reviewer.reviews = reviews;
                    Some(history)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(history) = pending {
                let (complete, completed) = oneshot::channel();
                self.saves
                    .send(PendingSave { history, complete })
                    .expect("save receiver");
                completed.await.expect("release the reviewer save");
            }
            self.inner.flush_thread(thread_id).await
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guardian_saves_each_completed_review_before_releasing_its_action() -> anyhow::Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    core_test_support::skip_if_wine_exec!(Ok(()), "uses the Unix command approval harness");
    let server = responses::start_mock_server().await;
    let (saves, mut pending_saves) = mpsc::unbounded_channel();
    let store = Arc::new(GatedReviewerStore {
        inner: InMemoryThreadStore::default(),
        reviewer: Mutex::new(ReviewerSaves::default()),
        saves,
        reads: None,
        starts: None,
    });
    let test = test_codex()
        .with_thread_store(store)
        .with_history_mode(ThreadHistoryMode::Legacy)
        .with_config(|config| {
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
            config.approvals_reviewer = ApprovalsReviewer::AutoReview;
        })
        .build_with_auto_env(&server)
        .await?;
    let command = json!({
        "cmd": "true", "sandbox_permissions": "require_escalated",
        "justification": "Run the authorized command.",
    })
    .to_string();
    let approval = r#"{"risk_level":"low","user_authorization":"high","outcome":"allow"}"#;
    let mut turns = Vec::new();
    for index in 0..2 {
        turns.push(responses::sse(vec![
            responses::ev_function_call(&format!("command-{index}"), "exec_command", &command),
            responses::ev_completed(&format!("parent-{index}")),
        ]));
        turns.push(responses::sse(vec![
            responses::ev_assistant_message(&format!("review-{index}"), approval),
            responses::ev_completed(&format!("guardian-{index}")),
        ]));
    }
    turns.push(responses::sse(vec![responses::ev_completed("parent-done")]));
    let mock = responses::mount_sse_sequence(&server, turns).await;
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Run both commands.".to_owned(),
            text_elements: Vec::new(),
        }]))
        .await?;

    let mut reviewer_id = None;
    for review in 1..=2 {
        let pending = timeout(Duration::from_secs(10), pending_saves.recv())
            .await?
            .expect("review save");
        assert_eq!(
            pending.history.items.iter().find_map(|item| match item {
                RolloutItem::SessionMeta(meta) => Some(meta.meta.history_mode),
                _ => None,
            }),
            Some(ThreadHistoryMode::Paginated)
        );
        assert_eq!(
            *reviewer_id.get_or_insert(pending.history.thread_id),
            pending.history.thread_id
        );
        assert_eq!(
            pending
                .history
                .items
                .iter()
                .filter(|item| matches!(item, RolloutItem::EventMsg(EventMsg::TurnComplete(_))))
                .count(),
            review,
            "the first save after inference must include turn completion"
        );
        // The parent cannot consume the approval while its review is still being saved.
        assert!(
            timeout(
                Duration::from_millis(50),
                wait_for_event(&test.codex, |event| {
                    matches!(event, EventMsg::ExecCommandBegin(_))
                })
            )
            .await
            .is_err()
        );
        assert_eq!(mock.requests().len(), review * 2);
        pending.complete.send(()).expect("finish save");
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::ExecCommandEnd(_))
        })
        .await;
    }
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(mock.requests().len(), 5);
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_before_guardian_finalization_keeps_raw_evidence_private_until_review_is_admitted()
-> anyhow::Result<()> {
    use anyhow::Context;
    use codex_protocol::protocol::ExecutionControlState;

    core_test_support::skip_if_no_network!(Ok(()));
    core_test_support::skip_if_wine_exec!(Ok(()), "uses the Unix command approval harness");
    const RAW_EVIDENCE: &str = "PENDING_GUARDIAN_EVIDENCE_MARKER";
    let server = responses::start_mock_server().await;
    let (saves, mut pending_saves) = mpsc::unbounded_channel();
    let (starts, mut pending_starts) = mpsc::unbounded_channel();
    let store = Arc::new(GatedReviewerStore {
        inner: InMemoryThreadStore::default(),
        reviewer: Mutex::new(ReviewerSaves::default()),
        saves,
        reads: None,
        starts: Some(starts),
    });
    let command = json!({
        "cmd": "true", "sandbox_permissions": "require_escalated",
        "justification": "Run the authorized command.",
    })
    .to_string();
    let approval = r#"{"risk_level":"low","user_authorization":"high","outcome":"allow"}"#;
    let (release_inference, inference_gate) = oneshot::channel();
    let (streaming, _) = start_streaming_sse_server(vec![
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_function_call("pending-review-command", "exec_command", &command),
                responses::ev_completed("parent-command"),
            ]),
        }],
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![responses::ev_response_created("held-review")]),
            },
            StreamingSseChunk {
                gate: Some(inference_gate),
                body: responses::sse(vec![responses::ev_completed("held-review")]),
            },
        ],
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![
                responses::ev_assistant_message("review", approval),
                responses::ev_completed("guardian-review"),
            ]),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![responses::ev_completed("parent-done")]),
        }],
    ])
    .await;
    let base_url = format!("{}/v1", streaming.uri());
    let test = test_codex()
        .with_thread_store(store.clone())
        .with_history_mode(ThreadHistoryMode::Paginated)
        .with_config(move |config| {
            config.model_provider.base_url = Some(base_url);
            config.model_provider.supports_websockets = false;
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
            config.approvals_reviewer = ApprovalsReviewer::AutoReview;
        })
        .build_with_auto_env(&server)
        .await?;
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: format!("Run the authorized command. {RAW_EVIDENCE}"),
            text_elements: Vec::new(),
        }]))
        .await?;
    // The actual reviewer has retained its raw input, but its startup event cannot
    // finish until the real store gate opens. Finalization has not run yet.
    let pending = timeout(Duration::from_secs(10), pending_starts.recv())
        .await?
        .expect("pending reviewer start");
    let before = {
        let reviewer = store.reviewer.lock().await;
        (reviewer.persists, reviewer.flushes)
    };
    let pause = {
        let codex = Arc::clone(&test.codex);
        tokio::spawn(async move { codex.pause_execution().await })
    };
    // The gated startup append also holds the reviewer's event-ordering lock.
    // Observe closed admission on the root before releasing that storage operation;
    // pause can then publish its reviewer notification and acknowledge settlement.
    timeout(
        Duration::from_secs(10),
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::ExecutionControlChanged(state)
            if state.state == ExecutionControlState::Pausing)
        }),
    )
    .await
    .context("root did not close admission while reviewer startup was held")?;
    assert_eq!(
        test.codex.execution_state().await?.state,
        ExecutionControlState::Pausing
    );
    let after = {
        let reviewer = store.reviewer.lock().await;
        (reviewer.persists, reviewer.flushes)
    };
    assert_eq!(
        after, before,
        "pause must not save the reviewer before finalization"
    );
    let journal = test
        .home
        .path()
        .join("execution-control")
        .join(format!("{}.json", test.session_configured.thread_id));
    assert!(
        !tokio::fs::read_to_string(&journal)
            .await?
            .contains(RAW_EVIDENCE),
        "raw pending reviewer input must not enter the durable execution journal"
    );
    let history = store
        .inner
        .load_latest_model_context(LoadThreadHistoryParams {
            thread_id: pending.thread_id,
            include_archived: true,
        })
        .await?;
    assert!(
        !serde_json::to_string(&history.items)?.contains(RAW_EVIDENCE),
        "pending evidence must not reach reviewer history before finalization"
    );
    assert_eq!(streaming.requests().await.len(), 1);
    assert!(pending_saves.try_recv().is_err());
    pending
        .complete
        .send(())
        .expect("release reviewer startup while still paused");
    timeout(Duration::from_secs(10), pending.resumed).await??;
    let paused = timeout(Duration::from_secs(10), pause)
        .await
        .context("pause did not settle after reviewer startup storage was released")???;
    assert_eq!(paused.state, ExecutionControlState::Paused);
    // Releasing storage must not authorize inference or the parent's command.
    assert!(
        timeout(
            Duration::from_millis(150),
            wait_for_event(&test.codex, |event| {
                matches!(
                    event,
                    EventMsg::ExecCommandBegin(_) | EventMsg::TurnComplete(_)
                )
            })
        )
        .await
        .is_err()
    );
    assert_eq!(
        streaming.requests().await.len(),
        1,
        "reviewer inference stays gated until explicit unpause"
    );
    assert_eq!(
        test.codex.execution_state().await?.state,
        ExecutionControlState::Paused
    );
    assert!(pending_saves.try_recv().is_err());
    test.codex.unpause_execution().await?;
    timeout(
        Duration::from_secs(10),
        streaming.wait_for_request_count(/*count*/ 2),
    )
    .await?;
    let finalized_request: serde_json::Value =
        serde_json::from_slice(&streaming.requests().await[1])?;
    assert!(
        finalized_request.to_string().contains(RAW_EVIDENCE),
        "the finalized review must receive the actual evidence"
    );
    let before = {
        let reviewer = store.reviewer.lock().await;
        (reviewer.persists, reviewer.flushes)
    };
    // The reviewer has finalized its evidence and started inference. A later pause
    // must retain the existing private-review terminal-save boundary as well.
    timeout(Duration::from_secs(10), test.codex.pause_execution()).await??;
    assert_eq!(
        test.codex.execution_state().await?.state,
        ExecutionControlState::Paused
    );
    let after = {
        let reviewer = store.reviewer.lock().await;
        (reviewer.persists, reviewer.flushes)
    };
    assert_eq!(
        after, before,
        "a finalized private review must not be checkpointed before its terminal event"
    );
    assert!(pending_saves.try_recv().is_err());
    assert_eq!(streaming.requests().await.len(), 2);
    test.codex.unpause_execution().await?;
    let saved = timeout(Duration::from_secs(10), pending_saves.recv())
        .await?
        .expect("completed reviewer save");
    assert_eq!(
        saved
            .history
            .items
            .iter()
            .filter(|item| matches!(item, RolloutItem::EventMsg(EventMsg::TurnComplete(_))))
            .count(),
        1,
        "the first save after inference must include turn completion"
    );
    assert_eq!(streaming.requests().await.len(), 3);
    assert!(
        timeout(
            Duration::from_millis(50),
            wait_for_event(&test.codex, |event| {
                matches!(event, EventMsg::ExecCommandBegin(_))
            })
        )
        .await
        .is_err(),
        "the parent must not execute before its completed review is saved"
    );
    saved
        .complete
        .send(())
        .expect("commit the completed review");
    let EventMsg::ExecCommandEnd(end) = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ExecCommandEnd(end) if end.call_id == "pending-review-command")
    }).await else { unreachable!(); };
    assert_eq!(
        (end.status, end.exit_code),
        (codex_protocol::protocol::ExecCommandStatus::Completed, 0)
    );
    let EventMsg::TurnComplete(completed) = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await
    else {
        unreachable!();
    };
    assert_eq!(completed.error, None);
    assert_eq!(streaming.requests().await.len(), 4);
    let parent_history = store
        .inner
        .load_latest_model_context(LoadThreadHistoryParams {
            thread_id: test.session_configured.thread_id,
            include_archived: true,
        })
        .await?;
    let completed_commands = parent_history
        .items
        .iter()
        .filter_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::ItemCompleted(event)) => match &event.item {
                codex_protocol::items::TurnItem::CommandExecution(command)
                    if command.id == "pending-review-command" =>
                {
                    Some((command.status, command.exit_code))
                }
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        completed_commands,
        vec![(
            codex_protocol::items::CommandExecutionStatus::Completed,
            Some(0)
        )],
        "the approved parent action must have exactly one successful durable completion"
    );
    let final_request: serde_json::Value = serde_json::from_slice(&streaming.requests().await[3])?;
    let output_count = final_request["input"]
        .as_array()
        .expect("parent model input")
        .iter()
        .filter(|item| {
            item["type"] == "function_call_output" && item["call_id"] == "pending-review-command"
        })
        .count();
    assert_eq!(
        output_count, 1,
        "the parent model must receive exactly one result for its approved action"
    );
    let _ = release_inference.send(());
    test.codex.shutdown_and_wait().await?;
    streaming.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_history_read_does_not_block_turn_completion() -> anyhow::Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let (finish, gate) = oneshot::channel();
    let (streaming, _) = start_streaming_sse_server(vec![
        vec![StreamingSseChunk {
            gate: Some(gate),
            body: responses::sse(vec![responses::ev_completed("first")]),
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: responses::sse(vec![responses::ev_completed("delivery")]),
        }],
    ])
    .await;
    let (saves, _pending_saves) = mpsc::unbounded_channel();
    let (reads, mut pending_reads) = mpsc::unbounded_channel();
    let store = Arc::new(GatedReviewerStore {
        inner: InMemoryThreadStore::default(),
        reviewer: Mutex::new(ReviewerSaves::default()),
        saves,
        reads: Some(reads),
        starts: None,
    });
    let base_url = format!("{}/v1", streaming.uri());
    let test = test_codex()
        .with_thread_store(store.clone())
        .with_config(move |config| config.model_provider.base_url = Some(base_url))
        .build_with_auto_env(&server)
        .await?;
    let first = test
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Inspect staging.".to_owned(),
            text_elements: Vec::new(),
        }]))
        .await?;
    streaming.wait_for_request_count(/*count*/ 1).await;
    let source = ThreadId::new();
    let delivery: ResponseItem = serde_json::from_value(json!({
        "type": "function_call_output", "id": "delivery",
        "name": "send_message", "namespace": "cloud_threads",
        "output": format!("<codex_delegation><source_thread_id>{source}</source_thread_id><input>Inspect.</input></codex_delegation>"),
    }))?;
    let codex = Arc::clone(&test.codex);
    let admission = tokio::spawn(async move {
        codex
            .start_or_steer_turn(TurnInputRequest::new(TurnInput::ResponseItem(delivery)))
            .await
    });
    let read = timeout(Duration::from_secs(/*secs*/ 3), pending_reads.recv())
        .await?
        .expect("sender read");
    finish.send(()).expect("finish first turn");
    // This must finish before the five-second sender-read timeout. Completion and
    // cancellation both need the active-turn lock; submission-loop interrupts are serial.
    timeout(
        Duration::from_secs(/*secs*/ 3),
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        }),
    )
    .await?;
    read.send(()).expect("release sender read");
    let accepted = timeout(Duration::from_secs(/*secs*/ 3), admission).await???;
    assert_ne!(accepted, first);
    let TurnInputSubmission::Started { turn_id, .. } = accepted else {
        panic!("delivery must start a new turn after the first finishes");
    };
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let history = test.codex.conversation_history_snapshot().await;
    let evidence = history
        .retained_context()
        .expect("retained context")
        .sender_user_messages()
        .expect("explicit missing sender evidence");
    assert_eq!(evidence.receiver_turn_id, turn_id);
    assert_eq!(evidence.receiver_message_id, "delivery");
    assert!(
        evidence
            .text
            .contains("No sender user messages are available.")
    );
    assert_eq!(store.inner.calls().await.load_latest_model_context, 1);
    test.codex.shutdown_and_wait().await?;
    streaming.shutdown().await;
    Ok(())
}
