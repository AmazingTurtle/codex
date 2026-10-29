//! Bounded current-turn native items supplement the persisted-history reducer for live inspection.
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadItemEntry;
use codex_app_server_protocol::Turn;
use codex_protocol::protocol::EventMsg;

const MAX_ITEMS: usize = 1000;
const MAX_BYTES: usize = 16 * 1024 * 1024;
#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemStage {
    Started,
    Completed,
}
enum DeltaTarget {
    Agent,
    Command,
}
struct LiveItem {
    item: ThreadItem,
    stage: ItemStage,
    started_at_ms: Option<i64>,
    completed_at_ms: Option<i64>,
}
pub(crate) struct Snapshot {
    pub(crate) turn_id: String,
    pub(crate) items: Vec<ThreadItemEntry>,
}

#[derive(Default)]
pub(super) struct ActiveItems {
    turn_id: Option<String>,
    items: Vec<LiveItem>,
    bytes: usize,
    incomplete: bool,
}

impl ActiveItems {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(super) fn observe(&mut self, turn: &str, event: &EventMsg) {
        if let EventMsg::TurnStarted(start) = event {
            if start.turn_id == turn {
                self.reset();
                self.turn_id = Some(start.turn_id.clone());
            }
            return;
        }
        if self.turn_id.as_deref() != Some(turn) {
            return;
        }
        match event {
            EventMsg::ItemStarted(start) if start.turn_id == turn => {
                let item = ThreadItem::from(start.item.clone());
                if !self.items.iter().any(|old| old.item.id() == item.id()) {
                    self.upsert(
                        item,
                        ItemStage::Started,
                        (start.started_at_ms != 0).then_some(start.started_at_ms),
                        /*completed_at_ms*/ None,
                    );
                }
            }
            EventMsg::ItemCompleted(done) if done.turn_id == turn => self.upsert(
                ThreadItem::from(done.item.clone()),
                ItemStage::Completed,
                done.started_at_ms,
                (done.completed_at_ms != 0).then_some(done.completed_at_ms),
            ),
            EventMsg::AgentMessageContentDelta(delta) if delta.turn_id == turn => {
                self.append(&delta.item_id, &delta.delta, DeltaTarget::Agent)
            }
            EventMsg::ExecCommandOutputDelta(delta) => self.append(
                &delta.call_id,
                &String::from_utf8_lossy(&delta.chunk),
                DeltaTarget::Command,
            ),
            EventMsg::TurnComplete(done) if done.turn_id == turn => self.reset(),
            EventMsg::TurnAborted(done) if done.turn_id.as_deref().is_none_or(|id| id == turn) => {
                self.reset()
            }
            _ => {}
        }
    }

    fn upsert(
        &mut self,
        item: ThreadItem,
        completed: ItemStage,
        started_at_ms: Option<i64>,
        completed_at_ms: Option<i64>,
    ) {
        let existing = self.items.iter().position(|old| old.item.id() == item.id());
        if existing.is_some_and(|index| self.items[index].stage == ItemStage::Completed) {
            return;
        }
        let size = serde_json::to_vec(&item).map_or(usize::MAX, |bytes| bytes.len());
        let previous = existing
            .and_then(|index| serde_json::to_vec(&self.items[index].item).ok())
            .map_or(0, |bytes| bytes.len());
        let bytes = self.bytes.saturating_sub(previous).saturating_add(size);
        if (existing.is_none() && self.items.len() >= MAX_ITEMS) || bytes > MAX_BYTES {
            self.incomplete = true;
            return;
        }
        self.bytes = bytes;
        let started_at_ms =
            started_at_ms.or_else(|| existing.and_then(|index| self.items[index].started_at_ms));
        let item = LiveItem {
            item,
            stage: completed,
            started_at_ms,
            completed_at_ms,
        };
        if let Some(index) = existing {
            self.items[index] = item;
        } else {
            self.items.push(item);
        }
    }

    fn append(&mut self, id: &str, delta: &str, target: DeltaTarget) {
        let Some(entry) = self.items.iter_mut().find(|entry| entry.item.id() == id) else {
            return;
        };
        if entry.stage == ItemStage::Completed {
            return;
        }
        let size =
            serde_json::to_vec(delta).map_or(usize::MAX, |bytes| bytes.len().saturating_sub(2));
        if self.bytes.saturating_add(size) > MAX_BYTES {
            self.incomplete = true;
            return;
        }
        let text = match &mut entry.item {
            ThreadItem::AgentMessage { text, .. } if matches!(target, DeltaTarget::Agent) => text,
            ThreadItem::CommandExecution {
                aggregated_output, ..
            } if matches!(target, DeltaTarget::Command) => {
                aggregated_output.get_or_insert_with(String::new)
            }
            _ => return,
        };
        text.push_str(delta);
        self.bytes = self.bytes.saturating_add(size);
    }

    pub(super) fn merge(&self, mut turn: Turn) -> Result<Turn, String> {
        if self.turn_id.as_deref() != Some(turn.id.as_str()) {
            return Ok(turn);
        }
        if self.incomplete {
            return Err("The current native conversation exceeds the bounded live snapshot; its durable history remains available.".into());
        }
        // Modern agent lifecycle items carry true native IDs. The legacy message
        // events used by the history reducer synthesize IDs and must not create
        // a second copy of those same canonical messages in a live snapshot.
        let canonical_agents = self
            .items
            .iter()
            .any(|entry| matches!(&entry.item, ThreadItem::AgentMessage { .. }));
        let canonical_users = self
            .items
            .iter()
            .any(|entry| matches!(&entry.item, ThreadItem::UserMessage { .. }));
        turn.items.retain(|item| {
            !((canonical_agents && matches!(item, ThreadItem::AgentMessage { .. }))
                || (canonical_users && matches!(item, ThreadItem::UserMessage { .. })))
        });
        for (offset, entry) in self.items.iter().enumerate() {
            let item = &entry.item;
            if let Some(index) = turn.items.iter().position(|old| old.id() == item.id()) {
                if entry.stage == ItemStage::Completed
                    || matches!(
                        item,
                        ThreadItem::CommandExecution {
                            status: codex_app_server_protocol::CommandExecutionStatus::InProgress,
                            ..
                        }
                    )
                {
                    turn.items[index] = item.clone();
                }
            } else {
                let next = self.items.iter().skip(offset + 1).find_map(|next| {
                    turn.items
                        .iter()
                        .position(|known| known.id() == next.item.id())
                });
                turn.items
                    .insert(next.unwrap_or(turn.items.len()), item.clone());
            }
        }
        if turn.items.len() > MAX_ITEMS
            || serde_json::to_vec(&turn.items).map_or(true, |bytes| bytes.len() > MAX_BYTES)
        {
            return Err("The current native conversation exceeds the bounded live snapshot; its durable history remains available.".into());
        }
        Ok(turn)
    }

    pub(super) fn snapshot(&self, turn: Turn) -> Result<Snapshot, String> {
        let turn = self.merge(turn)?;
        let items = turn
            .items
            .into_iter()
            .map(|item| {
                let known = self.items.iter().find(|entry| entry.item.id() == item.id());
                ThreadItemEntry {
                    turn_id: turn.id.clone(),
                    item,
                    started_at_ms: known.and_then(|entry| entry.started_at_ms),
                    completed_at_ms: known.and_then(|entry| entry.completed_at_ms),
                }
            })
            .collect();
        Ok(Snapshot {
            turn_id: turn.id,
            items,
        })
    }
}

#[cfg(test)]
#[path = "thread_state_live_tests.rs"]
mod tests;
