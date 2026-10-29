use super::*;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::WorkflowAgentMessageFormat;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn native_cold_replay_and_late_proof_use_the_same_exact_message_policy() {
    let (mut chat, _sender, mut events, _ops) =
        crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
    let thread = ThreadId::new();
    chat.thread_id = Some(thread);
    while events.try_recv().is_ok() {}
    let proof = WorkflowAgentMessageProvenance {
        run_id: "run".into(),
        execution_id: "execution".into(),
        attempt_id: "attempt".into(),
        child_thread_id: thread.to_string(),
        turn_id: "owned".into(),
        item_id: "final".into(),
        format: WorkflowAgentMessageFormat::Json,
    };
    let native =
        history_cell::AgentMarkdownCell::new("{\"ok\":true}".into(), chat.config.cwd.as_path());
    let late = chat.identify_agent_message(
        native,
        Some(NativeMessageIdentity {
            thread_id: thread.to_string(),
            turn_id: "owned".into(),
            item_id: "final".into(),
        }),
    );
    let mut wrong = proof.clone();
    wrong.turn_id = "later-human".into();
    let cell = late.as_any().downcast_ref::<WorkflowMessageCell>().unwrap();
    assert!(!cell.apply(&wrong));
    chat.remember_workflow_final(proof.clone());
    let event = events.try_recv().unwrap();
    assert!(matches!(event,AppEvent::ApplyWorkflowFinal(ref observed) if observed==&proof));
    assert!(cell.apply(&proof));
    let native =
        history_cell::AgentMarkdownCell::new("{\"ok\":true}".into(), chat.config.cwd.as_path());
    let cold = chat.identify_agent_message(
        native,
        Some(NativeMessageIdentity {
            thread_id: thread.to_string(),
            turn_id: "owned".into(),
            item_id: "final".into(),
        }),
    );
    assert_eq!(
        (cold.display_lines(60), cold.copy_source()),
        (late.display_lines(60), late.copy_source())
    );
    assert_eq!(cold.copy_source(), Some("{\"ok\":true}"));
    // A later human reply can have identical bytes; it must remain ordinary native Markdown.
    let later = chat.identify_agent_message(
        history_cell::AgentMarkdownCell::new("{\"ok\":true}".into(), chat.config.cwd.as_path()),
        Some(NativeMessageIdentity {
            thread_id: thread.to_string(),
            turn_id: "later-human".into(),
            item_id: "human-final".into(),
        }),
    );
    assert_ne!(later.display_lines(60), cold.display_lines(60));
    // The normal cold item projector receives the same server proof with no workflow center.
    let item = ThreadItem::AgentMessage {
        id: "final".into(),
        text: "{\"ok\":true}".into(),
        workflow: Some(proof),
        phase: None,
        memory_citation: None,
        delivery: None,
        questions: None,
    };
    let items = crate::thread_transcript::thread_items_to_transcript_cells(
        Some(thread),
        &chat.config.cwd,
        [item.clone()],
        crate::thread_transcript::RawReasoningVisibility::Hidden,
        Some(&chat.config),
    );
    assert_eq!(
        (items[0].display_lines(60), items[0].copy_source()),
        (cold.display_lines(60), cold.copy_source())
    );
    chat.replay_thread_item(item, "owned".into(), ReplayKind::ResumeInitialMessages);
    let replay = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) if cell.as_any().is::<WorkflowMessageCell>() => {
                Some(cell)
            }
            _ => None,
        })
        .expect("normal native replay publishes the actual identified history cell");
    assert_eq!(
        (replay.display_lines(60), replay.copy_source()),
        (cold.display_lines(60), cold.copy_source())
    );
}
