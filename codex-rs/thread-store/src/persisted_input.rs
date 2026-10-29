//! Bounded delivery receipts from raw persisted history, including compacted inputs.

use std::collections::BTreeSet;

use codex_protocol::ResponseItemId;
use codex_protocol::ThreadId;
use codex_rollout::RolloutItem;

use crate::ThreadStoreError;
use crate::ThreadStoreResult;

/// Maximum number of delivery keys accepted by one persisted-input lookup.
pub const MAX_PERSISTED_INPUT_IDS: usize = 256;

/// A durable input identity independent of model-context compaction and UI projection.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PersistedInputId {
    /// Matches a raw response item's ID or a legacy agent communication's ID.
    ResponseItem(ResponseItemId),
    /// Matches the acceptance order attached to persisted user input.
    UserInputOrder(u64),
    /// Matches an immutable workflow UI event independently of model input.
    WorkflowLifecycle(String),
}

/// A bounded query against the requested thread's persisted rollout lineage.
///
/// Callers must flush pending writes before querying when they need durable delivery proof.
/// Archived lineage segments remain eligible. Returned keys are a subset of `ids`.
#[derive(Clone, Debug)]
pub struct FindPersistedInputIdsParams {
    pub thread_id: ThreadId,
    pub ids: Vec<PersistedInputId>,
}

pub(crate) struct PersistedInputSearch {
    remaining: BTreeSet<PersistedInputId>,
    found: BTreeSet<PersistedInputId>,
}

impl PersistedInputSearch {
    pub(crate) fn new(ids: Vec<PersistedInputId>) -> ThreadStoreResult<Self> {
        if ids.len() > MAX_PERSISTED_INPUT_IDS {
            return Err(ThreadStoreError::InvalidRequest {
                message: format!(
                    "persisted-input lookup accepts at most {MAX_PERSISTED_INPUT_IDS} keys"
                ),
            });
        }
        Ok(Self {
            remaining: ids.into_iter().collect(),
            found: BTreeSet::new(),
        })
    }

    pub(crate) fn observe(&mut self, item: &RolloutItem) {
        match item {
            RolloutItem::EventMsg(codex_protocol::protocol::EventMsg::WorkflowLifecycle(event)) => {
                self.record(PersistedInputId::WorkflowLifecycle(event.id.clone()));
            }
            RolloutItem::ResponseItem(envelope) => {
                if let Some(id) = envelope.item.id() {
                    self.record(PersistedInputId::ResponseItem(id.clone()));
                }
                if envelope.item.is_user_message()
                    && let Some(order) = envelope
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.user_input_order)
                {
                    self.record(PersistedInputId::UserInputOrder(order));
                }
            }
            RolloutItem::InterAgentCommunication(mail) => {
                if let Some(id) = &mail.id {
                    self.record(PersistedInputId::ResponseItem(id.clone()));
                }
            }
            _ => {}
        }
    }

    fn record(&mut self, id: PersistedInputId) {
        if self.remaining.remove(&id) {
            self.found.insert(id);
        }
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.remaining.is_empty()
    }

    pub(crate) fn finish(self) -> BTreeSet<PersistedInputId> {
        self.found
    }
}
