use super::*;
use codex_history::ResponseItemEnvelope;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

fn completion() -> TurnInput {
    TurnInput::ResponseItem(ResponseItemEnvelope::new(
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: None,
            name: None,
            namespace: None,
            output: FunctionCallOutputPayload::from_text("background result".to_string()),
            internal_chat_message_metadata_passthrough: None,
        },
    ))
}

#[tokio::test]
async fn pause_is_durable_before_inference_closes_and_waits_for_its_lease() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let root = ThreadId::new();
    let control = Arc::new(TreeExecutionControl::default());
    control.initialize(home.path(), root).await?;
    let cancel = CancellationToken::new();
    let inference = control.inference(&cancel).await?;
    let pausing = {
        let control = Arc::clone(&control);
        tokio::spawn(async move { control.pause().await })
    };
    inference.paused().await;
    assert_eq!(
        control.snapshot().await?.state,
        ExecutionControlState::Pausing
    );
    let recovered = TreeExecutionControl::default();
    recovered.initialize(home.path(), root).await?;
    assert_eq!(
        recovered.snapshot().await?,
        ConversationExecutionState {
            root_thread_id: root,
            state: ExecutionControlState::Paused,
        }
    );
    assert!(!pausing.is_finished());
    drop(inference);
    assert_eq!(pausing.await??.state, ExecutionControlState::Paused);
    assert_eq!(control.pause().await?.state, ExecutionControlState::Paused);
    assert_eq!(
        control.unpause().await?.state,
        ExecutionControlState::Running
    );
    control.inference(&cancel).await?;
    Ok(())
}

#[tokio::test]
async fn queued_results_survive_claim_crashes_and_acknowledge_only_durable_ids()
-> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let root = ThreadId::new();
    let child = ThreadId::new();
    let control = TreeExecutionControl::default();
    control.initialize(home.path(), root).await?;
    control.pause().await?;
    let options = codex_protocol::turn_input::TurnStartOptions {
        parent_turn_id: Some("parent-turn".to_string()),
        root_turn_id: Some("root-turn".to_string()),
        service_tier: Some("priority".to_string()),
        final_output_json_schema: Some(serde_json::json!({ "type": "object" })),
        ..Default::default()
    };
    assert!(
        control
            .queue_if_paused(QueuedSignal::new(child, vec![completion()]).on_start(options.clone()))
            .await?
    );
    assert!(control.take_signals(child).await?.is_empty());
    control.unpause().await?;
    assert!(control.take_signals(root).await?.is_empty());
    let input = control
        .take_signals(child)
        .await?
        .into_iter()
        .flat_map(|signal| signal.input)
        .collect::<Vec<_>>();
    assert_eq!(input.len(), 1);
    assert!(control.take_signals(child).await?.is_empty());
    // Claiming is not an acknowledgement. A process restart must redeliver it.
    let recovered = TreeExecutionControl::default();
    recovered.initialize(home.path(), root).await?;
    let recovered_signals = recovered.take_signals(child).await?;
    assert_eq!(
        serde_json::to_value(&recovered_signals[0].start_options)?,
        serde_json::to_value(options)?
    );
    assert_eq!(
        recovered_signals
            .into_iter()
            .flat_map(|signal| signal.input)
            .collect::<Vec<_>>(),
        input
    );
    recovered.acknowledge(child, &HashSet::new()).await?;
    assert!(recovered.has_signals(child).await);
    let TurnInput::ResponseItem(item) = &input[0] else {
        panic!("expected response item")
    };
    let persisted = HashSet::from([item.item.id().expect("journal assigns IDs").to_string()]);
    recovered.acknowledge(child, &persisted).await?;
    let restarted = TreeExecutionControl::default();
    restarted.initialize(home.path(), root).await?;
    assert!(!restarted.has_signals(child).await);
    assert!(restarted.take_signals(child).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn independent_roots_and_cancelled_waiters_do_not_release_a_pause() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let paused = Arc::new(TreeExecutionControl::default());
    let other = Arc::new(TreeExecutionControl::default());
    paused.initialize(home.path(), ThreadId::new()).await?;
    other.initialize(home.path(), ThreadId::new()).await?;
    paused.pause().await?;
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(paused.wait_until_running(&cancel).await.is_err());
    assert_eq!(
        paused.snapshot().await?.state,
        ExecutionControlState::Paused
    );
    other.inference(&CancellationToken::new()).await?;
    assert_eq!(
        other.snapshot().await?.state,
        ExecutionControlState::Running
    );
    assert!(
        !other
            .queue_if_paused(QueuedSignal::new(ThreadId::new(), vec![completion()]))
            .await?
    );
    Ok(())
}

#[tokio::test]
async fn persistence_failure_does_not_cancel_running_inference() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let control = Arc::new(TreeExecutionControl::default());
    control.initialize(home.path(), ThreadId::new()).await?;
    let inference = control.inference(&CancellationToken::new()).await?;
    // The persistence directory becoming a file is a real filesystem failure.
    tokio::fs::write(home.path().join("execution-control"), "unavailable").await?;
    assert!(control.pause().await.is_err());
    assert_eq!(
        control.snapshot().await?.state,
        ExecutionControlState::Running
    );
    assert_eq!(control.inference_count.load(Ordering::Acquire), 1);
    drop(inference);
    Ok(())
}

#[tokio::test]
async fn interrupt_discards_only_its_owned_input_and_stale_process_results() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let root = ThreadId::new();
    let child = ThreadId::new();
    let control = TreeExecutionControl::default();
    control.initialize(home.path(), root).await?;
    control
        .retain_input(
            QueuedSignal::new(root, vec![completion()]),
            SignalDelivery::Owned,
        )
        .await?;
    control.pause().await?;
    control
        .queue_if_paused(QueuedSignal::new(root, vec![completion()]).with_wake_generation(1))
        .await?;
    control
        .queue_if_paused(QueuedSignal::new(child, vec![completion()]).with_wake_generation(1))
        .await?;
    control.discard_interrupted_input(root, 2).await?;
    let recovered = TreeExecutionControl::default();
    recovered.initialize(home.path(), root).await?;
    assert_eq!(
        recovered.snapshot().await?.state,
        ExecutionControlState::Paused
    );
    assert!(!recovered.has_signals(root).await);
    assert!(recovered.has_signals(child).await);
    Ok(())
}

#[tokio::test]
async fn partial_terminal_disposition_never_replays_rejected_input() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let root = ThreadId::new();
    let control = TreeExecutionControl::default();
    control.initialize(home.path(), root).await?;
    let signal = QueuedSignal::new(root, vec![completion(), completion()]);
    let TurnInput::ResponseItem(rejected) = &signal.input[0] else {
        panic!("expected response item")
    };
    let rejected_id = rejected.item.id().expect("journal ID").to_string();
    let surviving = signal.input[1].clone();
    control
        .retain_input(signal.clone(), SignalDelivery::Queued)
        .await?;
    control.retain_input(signal, SignalDelivery::Owned).await?;
    control
        .acknowledge(root, &HashSet::from([rejected_id]))
        .await?;
    let recovered = TreeExecutionControl::default();
    recovered.initialize(home.path(), root).await?;
    let restored = recovered
        .take_signals(root)
        .await?
        .into_iter()
        .flat_map(|signal| signal.input)
        .collect::<Vec<_>>();
    assert_eq!(restored, vec![surviving]);
    Ok(())
}

#[tokio::test]
async fn concurrent_terminal_delivery_retries_keep_one_durable_signal() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let root = ThreadId::new();
    let control = TreeExecutionControl::default();
    control.initialize(home.path(), root).await?;
    let signal = QueuedSignal::new(root, vec![completion()]);
    let TurnInput::ResponseItem(item) = &signal.input[0] else {
        panic!("expected response item")
    };
    let id = item.item.id().expect("stable signal ID").clone();
    let mut retry = signal.clone();
    if let TurnInput::ResponseItem(item) = &mut retry.input[0]
        && let ResponseItem::FunctionCallOutput { output, .. } = &mut item.item
    {
        *output = FunctionCallOutputPayload::from_text(
            "retry cannot replace the accepted result".to_string(),
        );
    }
    let (first, second) = tokio::join!(
        control.retain_input(signal, SignalDelivery::Queued),
        control.retain_input(retry, SignalDelivery::Queued),
    );
    assert_eq!(
        [first?, second?]
            .into_iter()
            .filter(|accepted| *accepted)
            .count(),
        1
    );
    assert!(control.has_delivery(root, &id).await);
    let recovered = TreeExecutionControl::default();
    recovered.initialize(home.path(), root).await?;
    let delivered = recovered.take_signals(root).await?;
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].input.len(), 1);
    Ok(())
}

#[tokio::test]
async fn dormant_guidance_is_memory_only_until_first_pause_captures_it() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let root = ThreadId::new();
    let control = TreeExecutionControl::default();
    control.initialize(home.path(), root).await?;
    let guidance = TurnInput::UserInput {
        content: vec![codex_protocol::user_input::UserInput::Text {
            text: "accepted guidance before pausing".to_string(),
            text_elements: Vec::new(),
        }],
        client_id: None,
        metadata: crate::session::UserInputMetadata {
            acceptance_order: Some(7),
            origin: codex_history::UserInputOrigin::User,
        },
    };
    control
        .retain_input(
            QueuedSignal::new(root, vec![guidance.clone()]),
            SignalDelivery::Owned,
        )
        .await?;
    assert!(!home.path().join("execution-control").exists());
    control.pause().await?;
    let recovered = TreeExecutionControl::default();
    recovered.initialize(home.path(), root).await?;
    assert_eq!(
        recovered.snapshot().await?.state,
        ExecutionControlState::Paused
    );
    recovered.unpause().await?;
    assert_eq!(
        recovered
            .take_signals(root)
            .await?
            .into_iter()
            .flat_map(|signal| signal.input)
            .collect::<Vec<_>>(),
        vec![guidance]
    );
    Ok(())
}

#[tokio::test]
async fn protected_review_source_never_becomes_a_durable_pause_payload() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let root = ThreadId::new();
    let control = TreeExecutionControl::default();
    control.initialize(home.path(), root).await?;
    let signal = QueuedSignal::new(root, vec![completion()]).awaiting_finalization();
    let id = journal::receipt_key(&signal.input[0]).expect("receipt ID");
    control.retain_input(signal, SignalDelivery::Owned).await?;
    control.pause().await?;
    let bytes = tokio::fs::read(
        home.path()
            .join("execution-control")
            .join(format!("{root}.json")),
    )
    .await?;
    assert!(!String::from_utf8(bytes)?.contains("background result"));
    assert!(
        control
            .acknowledge_transient(root, &HashSet::from([id]))
            .await?
    );
    assert!(!control.has_signals(root).await);
    let recovered = TreeExecutionControl::default();
    recovered.initialize(home.path(), root).await?;
    assert!(!recovered.has_signals(root).await);
    Ok(())
}

#[tokio::test]
async fn queued_handoff_activates_a_matching_dormant_owned_receipt() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let root = ThreadId::new();
    let control = TreeExecutionControl::default();
    control.initialize(home.path(), root).await?;
    let signal = QueuedSignal::new(root, vec![completion()]);
    control
        .retain_input(signal.clone(), SignalDelivery::Owned)
        .await?;
    assert!(!home.path().join("execution-control").exists());
    assert!(
        !control
            .retain_input(signal.clone(), SignalDelivery::Queued)
            .await?
    );
    let recovered = TreeExecutionControl::default();
    recovered.initialize(home.path(), root).await?;
    let restored = recovered.take_signals(root).await?;
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].input, signal.input);
    assert!(
        control.take_signals(root).await?.is_empty(),
        "the original live owner retains its claim"
    );
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "hold admission as an unpause handoff barrier"
)]
async fn late_producer_uses_live_delivery_during_unpause_handoff() -> anyhow::Result<()> {
    use std::future::Future;
    use std::task::Poll;

    let (session, _) = crate::session::tests::make_session_and_context().await;
    let session = Arc::new(session);
    let control = Arc::clone(&session.services.local_agent_runtime.execution_control);
    control.register(&session).await;
    control.pause().await?;
    let mail = |text: &str| {
        crate::PendingAgentMessage::from(codex_protocol::protocol::InterAgentCommunication::new(
            codex_protocol::AgentPath::root(),
            codex_protocol::AgentPath::root(),
            Vec::new(),
            text.to_string(),
            /*trigger_turn*/ false,
        ))
    };
    let first = QueuedSignal::new(
        session.thread_id,
        vec![TurnInput::InterAgentCommunication(mail("before unpause"))],
    );
    assert!(control.queue_if_paused(first.clone()).await?);
    let admission = session.turn_start_lock.lock().await;
    let mut unpause = Box::pin(control.unpause());
    // Drive real unpause until its durable release commits and the member drain
    // waits on our admission barrier, while the public inference gate stays shut.
    std::future::poll_fn(|cx| {
        assert!(unpause.as_mut().poll(cx).is_pending());
        if control.storage.try_lock().is_ok_and(|storage| {
            storage
                .as_ref()
                .is_some_and(|storage| !storage.journal.paused)
        }) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
    assert_eq!(
        control.snapshot().await?.state,
        ExecutionControlState::Paused
    );
    let late = QueuedSignal::new(
        session.thread_id,
        vec![TurnInput::InterAgentCommunication(mail("during release"))],
    );
    assert!(
        !control.queue_if_paused(late.clone()).await?,
        "the released journal must transfer this producer to live delivery"
    );
    control
        .retain_input(late.clone(), SignalDelivery::Owned)
        .await?;
    let TurnInput::InterAgentCommunication(message) = late.input[0].clone() else {
        panic!("expected mail")
    };
    session
        .input_queue
        .enqueue_mailbox_communication(message, Default::default())
        .await;
    drop(admission);
    assert_eq!(unpause.await?.state, ExecutionControlState::Running);
    let delivered = session.input_queue.drain_mailbox_input_items().await.0;
    assert_eq!(delivered.len(), 2);
    for signal in [first, late] {
        assert_eq!(
            delivered
                .iter()
                .filter(|input| **input == signal.input[0])
                .count(),
            1
        );
    }
    assert!(
        control.take_signals(session.thread_id).await?.is_empty(),
        "both payloads have live owners"
    );
    Ok(())
}
