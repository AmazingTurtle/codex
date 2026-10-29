use super::*;
use codex_protocol::protocol::TokenUsage;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn retired_owned_usage_keeps_all_original_responses_and_excludes_a_later_human() {
    let (session, owned) = crate::session::tests::make_session_and_context().await;
    let usage = |tokens| TokenUsage {
        input_tokens: tokens,
        total_tokens: tokens,
        ..Default::default()
    };
    session.record_workflow_usage_turn(&owned).await;
    session
        .record_response_token_usage(&owned, "owned-one", Some(&usage(3)))
        .await;
    session
        .record_response_token_usage(&owned, "owned-two", Some(&usage(5)))
        .await;
    session.retire_workflow_owned_turns();
    let human = session.new_default_turn().await;
    session.record_workflow_usage_turn(&human).await;
    session
        .record_response_token_usage(&human, "human", Some(&usage(60)))
        .await;
    // A response already owned before cancellation can report after a new
    // human turn. Its native per-turn counter resets, but owned spending cannot.
    session
        .record_response_token_usage(&owned, "owned-late", Some(&usage(2)))
        .await;
    session.seal_workflow_usage(&human).await;
    let snapshot = session.workflow_usage_snapshot().await;
    let raw = session
        .subscribe_own_token_usage()
        .await
        .borrow()
        .clone()
        .expect("native own usage");
    let owned_record = snapshot.record.expect("retired owned response usage");
    assert_eq!(
        (
            snapshot.ownership_retired,
            snapshot.owned_turns,
            snapshot.terminal.is_some()
        ),
        (true, vec![owned.sub_id.clone()], false)
    );
    assert_eq!(
        (
            owned_record.turn_token_usage,
            owned_record.thread_token_usage
        ),
        (usage(10), usage(10))
    );
    assert_eq!(
        (raw.turn_token_usage, raw.thread_token_usage),
        (usage(2), usage(70))
    );
    assert_eq!(
        snapshot.response_counts.for_turns([owned.sub_id.as_str()]),
        Some(3)
    );
}

#[tokio::test]
async fn own_usage_watch_replays_current_record_and_broadcasts_cumulative_updates() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    let mut first = session.subscribe_own_token_usage().await;
    assert_eq!(*first.borrow_and_update(), None);
    let usage = TokenUsage {
        input_tokens: 11,
        cached_input_tokens: 3,
        output_tokens: 7,
        total_tokens: 18,
        ..Default::default()
    };
    session
        .record_response_token_usage(&turn, "response-one", Some(&usage))
        .await;
    first
        .changed()
        .await
        .expect("first subscriber should receive own usage");
    let initial = TokenUsageRecord {
        thread_id: session.thread_id,
        turn_id: turn.sub_id.clone(),
        session_id: session.session_id(),
        root_turn_id: turn.sub_id.clone(),
        response_id: "response-one".into(),
        usage: usage.clone(),
        turn_token_usage: usage.clone(),
        thread_token_usage: usage.clone(),
    };
    assert_eq!(*first.borrow_and_update(), Some(initial.clone()));
    let mut second = session.subscribe_own_token_usage().await;
    assert_eq!(*second.borrow_and_update(), Some(initial));
    session
        .record_response_token_usage(&turn, "response-two", Some(&usage))
        .await;
    first
        .changed()
        .await
        .expect("existing subscriber should retain updates");
    second
        .changed()
        .await
        .expect("new subscriber should also receive updates");
    let total = TokenUsage {
        input_tokens: 22,
        cached_input_tokens: 6,
        output_tokens: 14,
        total_tokens: 36,
        ..Default::default()
    };
    let expected = TokenUsageRecord {
        thread_id: session.thread_id,
        turn_id: turn.sub_id.clone(),
        session_id: session.session_id(),
        root_turn_id: turn.sub_id,
        response_id: "response-two".into(),
        usage,
        turn_token_usage: total.clone(),
        thread_token_usage: total,
    };
    assert_eq!((*first.borrow_and_update()).clone(), Some(expected.clone()));
    assert_eq!((*second.borrow_and_update()).clone(), Some(expected));
}

#[tokio::test]
async fn coalesced_response_counts_deduplicate_and_preserve_usage_bearing_zeroes() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    session.record_workflow_usage_turn(&turn).await;
    let mut subscriber = session.subscribe_own_token_usage().await;
    for response in ["first", "second", "second"] {
        session
            .record_response_token_usage(&turn, response, Some(&TokenUsage::default()))
            .await;
    }
    session
        .record_response_token_usage(&turn, "no-usage", None)
        .await;
    // Both distinct responses arrived before this subscriber read its latest value.
    subscriber.changed().await.expect("usage update");
    assert_eq!(
        subscriber
            .borrow_and_update()
            .as_ref()
            .map(|record| record.response_id.as_str()),
        Some("second")
    );
    session.seal_workflow_usage(&turn).await;
    let snapshot = session.workflow_usage_snapshot().await;
    assert_eq!(
        snapshot
            .terminal
            .as_ref()
            .map(|terminal| terminal.model_turns),
        Some(Some(2))
    );
    let _resubscribed = session.subscribe_own_token_usage().await;
    assert_eq!(
        session
            .workflow_usage_snapshot()
            .await
            .response_counts
            .for_turns([turn.sub_id.as_str()]),
        Some(2)
    );
}

#[test]
fn response_count_overflow_is_unknown_instead_of_a_truncated_total() {
    let mut responses = super::super::model_response_counts::ModelResponseCounts::default();
    for index in 0..4096 {
        responses.record(&format!("response-{index}"), "owned");
    }
    assert_eq!(responses.for_turns(["owned"]), Some(4096));
    responses.record("one-more", "owned");
    assert_eq!(
        (responses.incomplete(), responses.for_turns(["owned"])),
        (true, None)
    );
}
