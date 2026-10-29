use super::*;
use crate::session::TurnInput;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskResult;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TurnAbortReason;
use pretty_assertions::assert_eq;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn restored_idle_worker_without_a_seal_settles_without_adopting_latest_usage() {
    let (session, _) = crate::session::tests::make_session_and_context().await;
    let mut snapshot = session.workflow_usage_snapshot().await;
    assert_eq!(snapshot.status, AgentStatus::PendingInit);
    snapshot.restored = true;
    snapshot.record = Some(TokenUsageRecord {
        thread_id: session.thread_id,
        turn_id: "later-human".into(),
        session_id: session.session_id(),
        root_turn_id: "later-human".into(),
        response_id: "later-response".into(),
        usage: TokenUsage::default(),
        turn_token_usage: TokenUsage::default(),
        thread_token_usage: TokenUsage {
            total_tokens: 60,
            ..Default::default()
        },
    });
    let selected = select(&snapshot, /*direct*/ true, &Scopes::default());
    assert_eq!(
        selected.model_turns, None,
        "a restored latest usage record is not a response-count history"
    );
    assert_eq!(
        (
            selected.complete,
            selected.incomplete,
            selected.record,
            selected.owned_turn_id
        ),
        (true, true, None, None)
    );
}

#[tokio::test]
async fn fresh_queued_initial_worker_remains_open_until_a_real_terminal() {
    let (session, _) = crate::session::tests::make_session_and_context().await;
    let snapshot = session.workflow_usage_snapshot().await;
    assert_eq!(
        (snapshot.restored, snapshot.status.clone()),
        (false, AgentStatus::PendingInit)
    );
    let selected = select(&snapshot, /*direct*/ true, &Scopes::default());
    assert_eq!(
        (
            selected.complete,
            selected.incomplete,
            selected.record,
            selected.owned_turn_id
        ),
        (false, false, None, None)
    );
}

#[tokio::test]
async fn coalesced_private_reviews_count_only_the_attempts_owned_parent_turns() {
    let (parent, turn) = crate::session::tests::make_session_and_context().await;
    parent.record_workflow_usage_turn(&turn).await;
    let scopes = Scopes::default();
    select(
        &parent.workflow_usage_snapshot().await,
        /*direct*/ true,
        &scopes,
    );
    let (reviewer, review) = crate::session::tests::make_session_and_context().await;
    reviewer.record_workflow_usage_turn(&review).await;
    for response in ["private-first", "private-second"] {
        reviewer
            .record_response_token_usage(&review, response, Some(&TokenUsage::default()))
            .await;
    }
    reviewer.seal_workflow_usage(&review).await;
    let mut snapshot = reviewer.workflow_usage_snapshot().await;
    snapshot.private_reviewer = true;
    snapshot.parent_thread_id = Some(parent.thread_id);
    snapshot.reviews[0].parent_turn_id = Some(turn.sub_id.clone());
    // A later private review for an unrelated parent turn must not enter this ledger.
    let unrelated = reviewer.new_default_turn().await;
    reviewer
        .record_response_token_usage(
            &unrelated,
            "unrelated-private",
            Some(&TokenUsage::default()),
        )
        .await;
    let counts = reviewer.workflow_usage_snapshot().await.response_counts;
    snapshot.response_counts = counts;
    snapshot
        .response_counts
        .record_turn(&review.sub_id, Some(turn.sub_id.clone()));
    snapshot
        .response_counts
        .record_turn(&unrelated.sub_id, Some("other-parent-turn".into()));
    let selected = select(&snapshot, /*direct*/ false, &scopes);
    assert_eq!(
        (
            selected.model_turns,
            selected.model_turns_incomplete,
            selected.complete
        ),
        (Some(2), false, true)
    );
    assert_eq!(
        cached(&scopes, reviewer.thread_id).unwrap().model_turns,
        Some(2)
    );
}

struct HeldTask(Arc<Notify>);

impl SessionTask for HeldTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }
    fn span_name(&self) -> &'static str {
        "workflow_owned_turn_test"
    }
    async fn run(
        self: Arc<Self>,
        _session: Arc<Session>,
        _ctx: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        cancellation: CancellationToken,
    ) -> SessionTaskResult {
        self.0.notify_one();
        cancellation.cancelled().await;
        Ok(None)
    }
}

#[tokio::test]
async fn captured_owned_turn_cannot_interrupt_a_later_registered_turn() {
    let (session, turn, _events) = crate::session::tests::make_session_and_context_with_rx().await;
    let started = Arc::new(Notify::new());
    session
        .spawn_task(
            Arc::clone(&turn),
            Vec::new(),
            HeldTask(Arc::clone(&started)),
        )
        .await;
    started.notified().await;
    let selected = select(
        &session.workflow_usage_snapshot().await,
        /*direct*/ true,
        &Scopes::default(),
    );
    let captured = selected
        .owned_turn_id
        .expect("the owned turn should be captured");
    session
        .abort_turn_if_active(&captured, TurnAbortReason::Replaced, /*error*/ None)
        .await;
    let later = session.new_default_turn().await;
    assert_ne!(captured, later.sub_id);
    session
        .spawn_task(
            Arc::clone(&later),
            Vec::new(),
            HeldTask(Arc::clone(&started)),
        )
        .await;
    started.notified().await;
    assert!(
        !session
            .abort_turn_if_active(&captured, TurnAbortReason::Interrupted, /*error*/ None)
            .await
    );
    assert!(!session.abort_workflow_turn_if_active(&captured).await);
    assert!(
        !session.workflow_usage_snapshot().await.ownership_retired,
        "a stale workflow guard must not retire the later task's ownership"
    );
    assert_eq!(
        session
            .workflow_usage_snapshot()
            .await
            .active_turn
            .map(|turn| turn.turn_id),
        Some(later.sub_id.clone())
    );
    session.abort_all_tasks(TurnAbortReason::Replaced).await;
}

#[tokio::test]
async fn paused_initial_input_is_rejected_without_opening_the_gate_or_waiting_for_inference() {
    let (session, _, _events) = crate::session::tests::make_session_and_context_with_rx().await;
    let control = &session.services.local_agent_runtime.execution_control;
    control.pause().await.expect("pause the initialized tree");
    let communication = codex_protocol::protocol::InterAgentCommunication::new(
        codex_protocol::AgentPath::root(),
        codex_protocol::AgentPath::root(),
        Vec::new(),
        "initial worker input".into(),
        /*trigger_turn*/ true,
    );
    let initial = crate::execution_control::QueuedSignal::new(
        session.thread_id,
        vec![TurnInput::InterAgentCommunication(communication.into())],
    );
    assert!(
        control
            .queue_if_paused(initial)
            .await
            .expect("accept the native producer's paused initial input")
    );
    assert!(control.has_signals(session.thread_id).await);
    let scopes = Scopes::default();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        session.cancel_pending_workflow_start(|| stop_selected(&scopes, session.thread_id)),
    )
    .await
    .expect("pending cancellation must not wait for an open gate")
    .expect("discard durable initial input");
    assert!(matches!(
        result,
        crate::session::PendingWorkflowCancellation::Cancelled
    ));
    assert!(control.is_paused());
    assert!(!control.has_signals(session.thread_id).await);
    assert!(
        cached(&scopes, session.thread_id)
            .expect("the stopped scope is sealed")
            .complete
    );
}
