//! Freeze native contributions at their authoritative final; private reviews follow owned parent turns.

use super::host::ChildOwner;
use super::host::Children;
use crate::session::WorkflowUsageSnapshot;
use crate::session::session::Session;
use codex_protocol::ThreadId;
use codex_protocol::protocol::TokenUsageRecord;
use codex_thread_store::ReadThreadParams;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

#[derive(Default)]
pub(super) struct NativeScope {
    turns: HashSet<String>,
    sealed: bool,
    record: Option<TokenUsageRecord>,
    incomplete: bool,
    model_turns: Option<u64>,
    model_turns_incomplete: bool,
}

pub(super) type Scopes = Arc<Mutex<HashMap<ThreadId, NativeScope>>>;

pub(super) struct Selection {
    pub record: Option<TokenUsageRecord>,
    pub complete: bool,
    pub eligible: bool,
    pub incomplete: bool,
    pub owned_turn_id: Option<String>,
    pub model_turns: Option<u64>,
    pub model_turns_incomplete: bool,
    pub gate_active_duration_ms: u64,
}

pub(super) fn select(snapshot: &WorkflowUsageSnapshot, direct: bool, scopes: &Scopes) -> Selection {
    let mut scopes = scopes.lock().unwrap_or_else(PoisonError::into_inner);
    let parent_turns = snapshot
        .parent_thread_id
        .and_then(|parent| scopes.get(&parent))
        .map(|scope| scope.turns.clone())
        .unwrap_or_default();
    if snapshot.private_reviewer {
        let active_owned = snapshot
            .active_turn
            .as_ref()
            .and_then(|turn| turn.parent_turn_id.as_ref())
            .is_some_and(|parent| parent_turns.contains(parent));
        let terminal = snapshot
            .reviews
            .iter()
            .filter(|terminal| {
                terminal
                    .parent_turn_id
                    .as_ref()
                    .is_some_and(|parent| parent_turns.contains(parent))
            })
            .max_by_key(|terminal| {
                terminal
                    .record
                    .as_ref()
                    .map(|record| record.thread_token_usage.total_tokens)
            });
        let scope = scopes.entry(snapshot.thread_id).or_default();
        for review in &snapshot.reviews {
            if review
                .parent_turn_id
                .as_ref()
                .is_some_and(|parent| parent_turns.contains(parent))
            {
                if scope.turns.len() < 128 {
                    scope.turns.insert(review.turn_id.clone());
                } else if !scope.turns.contains(&review.turn_id) {
                    scope.incomplete = true;
                }
            }
        }
        if active_owned && let Some(turn) = &snapshot.active_turn {
            if scope.turns.len() < 128 {
                scope.turns.insert(turn.turn_id.clone());
            } else if !scope.turns.contains(&turn.turn_id) {
                scope.incomplete = true;
            }
        }
        let current = snapshot
            .record
            .as_ref()
            .filter(|record| scope.turns.contains(&record.turn_id));
        let latest = current
            .into_iter()
            .chain(terminal.and_then(|terminal| terminal.record.as_ref()))
            .max_by_key(|record| record.thread_token_usage.total_tokens);
        if let Some(latest) = latest {
            scope.record = Some(latest.clone());
        }
        scope.model_turns = (!snapshot.restored)
            .then(|| {
                snapshot
                    .response_counts
                    .for_parent_turns(parent_turns.iter().map(String::as_str))
            })
            .flatten();
        scope.model_turns_incomplete |= snapshot.response_counts.incomplete()
            || snapshot.reviews_overflowed
            || scope.incomplete;
        scope.sealed = !active_owned && (terminal.is_some() || snapshot.active_turn.is_none());
        return Selection {
            record: scope.record.clone(),
            complete: !active_owned && (terminal.is_some() || snapshot.active_turn.is_none()),
            eligible: active_owned || terminal.is_some() || !scope.turns.is_empty(),
            incomplete: snapshot.reviews_overflowed || scope.incomplete,
            model_turns: scope.model_turns,
            model_turns_incomplete: scope.model_turns_incomplete,
            gate_active_duration_ms: snapshot.gate_active_duration_ms,
            owned_turn_id: active_owned
                .then(|| {
                    snapshot
                        .active_turn
                        .as_ref()
                        .map(|turn| turn.turn_id.clone())
                })
                .flatten(),
        };
    }
    let parent_turn = snapshot
        .terminal
        .as_ref()
        .and_then(|terminal| terminal.parent_turn_id.as_ref())
        .or_else(|| {
            snapshot
                .active_turn
                .as_ref()
                .and_then(|turn| turn.parent_turn_id.as_ref())
        });
    if !direct && !parent_turn.is_some_and(|parent| parent_turns.contains(parent)) {
        return Selection {
            record: None,
            complete: false,
            eligible: false,
            incomplete: false,
            owned_turn_id: None,
            model_turns: None,
            model_turns_incomplete: false,
            gate_active_duration_ms: snapshot.gate_active_duration_ms,
        };
    }
    if !scopes.contains_key(&snapshot.thread_id)
        && scopes.len() >= codex_dynamic_workflows::MAX_OBSERVED_THREADS
    {
        return Selection {
            record: None,
            complete: snapshot.terminal.is_some(),
            eligible: true,
            incomplete: true,
            owned_turn_id: None,
            model_turns: None,
            model_turns_incomplete: true,
            gate_active_duration_ms: snapshot.gate_active_duration_ms,
        };
    }
    let scope = scopes.entry(snapshot.thread_id).or_default();
    if !scope.sealed {
        scope.incomplete |= snapshot.reviews_overflowed;
        for turn in &snapshot.owned_turns {
            if scope.turns.len() < 128 {
                scope.turns.insert(turn.clone());
            } else if !scope.turns.contains(turn) {
                scope.incomplete = true;
            }
        }
        if snapshot.restored {
            scope.incomplete = true;
            scope.sealed = snapshot.terminal.is_some()
                || (snapshot.active_turn.is_none()
                    && (crate::agent::status::is_final(&snapshot.status)
                        || matches!(
                            snapshot.status,
                            codex_protocol::protocol::AgentStatus::Interrupted
                                | codex_protocol::protocol::AgentStatus::PendingInit
                        )));
        } else if let Some(terminal) = &snapshot.terminal {
            scope.turns.insert(terminal.turn_id.clone());
            scope.record = terminal.record.clone();
            scope.sealed = true;
            scope.model_turns = terminal.model_turns;
        } else {
            if !snapshot.ownership_retired
                && let Some(turn) = &snapshot.active_turn
            {
                if scope.turns.len() < 128 {
                    scope.turns.insert(turn.turn_id.clone());
                } else if !scope.turns.contains(&turn.turn_id) {
                    scope.incomplete = true;
                }
            }
            scope.record = snapshot.record.clone();
        }
        if !snapshot.restored && snapshot.terminal.is_none() {
            scope.model_turns = snapshot
                .response_counts
                .for_turns(scope.turns.iter().map(String::as_str));
        }
        scope.model_turns_incomplete |= snapshot.terminal.as_ref().map_or_else(
            || snapshot.response_counts.incomplete() || snapshot.reviews_overflowed,
            |terminal| terminal.model_turns_incomplete || terminal.model_turns.is_none(),
        );
    }
    Selection {
        record: scope.record.clone(),
        complete: scope.sealed,
        eligible: true,
        incomplete: scope.incomplete,
        model_turns: scope.model_turns,
        model_turns_incomplete: scope.model_turns_incomplete,
        gate_active_duration_ms: snapshot.gate_active_duration_ms,
        owned_turn_id: (!scope.sealed)
            .then(|| {
                snapshot
                    .active_turn
                    .as_ref()
                    .map(|turn| turn.turn_id.clone())
            })
            .flatten(),
    }
}

pub(super) fn stop_selected(scopes: &Scopes, thread: ThreadId) {
    let mut scopes = scopes.lock().unwrap_or_else(PoisonError::into_inner);
    let scope = scopes.entry(thread).or_default();
    scope.sealed = true;
    // Cancellation may prevent a final provider usage report. Retain proven
    // counters and explicitly leave the remaining usage unknown.
    scope.incomplete = true;
}

pub(super) fn retire_selected(scopes: &Scopes, snapshot: &WorkflowUsageSnapshot) {
    let mut scopes = scopes.lock().unwrap_or_else(PoisonError::into_inner);
    let scope = scopes.entry(snapshot.thread_id).or_default();
    if let Some(record) = &snapshot.record {
        scope.record = Some(record.clone());
    }
    scope.turns.extend(snapshot.owned_turns.iter().cloned());
    scope.model_turns = snapshot
        .response_counts
        .for_turns(scope.turns.iter().map(String::as_str));
    scope.model_turns_incomplete |=
        snapshot.response_counts.incomplete() || snapshot.reviews_overflowed;
    scope.sealed = true;
    scope.incomplete = true;
}

pub(super) fn cached(scopes: &Scopes, thread: ThreadId) -> Option<Selection> {
    scopes
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&thread)
        .filter(|scope| scope.sealed)
        .map(|scope| Selection {
            record: scope.record.clone(),
            complete: true,
            eligible: true,
            incomplete: scope.incomplete,
            owned_turn_id: None,
            model_turns: scope.model_turns,
            model_turns_incomplete: scope.model_turns_incomplete,
            gate_active_duration_ms: 0,
        })
}

pub(super) async fn members(
    owner: &Session,
    children: &Children,
    run_id: &str,
    scopes: &Scopes,
) -> Result<Vec<(Arc<Session>, ChildOwner, Selection)>, String> {
    let known = children
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let mut snapshots = HashMap::new();
    let sessions = owner
        .services
        .local_agent_runtime
        .execution_control
        .members();
    for session in &sessions {
        snapshots.insert(session.thread_id, session.workflow_usage_snapshot().await);
    }
    let mut candidates = Vec::new();
    for session in sessions {
        let mut id = session.thread_id;
        let mut seen = HashSet::new();
        let attribution = loop {
            if !seen.insert(id) || seen.len() > 64 {
                return Err("workflow usage ancestry is invalid".into());
            }
            if let Some(attribution) = known.get(&id) {
                break Some(attribution.clone());
            }
            let parent = match snapshots.get(&id) {
                Some(snapshot) => snapshot.parent_thread_id,
                None => {
                    owner
                        .services
                        .thread_store
                        .read_thread(ReadThreadParams {
                            thread_id: id,
                            include_archived: true,
                            include_history: false,
                        })
                        .await
                        .map_err(|error| error.to_string())?
                        .parent_thread_id
                }
            };
            match parent {
                Some(parent) => id = parent,
                None => break None,
            }
        };
        if let Some(attribution) = attribution.filter(|attribution| attribution.run_id == run_id) {
            candidates.push((seen.len(), session, attribution));
        }
    }
    candidates.sort_by_key(|(depth, _, _)| *depth);
    let mut selected = Vec::new();
    for (_, session, attribution) in candidates {
        let snapshot = snapshots
            .get(&session.thread_id)
            .ok_or("workflow member snapshot is unavailable")?;
        let selection = select(snapshot, known.contains_key(&session.thread_id), scopes);
        if selection.eligible {
            selected.push((session, attribution, selection));
        }
    }
    Ok(selected)
}

#[cfg(test)]
#[path = "usage_ownership_tests.rs"]
mod tests;
