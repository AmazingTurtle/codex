//! Observes this thread's cumulative usage without sharing its event receiver.

use super::session::Session;
use codex_protocol::protocol::TokenUsageRecord;
use tokio::sync::watch;

pub(super) struct OwnTokenUsageWatch {
    pub(super) current: watch::Sender<Option<TokenUsageRecord>>,
    terminal: watch::Sender<Option<TerminalUsage>>,
    reviews: std::sync::Mutex<(Vec<TerminalUsage>, bool)>,
    turns: std::sync::Mutex<(Vec<String>, bool)>,
    ownership_retired: std::sync::atomic::AtomicBool,
    owned_records: std::sync::Mutex<std::collections::BTreeMap<String, TokenUsageRecord>>,
    responses: std::sync::Mutex<super::model_response_counts::ModelResponseCounts>,
    retired_gate_ms: std::sync::Mutex<Option<u64>>,
}

#[derive(Clone)]
pub(crate) struct TerminalUsage {
    pub record: Option<TokenUsageRecord>,
    pub turn_id: String,
    pub parent_turn_id: Option<String>,
    pub model_turns: Option<u64>,
    pub model_turns_incomplete: bool,
    pub gate_active_duration_ms: u64,
}

pub(crate) struct WorkflowUsageSnapshot {
    pub thread_id: codex_protocol::ThreadId,
    pub parent_thread_id: Option<codex_protocol::ThreadId>,
    pub active_turn: Option<codex_history::TurnAttribution>,
    pub record: Option<TokenUsageRecord>,
    pub terminal: Option<TerminalUsage>,
    pub reviews: Vec<TerminalUsage>,
    pub reviews_overflowed: bool,
    pub private_reviewer: bool,
    pub restored: bool,
    pub status: codex_protocol::protocol::AgentStatus,
    pub owned_turns: Vec<String>,
    pub ownership_retired: bool,
    pub response_counts: super::model_response_counts::ModelResponseCounts,
    pub gate_active_duration_ms: u64,
}

pub(crate) enum PendingWorkflowCancellation {
    Cancelled,
    Registered { turn_id: String },
    Sealed,
}

impl Default for OwnTokenUsageWatch {
    fn default() -> Self {
        Self {
            current: watch::channel(None).0,
            terminal: watch::channel(None).0,
            reviews: std::sync::Mutex::new((Vec::new(), false)),
            turns: std::sync::Mutex::new((Vec::new(), false)),
            ownership_retired: std::sync::atomic::AtomicBool::new(false),
            owned_records: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            responses: std::sync::Mutex::default(),
            retired_gate_ms: std::sync::Mutex::default(),
        }
    }
}

impl OwnTokenUsageWatch {
    pub(super) fn publish(&self, record: TokenUsageRecord) {
        self.responses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record(&record.response_id, &record.turn_id);
        let turns = self
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if turns.0.contains(&record.turn_id) {
            let mut records = self
                .owned_records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut owned = record.clone();
            // This is the same once-per-response publication as the native
            // thread counter. A later human turn cannot reset this owned sum.
            owned.turn_token_usage = records
                .get(&record.turn_id)
                .map(|previous| previous.turn_token_usage.clone())
                .unwrap_or_default();
            owned.turn_token_usage.add_assign(&record.usage);
            records.insert(record.turn_id.clone(), owned);
        }
        self.current.send_replace(Some(record));
    }
}

impl Session {
    pub(crate) async fn record_workflow_usage_turn(&self, turn: &super::turn_context::TurnContext) {
        let _state = self.state.lock().await;
        let observer = self
            .services
            .thread_extension_data
            .get_or_init(OwnTokenUsageWatch::default);
        observer
            .responses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_turn(&turn.sub_id, turn.turn_metadata_state.parent_turn_id());
        if observer.terminal.borrow().is_some()
            || observer
                .ownership_retired
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }
        let mut turns = observer
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !turns.0.contains(&turn.sub_id) {
            if turns.0.len() < 128 {
                turns.0.push(turn.sub_id.clone());
            } else {
                turns.1 = true;
            }
        }
    }

    /// Called only after selecting the exact owned task under turn-start admission.
    pub(crate) fn retire_workflow_owned_turns(&self) {
        let observer = self
            .services
            .thread_extension_data
            .get_or_init(OwnTokenUsageWatch::default);
        *observer
            .retired_gate_ms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(
            self.services
                .local_agent_runtime
                .execution_control
                .running_time_snapshot()
                .active_duration_ms,
        );
        observer
            .ownership_retired
            .store(true, std::sync::atomic::Ordering::Release);
    }
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "fence initial queued-input rejection with native turn admission"
    )]
    pub(crate) async fn cancel_pending_workflow_start(
        self: &std::sync::Arc<Self>,
        commit_cancelled: impl FnOnce(),
    ) -> codex_protocol::error::Result<PendingWorkflowCancellation> {
        let _admission = self.turn_start_lock.lock().await;
        let state = self.state.lock().await;
        let observer = self
            .services
            .thread_extension_data
            .get_or_init(OwnTokenUsageWatch::default);
        if observer.terminal.borrow().is_some()
            || matches!(
                self.history_initialization,
                codex_history::HistoryInitialization::ColdResume
            )
        {
            return Ok(PendingWorkflowCancellation::Sealed);
        }
        drop(state);
        if let Some(task) = self
            .active_turn
            .lock()
            .await
            .as_ref()
            .and_then(|active| active.task.as_ref())
        {
            return Ok(PendingWorkflowCancellation::Registered {
                turn_id: task.turn_context.sub_id.clone(),
            });
        }
        if self.current_agent_status() != codex_protocol::protocol::AgentStatus::PendingInit {
            return Ok(PendingWorkflowCancellation::Sealed);
        }
        let control = &self.services.local_agent_runtime.execution_control;
        let ids = control
            .pending_input_ids(self.thread_id)
            .await
            .into_iter()
            .filter_map(|id| match id {
                codex_thread_store::PersistedInputId::ResponseItem(id) => Some(id.to_string()),
                codex_thread_store::PersistedInputId::UserInputOrder(order) => {
                    Some(format!("user:{order}"))
                }
                codex_thread_store::PersistedInputId::WorkflowLifecycle(_) => None,
            })
            .collect();
        control.acknowledge(self.thread_id, &ids).await?;
        for input in self.input_queue.drain_mailbox_input_items().await.0 {
            self.discard_rejected_input(&input).await?;
        }
        self.mark_interrupted();
        let generation = self
            .wake_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            + 1;
        self.wake_tracker.invalidate_commands(generation);
        self.discard_interrupted_execution_input(generation).await;
        commit_cancelled();
        Ok(PendingWorkflowCancellation::Cancelled)
    }
    pub(crate) async fn seal_workflow_usage(&self, turn: &super::turn_context::TurnContext) {
        let state = self.state.lock().await;
        let observer = self
            .services
            .thread_extension_data
            .get_or_init(OwnTokenUsageWatch::default);
        if observer
            .ownership_retired
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }
        let (model_turns, model_turns_incomplete) = {
            let turns = observer
                .turns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let responses = observer
                .responses
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                responses.for_turns(turns.0.iter().map(String::as_str)),
                responses.incomplete() || turns.1,
            )
        };
        let terminal = TerminalUsage {
            record: state.latest_token_usage_record.clone(),
            turn_id: turn.sub_id.clone(),
            parent_turn_id: turn.turn_metadata_state.parent_turn_id(),
            model_turns,
            model_turns_incomplete,
            gate_active_duration_ms: self
                .services
                .local_agent_runtime
                .execution_control
                .running_time_snapshot()
                .active_duration_ms,
        };
        let mut reviews = observer
            .reviews
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = reviews
            .0
            .iter_mut()
            .find(|previous| previous.parent_turn_id == terminal.parent_turn_id)
        {
            *previous = terminal.clone();
        } else if reviews.0.len() < 128 {
            reviews.0.push(terminal.clone());
        } else {
            reviews.1 = true;
        }
        observer.terminal.send_if_modified(|first| {
            if first.is_some() {
                false
            } else {
                *first = Some(terminal);
                true
            }
        });
    }

    pub(crate) fn subscribe_workflow_terminal(&self) -> watch::Receiver<Option<TerminalUsage>> {
        self.services
            .thread_extension_data
            .get_or_init(OwnTokenUsageWatch::default)
            .terminal
            .subscribe()
    }

    pub(crate) async fn workflow_usage_snapshot(&self) -> WorkflowUsageSnapshot {
        let private_reviewer = self.is_private_guardian_reviewer().await;
        // Capture the turn before taking the state lock. A subsequent terminal
        // seal then wins over a newly admitted, independent human turn.
        let active_turn = self
            .active_turn
            .lock()
            .await
            .as_ref()
            .and_then(|active| active.task.as_ref())
            .map(|task| task.turn_context.attribution());
        let state = self.state.lock().await;
        let observer = self
            .services
            .thread_extension_data
            .get_or_init(OwnTokenUsageWatch::default);
        let reviews = observer
            .reviews
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let turns = observer
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ownership_retired = observer
            .ownership_retired
            .load(std::sync::atomic::Ordering::Acquire);
        let record = if ownership_retired {
            // Thread totals can already include a later independent human turn.
            // Sum only the frozen owned turns' actual cumulative response usage.
            let records = observer
                .owned_records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            records.values().last().cloned().map(|mut record| {
                record.thread_token_usage = records.values().fold(
                    codex_protocol::protocol::TokenUsage::default(),
                    |mut total, current| {
                        total.add_assign(&current.turn_token_usage);
                        total
                    },
                );
                record
            })
        } else {
            state.latest_token_usage_record.clone()
        };
        WorkflowUsageSnapshot {
            thread_id: self.thread_id,
            parent_thread_id: state.session_configuration.parent_thread_id,
            active_turn,
            record,
            terminal: observer.terminal.borrow().clone(),
            reviews: reviews.0.clone(),
            reviews_overflowed: reviews.1 || turns.1,
            private_reviewer,
            restored: matches!(
                self.history_initialization,
                codex_history::HistoryInitialization::ColdResume
            ),
            status: self.current_agent_status(),
            owned_turns: turns.0.clone(),
            ownership_retired,
            response_counts: observer
                .responses
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            gate_active_duration_ms: observer
                .terminal
                .borrow()
                .as_ref()
                .map(|terminal| terminal.gate_active_duration_ms)
                .or_else(|| {
                    *observer
                        .retired_gate_ms
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                })
                .unwrap_or_else(|| {
                    self.services
                        .local_agent_runtime
                        .execution_control
                        .running_time_snapshot()
                        .active_duration_ms
                }),
        }
    }
    pub(crate) fn current_agent_status(&self) -> codex_protocol::protocol::AgentStatus {
        self.agent_status.borrow().clone()
    }

    pub(crate) fn subscribe_agent_status(
        &self,
    ) -> watch::Receiver<codex_protocol::protocol::AgentStatus> {
        self.agent_status.subscribe()
    }

    pub(crate) async fn subscribe_own_token_usage(
        &self,
    ) -> watch::Receiver<Option<TokenUsageRecord>> {
        // Recording takes the same lock through publication. Seeding and subscribing
        // therefore cannot overwrite a newer record or miss the initial update.
        let state = self.state.lock().await;
        let observer = self
            .services
            .thread_extension_data
            .get_or_init(OwnTokenUsageWatch::default);
        observer
            .current
            .send_replace(state.latest_token_usage_record.clone());
        observer.current.subscribe()
    }
}

#[cfg(test)]
#[path = "token_usage_observer_tests.rs"]
mod tests;
