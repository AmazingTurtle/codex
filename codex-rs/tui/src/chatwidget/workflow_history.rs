//! The same stable identity deduplicates live delivery and canonical history replay.
use super::*;

impl ChatWidget {
    pub(crate) fn replay_thread_history(
        &mut self,
        timeline: Option<crate::session_state::TimelineHistorySnapshot>,
        turns: Vec<codex_app_server_protocol::Turn>,
        kind: ReplayKind,
    ) {
        match timeline {
            Some(crate::session_state::TimelineHistorySnapshot::Available(entries)) => {
                self.replay_workflow_timeline(entries, &turns, kind)
            }
            // Old peers keep ordinary conversation history without adding a
            // workflow warning to unrelated chats. Explicit workflow views own
            // their local unavailable/error presentation.
            Some(crate::session_state::TimelineHistorySnapshot::Unavailable) | None => {
                self.replay_thread_turns(turns, kind)
            }
        }
    }

    pub(crate) fn remember_workflow_event(&mut self, id: &str) -> bool {
        self.transcript.workflow_event_ids.insert(id.to_string())
    }
    pub(crate) fn replay_workflow_timeline(
        &mut self,
        mut entries: Vec<codex_app_server_protocol::ThreadTimelineEntry>,
        turns: &[codex_app_server_protocol::Turn],
        kind: ReplayKind,
    ) {
        use codex_app_server_protocol::ThreadTimelineEntry;
        use codex_app_server_protocol::Turn;
        use codex_app_server_protocol::TurnItemsView;
        use codex_app_server_protocol::TurnStatus;
        let adjacency = entries.clone();
        crate::thread_transcript::hide_private_timeline_items(&mut entries, turns, &[]);
        let hidden = turns
            .windows(2)
            .filter(|pair| crate::app_backtrack::is_hidden_nested_review_turn(&pair[0], &pair[1]))
            .map(|pair| pair[1].id.as_str())
            .collect::<std::collections::HashSet<_>>();
        for entry in entries {
            if !matches!(&entry, ThreadTimelineEntry::Workflow { .. }) {
                self.finish_workflow_task_group();
            }
            match entry {
                ThreadTimelineEntry::Workflow { event, .. } => {
                    if !self.transcript.workflow_event_ids.contains(&event.id)
                        && self.transcript.active_cell.as_ref()
                            .and_then(|cell| cell.as_any().downcast_ref::<history_cell::WorkflowLifecycleHistoryCell>())
                            .is_some_and(|group| !group.clone().append_from_timeline(
                                &history_cell::WorkflowLifecycleHistoryCell::new(event.as_ref().clone()),
                                &adjacency,
                            ))
                    {
                        self.flush_active_cell();
                    }
                    self.on_workflow_lifecycle(*event);
                },
                ThreadTimelineEntry::Item { item, turn_id, .. } => {
                    if !hidden.contains(turn_id.as_str()) { self.replay_thread_item(*item, turn_id, kind); }
                }
                ThreadTimelineEntry::TurnStarted { turn_id, started_at, .. } => {
                    self.handle_server_notification(ServerNotification::TurnStarted(codex_app_server_protocol::TurnStartedNotification {
                        thread_id: self.thread_id.map(|id| id.to_string()).unwrap_or_default(),
                        turn: Turn { id: turn_id, root_turn_id: None, items: Vec::new(), items_view: TurnItemsView::NotLoaded,
                            status: TurnStatus::InProgress, error: None, started_at, completed_at: None, duration_ms: None },
                    }), Some(kind));
                }
                ThreadTimelineEntry::TurnCompleted { turn_id, status, error, started_at, completed_at, duration_ms, .. } => {
                    self.handle_server_notification(ServerNotification::TurnCompleted(codex_app_server_protocol::TurnCompletedNotification {
                        thread_id: self.thread_id.map(|id| id.to_string()).unwrap_or_default(),
                        turn: Turn { id: turn_id, root_turn_id: None, items: Vec::new(), items_view: TurnItemsView::NotLoaded,
                            status, error, started_at, completed_at, duration_ms },
                    }), Some(kind));
                }
                ThreadTimelineEntry::Realtime { item, .. } => {
                    if let codex_app_server_protocol::ThreadRealtimeItemContent::TranscriptSegment { text, .. } = item.content {
                        self.add_to_history(history_cell::PlainHistoryCell::new(vec![ratatui::text::Line::from(text)]));
                    }
                }
            }
        }
    }

    /// Raw ordinary items break adjacency even when privacy hides their presentation.
    pub(super) fn finish_workflow_task_group(&mut self) {
        if self.transcript.active_cell.as_ref().is_some_and(|cell| {
            cell.as_any()
                .is::<history_cell::WorkflowLifecycleHistoryCell>()
        }) {
            self.flush_active_cell();
        }
    }

    pub(crate) fn on_workflow_lifecycle(
        &mut self,
        event: codex_app_server_protocol::WorkflowLifecycleEvent,
    ) {
        if self.thread_id.map(|id| id.to_string()).as_deref()
            != Some(event.owner_thread_id.as_str())
            || !self.transcript.workflow_event_ids.insert(event.id.clone())
        {
            return;
        }
        let cell = history_cell::WorkflowLifecycleHistoryCell::new(event);
        if !cell.is_completed_task() || self.has_active_stream_tail() {
            self.add_to_history(cell);
            return;
        }
        if self.transcript.active_cell.as_mut().is_some_and(|active| {
            active
                .as_any_mut()
                .downcast_mut::<history_cell::WorkflowLifecycleHistoryCell>()
                .is_some_and(|group| group.append(&cell))
        }) {
            self.bump_active_cell_revision();
        } else {
            self.flush_active_cell();
            self.transcript.active_cell = Some(Box::new(cell));
            self.bump_active_cell_revision();
        }
        self.request_redraw();
    }

    pub(crate) fn prepend_active_workflow_history(
        &mut self,
        older: &dyn HistoryCell,
        entries: &[codex_app_server_protocol::ThreadTimelineEntry],
    ) -> Option<(u64, u64)> {
        let mut group = older
            .as_any()
            .downcast_ref::<history_cell::WorkflowLifecycleHistoryCell>()?
            .clone();
        let active = self
            .transcript
            .active_cell
            .as_ref()?
            .as_any()
            .downcast_ref::<history_cell::WorkflowLifecycleHistoryCell>()?;
        if !group.append_from_timeline(active, entries) {
            return None;
        }
        let previous_revision = self.transcript.active_cell_revision;
        self.transcript.active_cell = Some(Box::new(group));
        self.bump_active_cell_revision();
        Some((previous_revision, self.transcript.active_cell_revision))
    }
}
