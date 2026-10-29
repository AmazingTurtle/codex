//! Own-response high-water observations, including private attributed descendants.

use super::host::Children;
use super::host::RunControl;
use crate::session::session::Session;
use codex_dynamic_workflows::WorkflowHostEvent;
use codex_dynamic_workflows::WorkflowObserver;
use codex_dynamic_workflows::WorkflowTokenUsage;
use codex_protocol::ThreadId;
use codex_protocol::protocol::TokenUsageRecord;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::PoisonError;
use tokio::task::JoinSet;

pub(super) async fn observe(
    owner: Arc<Session>,
    run_id: String,
    children: Children,
    control: Arc<RunControl>,
    observer: WorkflowObserver,
    clock_baseline: u64,
) -> Result<(), String> {
    let tree = &owner.services.local_agent_runtime.execution_control;
    // Subscribing precedes the first snapshot. A changed revision always causes
    // another full member snapshot, so coalesced registrations cannot lose a child.
    let mut members = tree.subscribe_members();
    let mut execution = tree.subscribe_state();
    let mut attached = HashSet::new();
    let mut watching = HashSet::new();
    let mut watches = JoinSet::new();
    let mut checkpoint = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut overflow_reported = false;
    loop {
        members.borrow_and_update();
        execution.borrow_and_update();
        let clock = tree.running_time_snapshot();
        observer(WorkflowHostEvent::ExecutionClock {
            active_duration_ms: clock.active_duration_ms.saturating_sub(clock_baseline),
            at_ms: clock.at_ms,
        })?;
        for (session, attribution, selected) in
            super::usage_ownership::members(&owner, &children, &run_id, &control.usage_scopes)
                .await?
        {
            let direct = children
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains_key(&session.thread_id);
            if direct {
                if !selected.complete {
                    control
                        .attempt_clocks
                        .resume_active(&attribution.attempt_id, selected.gate_active_duration_ms);
                }
                control.attempt_clocks.publish(
                    &observer,
                    &attribution.attempt_id,
                    selected.gate_active_duration_ms,
                    selected.complete,
                )?;
            }
            if selected.incomplete {
                observer(WorkflowHostEvent::UsageIncomplete {
                    reason: "workflow usage ownership metadata exceeds its bounded limit".into(),
                })?;
            }
            if watching.contains(&session.thread_id)
                || (attached.contains(&session.thread_id) && selected.complete)
            {
                continue;
            }
            if !attached.contains(&session.thread_id)
                && attached.len() >= codex_dynamic_workflows::MAX_OBSERVED_THREADS
            {
                if !overflow_reported {
                    observer(WorkflowHostEvent::UsageIncomplete {
                        reason: "workflow usage contributor limit reached".into(),
                    })?;
                    overflow_reported = true;
                }
                continue;
            }
            attached.insert(session.thread_id);
            let thread_id = session.thread_id;
            watching.insert(thread_id);
            let observation = observe_thread(
                session,
                attribution.attempt_id,
                Arc::clone(&observer),
                direct,
                Arc::clone(&control),
            );
            watches.spawn(async move { observation.await.map(|()| thread_id) });
        }
        let finishing = control.finish_requested.is_cancelled();
        if finishing && control.cancellation.is_cancelled() {
            break;
        }
        if finishing && watches.is_empty() && !members.has_changed().unwrap_or(false) {
            break;
        }
        tokio::select! {
            changed = members.changed() => changed.map_err(|_| "workflow tree membership observation ended")?,
            changed = execution.changed() => changed.map_err(|_| "workflow execution observation ended")?,
            _ = control.children_changed.notified() => {},
            _ = checkpoint.tick() => {},
            finished = watches.join_next(), if !watches.is_empty() => {
                let thread = finished.ok_or("workflow usage observer disappeared")?.map_err(|error| error.to_string())??;
                watching.remove(&thread);
            },
            _ = control.finish_requested.cancelled(), if !finishing => {},
            _ = control.observation_cancellation.cancelled() => break,
            _ = owner.services.local_agent_runtime.shutdown.cancelled() => break,
        }
    }
    // Each watcher publishes its final cached snapshot before exit. Joining,
    // rather than aborting, retains usage that raced terminal completion.
    control.observation_cancellation.cancel();
    while let Some(finished) = watches.join_next().await {
        finished.map_err(|error| error.to_string())??;
    }
    let clock = tree.running_time_snapshot();
    observer(WorkflowHostEvent::ExecutionClock {
        active_duration_ms: clock.active_duration_ms.saturating_sub(clock_baseline),
        at_ms: clock.at_ms,
    })
}

async fn observe_thread(
    session: Arc<Session>,
    attempt_id: String,
    observer: WorkflowObserver,
    direct: bool,
    control: Arc<RunControl>,
) -> Result<(), String> {
    let mut status = session.subscribe_agent_status();
    let mut usage = session.subscribe_own_token_usage().await;
    let mut terminal = session.subscribe_workflow_terminal();
    loop {
        let settled = control.settlement_changed.notified();
        tokio::pin!(settled);
        settled.as_mut().enable();
        usage.borrow_and_update();
        terminal.borrow_and_update();
        let snapshot = session.workflow_usage_snapshot().await;
        if direct {
            super::attempt_context::publish(&session, &snapshot, &attempt_id, &observer).await?;
        }
        let selected = super::usage_ownership::select(&snapshot, direct, &control.usage_scopes);
        if direct {
            control.attempt_clocks.publish(
                &observer,
                &attempt_id,
                selected.gate_active_duration_ms,
                selected.complete,
            )?;
        }
        publish_model_turns(&observer, &attempt_id, session.thread_id, &selected)?;
        if selected.incomplete {
            observer(WorkflowHostEvent::UsageIncomplete {
                reason: "workflow usage ownership metadata exceeds its bounded limit".into(),
            })?;
        }
        if let Some(record) = selected.record {
            publish(&observer, &attempt_id, &record)?;
        }
        status.borrow_and_update();
        if selected.complete || !selected.eligible {
            if !selected.complete {
                observer(WorkflowHostEvent::UsageIncomplete {
                    reason: "workflow contributor lost its ownership context before authoritative final usage".into(),
                })?;
            }
            observer(WorkflowHostEvent::UsageComplete {
                attempt_id,
                thread_id: session.thread_id.to_string(),
            })?;
            return Ok(());
        }
        tokio::select! {
            _ = &mut settled => {},
            changed = usage.changed() => changed.map_err(|_| "workflow usage observation ended")?,
            changed = status.changed() => changed.map_err(|_| "workflow child status observation ended")?,
            changed = terminal.changed() => changed.map_err(|_| "workflow terminal usage observation ended")?,
            _ = control.observation_cancellation.cancelled() => {
                let selected = super::usage_ownership::select(&session.workflow_usage_snapshot().await, direct, &control.usage_scopes);
                publish_model_turns(&observer, &attempt_id, session.thread_id, &selected)?;
                if selected.incomplete || !selected.complete {
                    observer(WorkflowHostEvent::UsageIncomplete { reason: "workflow contributor was interrupted before authoritative final usage".into() })?;
                }
                if let Some(record) = selected.record { publish(&observer, &attempt_id, &record)?; }
                if session.active_turn.lock().await.is_none() {
                    observer(WorkflowHostEvent::UsageComplete { attempt_id, thread_id: session.thread_id.to_string() })?;
                }
                return Ok(());
            },
        }
    }
}

pub(super) fn publish(
    observer: &WorkflowObserver,
    attempt_id: &str,
    record: &TokenUsageRecord,
) -> Result<(), String> {
    let own = &record.thread_token_usage;
    let counter = |value: i64| {
        u64::try_from(value).map_err(|_| "workflow usage contains a negative counter".to_string())
    };
    observer(WorkflowHostEvent::Usage {
        attempt_id: attempt_id.to_string(),
        thread_id: record.thread_id.to_string(),
        usage: WorkflowTokenUsage {
            input_tokens: counter(own.input_tokens)?,
            output_tokens: counter(own.output_tokens)?,
            cached_input_tokens: counter(own.cached_input_tokens)?,
            total_tokens: counter(own.total_tokens)?,
        },
    })
}

pub(super) fn reconcile_items(
    observer: &WorkflowObserver,
    attempt_id: &str,
    thread: ThreadId,
    history: &[codex_history::RolloutItem],
) -> Result<(), String> {
    let mut record = None;
    let mut responses = crate::session::ModelResponseCounts::default();
    let mut counted_turns = HashSet::new();
    let mut counted_usage = codex_protocol::protocol::TokenUsage::default();
    let mut saw_start = false;
    let mut saw_thread_beginning = false;
    if !history
        .iter()
        .any(|item| matches!(item, codex_history::RolloutItem::Compacted(_)))
    {
        for item in history {
            let terminal_turn = match item {
                codex_history::RolloutItem::SessionMeta(meta) if meta.meta.id == thread => {
                    saw_thread_beginning = true;
                    continue;
                }
                codex_history::RolloutItem::EventMsg(
                    codex_protocol::protocol::EventMsg::TurnStarted(_),
                ) => {
                    saw_start = true;
                    continue;
                }
                codex_history::RolloutItem::TokenUsageRecord(current)
                    if current.thread_id == thread =>
                {
                    if responses.record(&current.response_id, &current.turn_id) {
                        counted_usage.add_assign(&current.usage);
                    }
                    counted_turns.insert(current.turn_id.as_str());
                    record = Some(current);
                    continue;
                }
                codex_history::RolloutItem::EventMsg(
                    codex_protocol::protocol::EventMsg::TurnComplete(event),
                ) => Some(event.turn_id.as_str()),
                codex_history::RolloutItem::EventMsg(
                    codex_protocol::protocol::EventMsg::TurnAborted(event),
                ) => match event.reason {
                    codex_protocol::protocol::TurnAbortReason::Replaced
                    | codex_protocol::protocol::TurnAbortReason::ReviewEnded => {
                        event.turn_id.as_deref()
                    }
                    codex_protocol::protocol::TurnAbortReason::Interrupted
                    | codex_protocol::protocol::TurnAbortReason::BudgetLimited => continue,
                },
                _ => continue,
            };
            if let Some(record) =
                record.filter(|record| Some(record.turn_id.as_str()) == terminal_turn)
            {
                if responses.incomplete() {
                    observer(WorkflowHostEvent::ModelTurnsIncomplete {
                        attempt_id: attempt_id.into(),
                        thread_id: thread.to_string(),
                    })?;
                }
                observer(WorkflowHostEvent::ModelTurns {
                    attempt_id: attempt_id.into(),
                    thread_id: thread.to_string(),
                    model_turns: (saw_thread_beginning
                        && saw_start
                        && counted_usage == record.thread_token_usage)
                        .then(|| responses.for_turns(counted_turns.iter().copied()))
                        .flatten(),
                })?;
                publish(observer, attempt_id, record)?;
                observer(WorkflowHostEvent::UsageComplete {
                    attempt_id: attempt_id.to_string(),
                    thread_id: thread.to_string(),
                })?;
                return Ok(());
            }
            break;
        }
    }
    // Preserve the store's proven high-water counters. A newest cumulative
    // record cannot prove ownership once its first final is unavailable.
    observer(WorkflowHostEvent::ModelTurns {
        attempt_id: attempt_id.into(),
        thread_id: thread.to_string(),
        model_turns: None,
    })?;
    observer(WorkflowHostEvent::UsageIncomplete {
        reason: "historical workflow usage ownership cannot be proven from the retained context"
            .into(),
    })
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;

pub(super) async fn reconcile_live(
    owner: &Session,
    children: &Children,
    run_id: &str,
    observer: &WorkflowObserver,
    scopes: &super::usage_ownership::Scopes,
) -> Result<(), String> {
    for (session, attribution, selected) in
        super::usage_ownership::members(owner, children, run_id, scopes).await?
    {
        report_selection(
            observer,
            &attribution.attempt_id,
            session.thread_id,
            selected,
        )?;
    }
    Ok(())
}

pub(super) fn report_selection(
    observer: &WorkflowObserver,
    attempt: &str,
    thread: ThreadId,
    selected: super::usage_ownership::Selection,
) -> Result<(), String> {
    publish_model_turns(observer, attempt, thread, &selected)?;
    if selected.incomplete || (selected.complete && selected.record.is_none()) {
        observer(WorkflowHostEvent::UsageIncomplete {
            reason: "workflow contributor usage cannot be completely reconciled".into(),
        })?;
    }
    if let Some(record) = selected.record {
        publish(observer, attempt, &record)?;
    }
    if selected.complete {
        observer(WorkflowHostEvent::UsageComplete {
            attempt_id: attempt.to_string(),
            thread_id: thread.to_string(),
        })?;
    }
    Ok(())
}

fn publish_model_turns(
    observer: &WorkflowObserver,
    attempt: &str,
    thread: ThreadId,
    selected: &super::usage_ownership::Selection,
) -> Result<(), String> {
    if selected.model_turns_incomplete {
        observer(WorkflowHostEvent::ModelTurnsIncomplete {
            attempt_id: attempt.into(),
            thread_id: thread.to_string(),
        })?;
    }
    observer(WorkflowHostEvent::ModelTurns {
        attempt_id: attempt.into(),
        thread_id: thread.to_string(),
        model_turns: selected.model_turns,
    })
}

pub(super) async fn stop_children(
    owner: &Session,
    children: &Children,
    run_id: &str,
    scopes: &super::usage_ownership::Scopes,
) -> Result<(), String> {
    super::stop::children(owner, children, run_id, scopes).await
}
