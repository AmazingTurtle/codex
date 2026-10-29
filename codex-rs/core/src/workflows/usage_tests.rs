use super::*;
use codex_history::RolloutItem;
use codex_protocol::SessionId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use pretty_assertions::assert_eq;
use std::sync::Mutex;
use test_case::test_case;

#[derive(Debug, PartialEq)]
enum Observation {
    Usage(String, String, WorkflowTokenUsage),
    Complete(String, String),
    Incomplete,
    ModelTurns(Option<u64>),
}

fn record(thread: ThreadId, turn: &str, total_tokens: i64) -> RolloutItem {
    let usage = TokenUsage {
        input_tokens: total_tokens,
        total_tokens,
        ..Default::default()
    };
    RolloutItem::TokenUsageRecord(TokenUsageRecord {
        thread_id: thread,
        turn_id: turn.into(),
        session_id: SessionId::from(thread),
        root_turn_id: turn.into(),
        response_id: format!("response-{turn}"),
        usage: usage.clone(),
        turn_token_usage: usage.clone(),
        thread_token_usage: usage,
    })
}

fn history(thread: ThreadId, reason: TurnAbortReason) -> Vec<RolloutItem> {
    vec![
        record(thread, "owned", /*total_tokens*/ 8),
        RolloutItem::EventMsg(EventMsg::TurnAborted(TurnAbortedEvent {
            root_turn_id: Some("owned".into()),
            turn_id: Some("owned".into()),
            reason,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        })),
        record(thread, "human", /*total_tokens*/ 60),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: Some("human".into()),
            turn_id: "human".into(),
            last_agent_message: None,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        })),
    ]
}

fn reconcile(thread: ThreadId, history: &[RolloutItem]) -> Vec<Observation> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let observer: WorkflowObserver = Arc::new(move |event| {
        let event = match event {
            WorkflowHostEvent::Usage {
                attempt_id,
                thread_id,
                usage,
            } => Observation::Usage(attempt_id, thread_id, usage),
            WorkflowHostEvent::UsageComplete {
                attempt_id,
                thread_id,
            } => Observation::Complete(attempt_id, thread_id),
            WorkflowHostEvent::UsageIncomplete { .. } => Observation::Incomplete,
            WorkflowHostEvent::ModelTurns { model_turns, .. } => {
                Observation::ModelTurns(model_turns)
            }
            unexpected => panic!("unexpected reconciliation event: {unexpected:?}"),
        };
        captured.lock().unwrap().push(event);
        Ok(())
    });
    reconcile_items(&observer, "attempt", thread, history).expect("reconcile owned usage");
    drop(observer);
    Arc::try_unwrap(events).unwrap().into_inner().unwrap()
}

#[test_case(TurnAbortReason::Replaced, 8; "replacement seals the original contribution")]
#[test_case(TurnAbortReason::ReviewEnded, 8; "review end seals the original contribution")]
#[test_case(TurnAbortReason::Interrupted, 60; "interrupted work accepts guided continuation")]
#[test_case(TurnAbortReason::BudgetLimited, 60; "budget interrupted work accepts guided continuation")]
fn historical_abort_barrier_preserves_only_owned_usage(
    reason: TurnAbortReason,
    expected_tokens: u64,
) {
    let thread = ThreadId::new();
    assert_eq!(
        reconcile(thread, &history(thread, reason)),
        vec![
            Observation::ModelTurns(None),
            Observation::Usage(
                "attempt".into(),
                thread.to_string(),
                WorkflowTokenUsage {
                    input_tokens: expected_tokens,
                    total_tokens: expected_tokens,
                    ..Default::default()
                }
            ),
            Observation::Complete("attempt".into(), thread.to_string()),
        ]
    );
}

#[test]
fn definitive_abort_without_an_owned_record_does_not_adopt_later_human_usage() {
    let thread = ThreadId::new();
    let mut history = history(thread, TurnAbortReason::Replaced);
    history.remove(0);
    assert_eq!(
        reconcile(thread, &history),
        vec![Observation::ModelTurns(None), Observation::Incomplete]
    );
}

#[test]
fn historical_count_deduplicates_responses_and_does_not_infer_a_missing_prefix() {
    let thread = ThreadId::new();
    let first = record(thread, "owned", /*total_tokens*/ 2);
    let mut second = record(thread, "owned", /*total_tokens*/ 3);
    let RolloutItem::TokenUsageRecord(record) = &mut second else {
        unreachable!()
    };
    record.response_id = "second-response".into();
    record.thread_token_usage.total_tokens = 5;
    record.thread_token_usage.input_tokens = 5;
    record.turn_token_usage = record.thread_token_usage.clone();
    let start = RolloutItem::EventMsg(EventMsg::TurnStarted(
        codex_protocol::protocol::TurnStartedEvent {
            turn_attribution: None,
            turn_id: "owned".into(),
            root_turn_id: Some("owned".into()),
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: codex_protocol::config_types::ModeKind::Default,
        },
    ));
    let terminal = RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
        root_turn_id: Some("owned".into()),
        turn_id: "owned".into(),
        last_agent_message: None,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
        time_to_first_token_ms: None,
    }));
    let beginning = RolloutItem::SessionMeta(Box::new(codex_protocol::protocol::SessionMetaLine {
        meta: codex_protocol::protocol::SessionMeta {
            id: thread,
            session_id: SessionId::from(thread),
            ..Default::default()
        },
        git: None,
    }));
    let full = vec![
        beginning,
        start.clone(),
        first.clone(),
        first,
        second.clone(),
        terminal.clone(),
    ];
    assert_eq!(
        reconcile(thread, &full).first(),
        Some(&Observation::ModelTurns(Some(2)))
    );
    let gap = vec![start, second, terminal];
    assert_eq!(
        reconcile(thread, &gap).first(),
        Some(&Observation::ModelTurns(None))
    );
}
