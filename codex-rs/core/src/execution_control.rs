//! Durable admission and inference cancellation for one independent conversation tree.

use crate::session::TurnInput;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::protocol::ExecutionControlState;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[path = "execution_control/activity.rs"]
mod activity;
#[path = "execution_control/clock.rs"]
mod clock;
#[path = "execution_control/inference.rs"]
mod inference;
#[path = "execution_control/journal.rs"]
mod journal;
#[path = "execution_control/journal_input.rs"]
mod journal_input;
#[cfg(test)]
#[path = "execution_control_tests.rs"]
mod tests;
use inference::InferenceLease;
use journal::Journal;
pub(crate) use journal::QueuedSignal;
use journal::Storage;
use journal::persist;

/// Execution state of the whole owning conversation, including its workers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationExecutionState {
    pub root_thread_id: ThreadId,
    pub state: ExecutionControlState,
}

pub(crate) enum SignalDelivery {
    Queued,
    Owned,
}

/// Shared by every local controller clone; unrelated roots have separate controls.
pub(crate) struct TreeExecutionControl {
    storage: Mutex<Option<Storage>>,
    transition: Mutex<()>,
    state: watch::Sender<ExecutionControlState>,
    clock: clock::ExecutionClock,
    inference_count: AtomicUsize,
    settled: Notify,
    members: std::sync::Mutex<Vec<Weak<crate::session::session::Session>>>,
    member_revision: watch::Sender<u64>,
    activity_refresh: Mutex<()>,
    last_activity: AtomicBool,
}

impl Default for TreeExecutionControl {
    fn default() -> Self {
        Self {
            storage: Mutex::new(None),
            transition: Mutex::new(()),
            state: watch::channel(ExecutionControlState::Running).0,
            clock: Default::default(),
            inference_count: AtomicUsize::new(0),
            settled: Notify::new(),
            members: Default::default(),
            member_revision: watch::channel(0).0,
            activity_refresh: Mutex::new(()),
            last_activity: AtomicBool::new(false),
        }
    }
}

impl TreeExecutionControl {
    pub(crate) fn running_time_snapshot(&self) -> clock::ExecutionClockSnapshot {
        self.clock.snapshot()
    }

    fn set_state(&self, state: ExecutionControlState) {
        self.clock.transition(state);
        self.state.send_replace(state);
    }
    pub(crate) fn subscribe_state(&self) -> watch::Receiver<ExecutionControlState> {
        self.state.subscribe()
    }

    pub(crate) fn subscribe_members(&self) -> watch::Receiver<u64> {
        self.member_revision.subscribe()
    }
    pub(crate) async fn register(&self, session: &Arc<crate::session::session::Session>) {
        let ephemeral = session.get_config().await.ephemeral;
        if let Some(storage) = self.storage.lock().await.as_mut() {
            if ephemeral {
                storage.ephemeral_threads.insert(session.thread_id);
            }
            let ids = storage
                .journal
                .signals
                .iter()
                .filter(|signal| signal.thread_id == session.thread_id)
                .map(|signal| signal.id.clone())
                .collect::<HashSet<_>>();
            storage.claimed.retain(|id| !ids.contains(id));
        }
        let mut members = self.members.lock().unwrap_or_else(PoisonError::into_inner);
        members.retain(|member| member.strong_count() != 0);
        members.push(Arc::downgrade(session));
        self.member_revision
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    pub(crate) fn members(&self) -> Vec<Arc<crate::session::session::Session>> {
        self.members
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }

    async fn publish(&self, state: ExecutionControlState) -> Result<()> {
        self.set_state(state);
        self.broadcast_state(state).await
    }

    async fn broadcast_state(&self, state: ExecutionControlState) -> Result<()> {
        let root_thread_id = self.snapshot().await?.root_thread_id;
        for session in self.members() {
            session
                .send_event_raw(codex_protocol::protocol::Event {
                    id: crate::session::new_submission_id(),
                    msg: codex_protocol::protocol::EventMsg::ExecutionControlChanged(
                        codex_protocol::protocol::ExecutionControlChangedEvent {
                            root_thread_id,
                            state,
                        },
                    ),
                })
                .await;
        }
        Ok(())
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "load one journal before admission"
    )]
    pub(crate) async fn initialize(&self, home: &Path, root: ThreadId) -> Result<()> {
        let mut storage = self.storage.lock().await;
        if storage.is_some() {
            return Ok(());
        }
        let path = home.join("execution-control").join(format!("{root}.json"));
        let mut active_path = None;
        let journal = match tokio::fs::read(&path).await {
            Ok(bytes) => {
                active_path = Some(path.clone());
                serde_json::from_slice::<Journal>(&bytes)
                    .map_err(|error| CodexErr::Fatal(format!("read execution control: {error}")))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Journal {
                root_thread_id: root,
                paused: false,
                signals: Vec::new(),
            },
            Err(error) => return Err(CodexErr::Fatal(format!("read execution control: {error}"))),
        };
        if journal.root_thread_id != root {
            return Err(CodexErr::Fatal(
                "execution control has another owning root".into(),
            ));
        }
        self.set_state(if journal.paused {
            ExecutionControlState::Paused
        } else {
            ExecutionControlState::Running
        });
        *storage = Some(Storage {
            path: active_path,
            durable_path: Some(path),
            journal,
            claimed: HashSet::new(),
            ephemeral_threads: HashSet::new(),
        });
        Ok(())
    }

    pub(crate) async fn initialize_ephemeral(&self, root: ThreadId) {
        let mut storage = self.storage.lock().await;
        if storage.is_none() {
            *storage = Some(Storage {
                path: None,
                durable_path: None,
                journal: Journal {
                    root_thread_id: root,
                    paused: false,
                    signals: Vec::new(),
                },
                claimed: HashSet::new(),
                ephemeral_threads: HashSet::from([root]),
            });
        }
    }

    pub(crate) fn is_paused(&self) -> bool {
        *self.state.borrow() != ExecutionControlState::Running
    }

    pub(crate) async fn snapshot(&self) -> Result<ConversationExecutionState> {
        let storage = self.storage.lock().await;
        let storage = storage
            .as_ref()
            .ok_or_else(|| CodexErr::Fatal("execution control has not been initialized".into()))?;
        Ok(ConversationExecutionState {
            root_thread_id: storage.journal.root_thread_id,
            state: *self.state.borrow(),
        })
    }

    #[cfg(test)]
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "serialize durable pause and settlement"
    )]
    pub(crate) async fn pause(&self) -> Result<ConversationExecutionState> {
        let _transition = self.transition.lock().await;
        self.pause_after_transition().await
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "revalidate activity at the pause transition"
    )]
    pub(crate) async fn pause_if_active(&self) -> Result<ConversationExecutionState> {
        let _transition = self.transition.lock().await;
        if self.is_paused() || !self.has_pending_activity().await? {
            return self.snapshot().await;
        }
        self.pause_after_transition().await
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "commit journal ownership before cancellation"
    )]
    async fn pause_after_transition(&self) -> Result<ConversationExecutionState> {
        // Capture dormant ownership before checkpointing writers. After this
        // commit, another acknowledgement must flush its representation before
        // removing it; earlier memory-only receipts are fenced by the checkpoint.
        {
            let mut storage = self.storage.lock().await;
            let storage = storage.as_mut().ok_or_else(|| {
                CodexErr::Fatal("execution control has not been initialized".into())
            })?;
            if storage.path.is_none() {
                persist(&storage.durable_path, &storage.journal).await?;
                storage.path = storage.durable_path.clone();
            }
        }
        // A fresh thread can be paused before its first prompt records history.
        // Persist its identity before a durable pause or pending input can outlive it.
        for session in self.members() {
            // Trusted private reviewers save their transcript with TurnComplete,
            // including after input finalization removes its pending marker.
            if session.is_private_guardian_reviewer().await
                || session
                    .services
                    .thread_extension_data
                    .get::<crate::guardian::PendingReviewContext>()
                    .is_some()
            {
                continue;
            }
            session
                .try_ensure_rollout_materialized(codex_thread_store::PersistContext::Standard)
                .await
                .map_err(|error| CodexErr::Fatal(error.to_string()))?;
            session
                .flush_rollout()
                .await
                .map_err(|error| CodexErr::Fatal(error.to_string()))?;
        }
        {
            let mut storage = self.storage.lock().await;
            let storage = storage.as_mut().ok_or_else(|| {
                CodexErr::Fatal("execution control has not been initialized".into())
            })?;
            if !storage.journal.paused {
                let mut journal = storage.journal.clone();
                journal.paused = true;
                persist(&storage.durable_path, &journal).await?;
                storage.path = storage.durable_path.clone();
                storage.journal = journal;
            }
            self.set_state(ExecutionControlState::Pausing);
        }
        self.publish(ExecutionControlState::Pausing).await?;
        loop {
            let notified = self.settled.notified();
            if self.inference_count.load(Ordering::Acquire) == 0 {
                break;
            }
            notified.await;
        }
        self.publish(ExecutionControlState::Paused).await?;
        self.snapshot().await
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "restore queued work before release"
    )]
    pub(crate) async fn unpause(&self) -> Result<ConversationExecutionState> {
        let _transition = self.transition.lock().await;
        if !self.is_paused() {
            return self.snapshot().await;
        }
        let snapshot = {
            let mut storage = self.storage.lock().await;
            let storage = storage.as_mut().ok_or_else(|| {
                CodexErr::Fatal("execution control has not been initialized".into())
            })?;
            if storage.journal.paused {
                let mut journal = storage.journal.clone();
                journal.paused = false;
                persist(&storage.path, &journal).await?;
                storage.journal = journal;
            }
            ConversationExecutionState {
                root_thread_id: storage.journal.root_thread_id,
                state: ExecutionControlState::Running,
            }
        };
        for session in self.members() {
            session.drain_paused_signals().await?;
        }
        self.publish(ExecutionControlState::Running).await?;
        Ok(snapshot)
    }

    pub(crate) async fn wait_until_running(&self, cancel: &CancellationToken) -> Result<()> {
        let mut state = self.state.subscribe();
        loop {
            if *state.borrow_and_update() == ExecutionControlState::Running {
                return Ok(());
            }
            tokio::select! {
                _ = cancel.cancelled() => return Err(CodexErr::TurnAborted),
                changed = state.changed() => {
                    changed.map_err(|_| CodexErr::TurnAborted)?;
                }
            }
        }
    }

    pub(crate) async fn inference(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Result<InferenceLease> {
        loop {
            self.wait_until_running(cancel).await?;
            let storage = self.storage.lock().await;
            if self.is_paused() {
                continue;
            }
            self.inference_count.fetch_add(1, Ordering::AcqRel);
            drop(storage);
            return Ok(InferenceLease {
                control: Arc::clone(self),
            });
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "serialize journal acceptance commits"
    )]
    pub(crate) async fn queue_if_paused(&self, mut signal: QueuedSignal) -> Result<bool> {
        let mut storage = self.storage.lock().await;
        let storage = storage
            .as_mut()
            .ok_or_else(|| CodexErr::Fatal("missing execution journal".into()))?;
        if !storage.journal.paused {
            return Ok(false);
        }
        signal.ephemeral = storage.ephemeral_threads.contains(&signal.thread_id);
        if storage
            .journal
            .signals
            .iter()
            .any(|existing| existing.same_delivery(&signal))
        {
            return Ok(true);
        }
        let mut journal = storage.journal.clone();
        journal.signals.push(signal);
        persist(&storage.path, &journal).await?;
        storage.journal = journal;
        Ok(true)
    }

    /// Retain accepted input until its original destination has flushed it.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "commit delivery ownership atomically"
    )]
    pub(crate) async fn retain_input(
        &self,
        mut signal: QueuedSignal,
        delivery: SignalDelivery,
    ) -> Result<bool> {
        if signal.input.is_empty() {
            return Ok(false);
        }
        let mut storage = self.storage.lock().await;
        let storage = storage
            .as_mut()
            .ok_or_else(|| CodexErr::Fatal("missing execution journal".into()))?;
        signal.ephemeral = storage.ephemeral_threads.contains(&signal.thread_id);
        if let Some(existing) = storage
            .journal
            .signals
            .iter()
            .find(|existing| existing.same_delivery(&signal))
            .map(|signal| signal.id.clone())
        {
            if matches!(delivery, SignalDelivery::Queued) && storage.path.is_none() {
                persist(&storage.durable_path, &storage.journal).await?;
                storage.path = storage.durable_path.clone();
            }
            if matches!(delivery, SignalDelivery::Owned) {
                storage.claimed.insert(existing);
            }
            return Ok(false);
        }
        let mut journal = storage.journal.clone();
        let id = signal.id.clone();
        let path = if matches!(delivery, SignalDelivery::Queued) {
            storage.durable_path.clone()
        } else {
            storage.path.clone()
        };
        journal.signals.push(signal);
        persist(&path, &journal).await?;
        storage.path = path;
        storage.journal = journal;
        if matches!(delivery, SignalDelivery::Owned) {
            storage.claimed.insert(id);
        }
        Ok(true)
    }

    /// Claim a bounded batch, retaining it until the destination history is durable.
    pub(crate) async fn take_signals(&self, thread_id: ThreadId) -> Result<Vec<QueuedSignal>> {
        const BATCH_SIZE: usize = 64;
        let mut storage = self.storage.lock().await;
        let storage = storage
            .as_mut()
            .ok_or_else(|| CodexErr::Fatal("missing execution journal".into()))?;
        if storage.journal.paused {
            return Ok(Vec::new());
        }
        let mut count = 0;
        let mut input = Vec::new();
        for signal in &storage.journal.signals {
            if signal.thread_id == thread_id
                && count < BATCH_SIZE
                && storage.claimed.insert(signal.id.clone())
            {
                input.push(signal.clone());
                count += 1;
            }
        }
        Ok(input)
    }

    pub(crate) async fn has_signals(&self, thread_id: ThreadId) -> bool {
        self.storage.lock().await.as_ref().is_some_and(|storage| {
            storage
                .journal
                .signals
                .iter()
                .any(|signal| signal.thread_id == thread_id)
        })
    }

    pub(crate) async fn has_delivery(
        &self,
        thread_id: ThreadId,
        id: &codex_protocol::ResponseItemId,
    ) -> bool {
        self.storage.lock().await.as_ref().is_some_and(|storage| {
            storage.journal.signals.iter().any(|signal| {
                signal.thread_id == thread_id
                    && signal
                        .input
                        .iter()
                        .any(|input| journal::delivery_id(input) == Some(id))
            })
        })
    }

    pub(crate) async fn pending_input_ids(
        &self,
        thread_id: ThreadId,
    ) -> Vec<codex_thread_store::PersistedInputId> {
        let storage = self.storage.lock().await;
        let Some(storage) = storage.as_ref() else {
            return Vec::new();
        };
        storage
            .journal
            .signals
            .iter()
            .filter(|signal| signal.thread_id == thread_id && !storage.claimed.contains(&signal.id))
            .flat_map(|signal| &signal.input)
            .filter_map(|input| match input {
                TurnInput::UserInput { metadata, .. } => metadata
                    .acceptance_order
                    .map(codex_thread_store::PersistedInputId::UserInputOrder),
                input => journal::delivery_id(input)
                    .cloned()
                    .map(codex_thread_store::PersistedInputId::ResponseItem),
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) async fn max_pending_user_order(&self, thread_id: ThreadId) -> Option<u64> {
        let storage = self.storage.lock().await;
        storage
            .as_ref()?
            .journal
            .signals
            .iter()
            .filter(|signal| signal.thread_id == thread_id)
            .flat_map(|signal| &signal.input)
            .filter_map(|input| match input {
                TurnInput::UserInput { metadata, .. } => metadata.acceptance_order,
                TurnInput::FunctionCallOutput(item) | TurnInput::ResponseItem(item) => item
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.user_input_order),
                TurnInput::InterAgentCommunication(_) => None,
            })
            .max()
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "commit cancellation before admission"
    )]
    pub(crate) async fn discard_interrupted_input(
        &self,
        thread_id: ThreadId,
        generation: u64,
    ) -> Result<()> {
        let mut storage = self.storage.lock().await;
        let storage = storage
            .as_mut()
            .ok_or_else(|| CodexErr::Fatal("missing execution journal".into()))?;
        let mut journal = storage.journal.clone();
        journal.signals.retain(|signal| {
            signal.thread_id != thread_id
                || (!storage.claimed.contains(&signal.id)
                    && signal
                        .wake_generation
                        .is_none_or(|value| value >= generation))
        });
        persist(&storage.path, &journal).await?;
        storage
            .claimed
            .retain(|id| journal.signals.iter().any(|signal| &signal.id == id));
        storage.journal = journal;
        Ok(())
    }

    /// Completes inputs after a flushed destination write or terminal rejection.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "serialize durable receipt removal"
    )]
    pub(crate) async fn acknowledge(
        &self,
        thread_id: ThreadId,
        persisted: &HashSet<String>,
    ) -> Result<()> {
        let mut storage = self.storage.lock().await;
        let storage = storage
            .as_mut()
            .ok_or_else(|| CodexErr::Fatal("missing execution journal".into()))?;
        let mut journal = storage.journal.clone();
        if journal::remove_receipts(&mut journal, thread_id, persisted) {
            persist(&storage.path, &journal).await?;
            storage
                .claimed
                .retain(|id| journal.signals.iter().any(|signal| &signal.id == id));
            storage.journal = journal;
        }
        Ok(())
    }

    /// Dormant and protected input ownership needs no extra rollout save. The
    /// check and removal share the pause commit lock so activation cannot race it.
    pub(crate) async fn acknowledge_transient(
        &self,
        thread_id: ThreadId,
        receipts: &HashSet<String>,
    ) -> Result<bool> {
        let mut storage = self.storage.lock().await;
        let storage = storage
            .as_mut()
            .ok_or_else(|| CodexErr::Fatal("missing execution journal".into()))?;
        if storage.path.is_some()
            && storage.journal.signals.iter().any(|signal| {
                signal.thread_id == thread_id
                    && !signal.ephemeral
                    && !signal.awaiting_finalization
                    && signal.input.iter().any(|input| {
                        journal::receipt_key(input).is_some_and(|id| receipts.contains(&id))
                    })
            })
        {
            return Ok(false);
        }
        journal::remove_receipts(&mut storage.journal, thread_id, receipts);
        let retained = storage
            .journal
            .signals
            .iter()
            .map(|signal| &signal.id)
            .collect::<HashSet<_>>();
        storage.claimed.retain(|id| retained.contains(id));
        Ok(true)
    }
}
