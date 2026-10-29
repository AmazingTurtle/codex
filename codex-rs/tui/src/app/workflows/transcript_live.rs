//! Ordinary worker notifications update only the exact recorded workflow-owned conversations.
use super::*;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadItemEntry;

enum Change<'a> {
    Started(&'a ThreadItem, i64),
    Completed(&'a ThreadItem, i64),
    AgentDelta(&'a str, &'a str),
    CommandDelta(&'a str, &'a str),
    TurnCompleted,
}

impl App {
    pub(in crate::app) fn refresh_workflow_transcript_after_update(
        &mut self,
        app_server: &AppServerSession,
        update: &codex_app_server_protocol::WorkflowUpdatedNotification,
    ) {
        {
            let mut state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for transcript in state.transcripts.values_mut() {
                if transcript.target.as_ref().is_some_and(|target| {
                    target.owner.to_string() == update.thread_id
                        && target.run_id == update.workflow.id
                        && target.execution_id == update.workflow.execution_id
                }) {
                    transcript.dirty = true;
                }
            }
        }
        self.refresh_visible_workflow_transcript(app_server);
    }

    pub(in crate::app) fn refresh_visible_workflow_transcript(
        &mut self,
        app_server: &AppServerSession,
    ) {
        if !self.chat_widget.active_view_is(crate::workflows::CENTER_ID) {
            return;
        }
        let target = {
            let state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.center.saved
                || state.center.detail_tab != crate::workflows::DetailTab::Transcript
            {
                return;
            }
            let Some(target) = state.selected_transcript_target() else {
                return;
            };
            if state
                .transcripts
                .get(&target.cache_key())
                .is_some_and(|transcript| !transcript.dirty)
            {
                return;
            }
            target
        };
        self.read_workflow_transcript(app_server, target, /*older*/ false);
    }

    pub(in crate::app) fn apply_workflow_chat_notification(
        &mut self,
        notification: &ServerNotification,
    ) {
        let (thread, turn, change) = match notification {
            ServerNotification::ItemStarted(item) => (
                item.thread_id.as_str(),
                item.turn_id.as_str(),
                Change::Started(&item.item, item.started_at_ms),
            ),
            ServerNotification::ItemCompleted(item) => (
                item.thread_id.as_str(),
                item.turn_id.as_str(),
                Change::Completed(&item.item, item.completed_at_ms),
            ),
            ServerNotification::AgentMessageDelta(item) => (
                item.thread_id.as_str(),
                item.turn_id.as_str(),
                Change::AgentDelta(&item.item_id, &item.delta),
            ),
            ServerNotification::CommandExecutionOutputDelta(item) => (
                item.thread_id.as_str(),
                item.turn_id.as_str(),
                Change::CommandDelta(&item.item_id, &item.delta),
            ),
            ServerNotification::TurnCompleted(item) => (
                item.thread_id.as_str(),
                item.turn.id.as_str(),
                Change::TurnCompleted,
            ),
            _ => return,
        };
        let mut state = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = state
            .rows
            .iter()
            .map(|row| (row.id.clone(), row.execution_id.clone()))
            .collect::<HashMap<_, _>>();
        for transcript in state.transcripts.values_mut() {
            let Some(target) = &transcript.target else {
                continue;
            };
            if current.get(&target.run_id) != Some(&target.execution_id) {
                continue;
            }
            let mut changed = false;
            for group in &mut transcript.groups {
                if group.origin.child_thread_id != thread
                    || !group.origin.owned_turn_ids.iter().any(|id| id == turn)
                {
                    continue;
                }
                let (id, entry) = match &change {
                    Change::Started(item, at) | Change::Completed(item, at) => {
                        let old = group.items.iter().find(|old| {
                            old.entry.turn_id == turn && old.entry.item.id() == item.id()
                        });
                        if matches!(&change, Change::Started(..)) && old.is_some() {
                            continue;
                        }
                        (
                            item.id(),
                            ThreadItemEntry {
                                turn_id: turn.into(),
                                item: (*item).clone(),
                                started_at_ms: if matches!(&change, Change::Started(..)) {
                                    Some(*at)
                                } else {
                                    old.and_then(|old| old.entry.started_at_ms)
                                },
                                completed_at_ms: if matches!(&change, Change::Completed(..)) {
                                    Some(*at)
                                } else {
                                    None
                                },
                            },
                        )
                    }
                    Change::AgentDelta(id, delta) | Change::CommandDelta(id, delta) => {
                        let Some(old) = group
                            .items
                            .iter()
                            .find(|old| old.entry.turn_id == turn && old.entry.item.id() == *id)
                        else {
                            continue;
                        };
                        if old.entry.completed_at_ms.is_some() {
                            continue;
                        }
                        let mut entry = old.entry.clone();
                        let text = match (&mut entry.item, &change) {
                            (ThreadItem::AgentMessage { text, .. }, Change::AgentDelta(..)) => text,
                            (
                                ThreadItem::CommandExecution {
                                    aggregated_output, ..
                                },
                                Change::CommandDelta(..),
                            ) => aggregated_output.get_or_insert_with(String::new),
                            _ => continue,
                        };
                        if text.len().saturating_add(delta.len()) > 8 * 1024 * 1024 {
                            transcript.error=Some("Live transcript preview limit reached. Open the worker conversation to read more.".into());
                            continue;
                        }
                        text.push_str(delta);
                        (*id, entry)
                    }
                    Change::TurnCompleted => {
                        transcript.dirty = true;
                        continue;
                    }
                };
                let position = group
                    .items
                    .iter()
                    .position(|old| old.entry.turn_id == turn && old.entry.item.id() == id);
                if position.is_some_and(|index| group.items[index].entry == entry) {
                    continue;
                }
                let old_bytes = position
                    .and_then(|index| serde_json::to_vec(&group.items[index].entry).ok())
                    .map_or(0, |bytes| bytes.len());
                let new_bytes = serde_json::to_vec(&entry).map_or(usize::MAX, |bytes| bytes.len());
                let total_bytes = transcript
                    .retained_bytes
                    .saturating_sub(old_bytes)
                    .saturating_add(new_bytes);
                if (position.is_none() && transcript.retained_items >= 1000)
                    || total_bytes > 16 * 1024 * 1024
                {
                    transcript.error=Some("Live transcript preview limit reached. Open the worker conversation to read more.".into());
                    continue;
                }
                let item = TranscriptItem { entry };
                if let Some(index) = position {
                    group.items[index] = item;
                } else {
                    group.items.push(item);
                    transcript.retained_items += 1;
                }
                transcript.retained_bytes = total_bytes;
                group.project(&self.config, self.local_settings.tui.tool_call_display);
                changed = true;
            }
            if changed || transcript.error.is_some() {
                transcript.refresh_cells();
            }
        }
    }
}
