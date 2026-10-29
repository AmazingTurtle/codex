//! A copied fork prefix retains its source identities until the child's own settings.
use crate::ThreadStoreError;
use crate::ThreadStoreResult;
use codex_app_server_protocol::WorkflowLifecycleEvent;
use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_rollout::RolloutItem;
use serde::Deserialize;
use serde::Serialize;

const MAX_SOURCE_DEPTH: usize = 64;

#[derive(Clone, Serialize, Deserialize)]
pub(in crate::local) struct WorkflowOrigins {
    thread_id: ThreadId,
    header_seen: bool,
    prefix_open: bool,
    expected_source: Option<ThreadId>,
    source_owners: Vec<ThreadId>,
}

#[derive(Clone)]
pub(in crate::local) enum WorkflowSourceBoundary {
    SessionMeta {
        thread_id: ThreadId,
        forked_from_id: Option<ThreadId>,
    },
    Settings {
        thread_id: Option<ThreadId>,
    },
}

pub(in crate::local) struct ProvenWorkflowEvent {
    thread_id: ThreadId,
    event: WorkflowLifecycleEvent,
}

impl ProvenWorkflowEvent {
    pub(super) fn thread_id(&self) -> ThreadId {
        self.thread_id
    }
    pub(super) fn into_event(self) -> WorkflowLifecycleEvent {
        self.event
    }
}

impl WorkflowSourceBoundary {
    pub(in crate::local) fn from_item(item: &RolloutItem) -> Option<Self> {
        match item {
            RolloutItem::SessionMeta(line) => Some(Self::SessionMeta {
                thread_id: line.meta.id,
                forked_from_id: line.meta.forked_from_id,
            }),
            RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(settings)) => {
                Some(Self::Settings {
                    thread_id: settings.thread_id,
                })
            }
            _ => None,
        }
    }
}

impl WorkflowOrigins {
    pub(in crate::local) fn new(thread_id: ThreadId) -> Self {
        Self {
            thread_id,
            header_seen: false,
            prefix_open: false,
            expected_source: None,
            source_owners: Vec::new(),
        }
    }

    pub(in crate::local) fn restore(thread_id: ThreadId, json: &str) -> ThreadStoreResult<Self> {
        if json.len() > 8192 {
            return Err(invalid("workflow origin checkpoint exceeds its bound"));
        }
        let state: Self = serde_json::from_str(json).map_err(|error| invalid(error.to_string()))?;
        if state.thread_id != thread_id || state.source_owners.len() > MAX_SOURCE_DEPTH {
            return Err(invalid(
                "workflow origin checkpoint belongs to another thread or exceeds its bound",
            ));
        }
        Ok(state)
    }

    pub(in crate::local) fn observe(
        &mut self,
        boundary: WorkflowSourceBoundary,
    ) -> ThreadStoreResult<()> {
        match boundary {
            WorkflowSourceBoundary::SessionMeta {
                thread_id,
                forked_from_id,
            } if !self.header_seen => {
                if thread_id != self.thread_id {
                    return Err(invalid(
                        "canonical workflow history metadata belongs to another thread",
                    ));
                }
                self.header_seen = true;
                self.expected_source = forked_from_id;
                self.prefix_open = forked_from_id.is_some();
            }
            WorkflowSourceBoundary::SessionMeta {
                thread_id,
                forked_from_id,
            } if self.prefix_open && self.expected_source == Some(thread_id) => {
                if !self.source_owners.contains(&thread_id) {
                    if self.source_owners.len() == MAX_SOURCE_DEPTH {
                        return Err(invalid(
                            "copied workflow history exceeds 64 source ancestors",
                        ));
                    }
                    self.source_owners.push(thread_id);
                }
                self.expected_source = forked_from_id;
            }
            WorkflowSourceBoundary::Settings {
                thread_id: Some(thread_id),
            } if thread_id == self.thread_id => {
                self.prefix_open = false;
                self.expected_source = None;
                self.source_owners.clear();
            }
            WorkflowSourceBoundary::SessionMeta { .. }
            | WorkflowSourceBoundary::Settings { .. } => {}
        }
        Ok(())
    }

    pub(in crate::local) fn authorize(
        &self,
        event: WorkflowLifecycleEvent,
    ) -> ThreadStoreResult<ProvenWorkflowEvent> {
        if event.owner_thread_id != self.thread_id.to_string()
            && !(self.prefix_open
                && self
                    .source_owners
                    .iter()
                    .any(|owner| event.owner_thread_id == owner.to_string()))
        {
            return Err(invalid("workflow event belongs to another thread"));
        }
        Ok(ProvenWorkflowEvent {
            thread_id: self.thread_id,
            event,
        })
    }
}

fn invalid(message: impl Into<String>) -> ThreadStoreError {
    ThreadStoreError::InvalidRequest {
        message: message.into(),
    }
}
