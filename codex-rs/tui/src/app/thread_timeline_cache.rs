//! Refresh canonical history before reusing a warm thread's bounded replay buffer.

use super::*;
use crate::session_state::TimelineHistorySnapshot;
use codex_app_server_protocol::ThreadTimelineEntry;

impl ThreadEventStore {
    pub(super) fn note_timeline_notification(&mut self, notification: &ServerNotification) {
        if matches!(
            notification,
            ServerNotification::TurnStarted(_)
                | ServerNotification::TurnCompleted(_)
                | ServerNotification::ItemStarted(_)
                | ServerNotification::ItemCompleted(_)
                | ServerNotification::AgentMessageDelta(_)
                | ServerNotification::PlanDelta(_)
                | ServerNotification::ReasoningSummaryTextDelta(_)
                | ServerNotification::ReasoningTextDelta(_)
                | ServerNotification::ReasoningSummaryPartAdded(_)
                | ServerNotification::CommandExecutionOutputDelta(_)
                | ServerNotification::FileChangeOutputDelta(_)
                | ServerNotification::WorkflowLifecycleChanged(_)
                | ServerNotification::ThreadRealtimeItemAdded(_)
                | ServerNotification::ThreadReverted(_)
        ) {
            self.timeline_revision = self.timeline_revision.wrapping_add(1);
        }
    }

    fn dirty_timeline_revision(&self) -> Option<u64> {
        (self.timeline_revision != self.timeline_loaded_revision
            && matches!(
                self.session
                    .as_ref()
                    .and_then(|session| session.timeline.as_ref()),
                Some(TimelineHistorySnapshot::Available(_))
            ))
        .then_some(self.timeline_revision)
    }

    /// A notification can already be in the RPC snapshot while waiting in the transport queue.
    /// Match stable identities and completed contents rather than guessing its rollout position.
    pub(super) fn timeline_notification_covered(&self, notification: &ServerNotification) -> bool {
        let Some(TimelineHistorySnapshot::Available(entries)) = self
            .session
            .as_ref()
            .and_then(|session| session.timeline.as_ref())
        else {
            return false;
        };
        let completed_turn = |id: &str| {
            entries.iter().any(|entry|
            matches!(entry, ThreadTimelineEntry::TurnCompleted { turn_id, .. } if turn_id == id))
        };
        match notification {
            ServerNotification::WorkflowLifecycleChanged(n) => entries.iter().any(|entry|
                matches!(entry, ThreadTimelineEntry::Workflow { event, .. } if event.id == n.event.id)),
            ServerNotification::TurnStarted(n) => completed_turn(&n.turn.id)
                || (self.active_turn_id.as_ref() == Some(&n.turn.id) && entries.iter().any(|entry|
                    matches!(entry, ThreadTimelineEntry::TurnStarted { turn_id, .. } if turn_id == &n.turn.id))),
            ServerNotification::TurnCompleted(n) => entries.iter().any(|entry|
                matches!(entry, ThreadTimelineEntry::TurnCompleted { turn_id, status, error, .. }
                    if turn_id == &n.turn.id && status == &n.turn.status && error == &n.turn.error)),
            ServerNotification::ItemStarted(n) => entries.iter().any(|entry|
                matches!(entry, ThreadTimelineEntry::Item { turn_id, item, .. }
                    if turn_id == &n.turn_id && item.id() == n.item.id())),
            ServerNotification::ItemCompleted(n) => {
                // Questions are live interactive state even when their accompanying text was saved.
                !matches!(&n.item, ThreadItem::AgentMessage { questions: Some(_), .. })
                    && entries.iter().any(|entry|
                        matches!(entry, ThreadTimelineEntry::Item { turn_id, item, .. }
                            if turn_id == &n.turn_id && item.as_ref() == &n.item))
            }
            ServerNotification::AgentMessageDelta(n) => completed_turn(&n.turn_id),
            ServerNotification::PlanDelta(n) => completed_turn(&n.turn_id),
            ServerNotification::ReasoningSummaryTextDelta(n) => completed_turn(&n.turn_id),
            ServerNotification::ReasoningTextDelta(n) => completed_turn(&n.turn_id),
            ServerNotification::ReasoningSummaryPartAdded(n) => completed_turn(&n.turn_id),
            ServerNotification::CommandExecutionOutputDelta(n) => completed_turn(&n.turn_id),
            ServerNotification::FileChangeOutputDelta(n) => completed_turn(&n.turn_id),
            _ => false,
        }
    }

    fn rebase_canonical_buffer(&mut self, entries: &[ThreadTimelineEntry]) {
        let completed_turns: HashSet<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                ThreadTimelineEntry::TurnCompleted { turn_id, .. } => Some(turn_id.as_str()),
                _ => None,
            })
            .collect();
        // Only a matching buffered completion proves that earlier streaming deltas
        // were incorporated. An ID alone must not discard newer partial output.
        let completed_items: HashSet<_> = self
            .buffer
            .iter()
            .filter_map(|event| {
                let ThreadBufferedEvent::Notification(notification) = event else {
                    return None;
                };
                let ServerNotification::ItemCompleted(n) = notification.as_ref() else {
                    return None;
                };
                entries
                    .iter()
                    .any(|entry| {
                        matches!(entry,
                ThreadTimelineEntry::Item { turn_id, item, .. }
                    if turn_id == &n.turn_id && item.as_ref() == &n.item)
                    })
                    .then(|| (n.turn_id.clone(), n.item.id().to_string()))
            })
            .collect();
        let active_turn = self.active_turn_id.clone();
        self.buffer.retain_mut(|event| {
            if let ThreadBufferedEvent::Notification(notification) = event {
                if let ServerNotification::ItemStarted(n) = notification.as_ref()
                    && entries.iter().any(|entry| {
                        matches!(entry,
                        ThreadTimelineEntry::Item { turn_id, item, .. }
                            if turn_id == &n.turn_id && item.id() == n.item.id())
                    })
                {
                    return false;
                }
                let live_item = match notification.as_ref() {
                    ServerNotification::ItemStarted(n) => Some((&n.turn_id, n.item.id())),
                    ServerNotification::ItemCompleted(n) => Some((&n.turn_id, n.item.id())),
                    ServerNotification::AgentMessageDelta(n) => {
                        Some((&n.turn_id, n.item_id.as_str()))
                    }
                    ServerNotification::PlanDelta(n) => Some((&n.turn_id, n.item_id.as_str())),
                    ServerNotification::ReasoningSummaryTextDelta(n) => {
                        Some((&n.turn_id, n.item_id.as_str()))
                    }
                    ServerNotification::ReasoningTextDelta(n) => {
                        Some((&n.turn_id, n.item_id.as_str()))
                    }
                    ServerNotification::ReasoningSummaryPartAdded(n) => {
                        Some((&n.turn_id, n.item_id.as_str()))
                    }
                    ServerNotification::CommandExecutionOutputDelta(n) => {
                        Some((&n.turn_id, n.item_id.as_str()))
                    }
                    ServerNotification::FileChangeOutputDelta(n) => {
                        Some((&n.turn_id, n.item_id.as_str()))
                    }
                    _ => None,
                };
                if live_item.is_some_and(|(turn, _)| completed_turns.contains(turn.as_str())) {
                    return Self::event_survives_session_refresh(event);
                }
                if live_item.is_some_and(|(turn, item)| {
                    active_turn.as_ref() == Some(turn)
                        && !completed_items.contains(&(turn.clone(), item.to_string()))
                }) {
                    return true;
                }
                // Canonical pagination owns all durable lifecycle facts, including older pages.
                if matches!(
                    notification.as_ref(),
                    ServerNotification::WorkflowLifecycleChanged(_)
                ) {
                    return false;
                }
            }
            Self::event_survives_session_refresh(event)
        });
        self.buffered_agent_message_delta_bytes = self
            .buffer
            .iter()
            .filter_map(|event| match event {
                ThreadBufferedEvent::Notification(n) => match n.as_ref() {
                    ServerNotification::AgentMessageDelta(delta) => Some(delta.delta.len()),
                    _ => None,
                },
                _ => None,
            })
            .sum();
    }
}

impl App {
    pub(super) async fn refresh_cached_thread_timeline(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) -> Result<()> {
        let Some(channel) = self.thread_event_channels.get(&thread_id) else {
            return Ok(());
        };
        let store = Arc::clone(&channel.store);
        let attachment = channel.attachment();
        let Some(revision) = store.lock().await.dirty_timeline_revision() else {
            return Ok(());
        };
        let displayed = self.current_displayed_thread_id();
        let backend = app_server.backend_id();
        let mut thread = app_server
            .thread_read(thread_id, /*include_turns*/ false)
            .await?;
        app_server
            .hydrate_initial_thread_history(
                &mut thread,
                /*turn_cursor*/ None,
                /*item_cursor*/ None,
                Some(&self.config),
                Some(&self.local_settings),
                crate::app_server_session::HistoryHydrationScope::Initial,
            )
            .await?;
        let still_attached = self
            .thread_event_channels
            .get(&thread_id)
            .is_some_and(|channel| {
                Arc::ptr_eq(&channel.store, &store) && channel.attachment() == attachment
            });
        let mut store = store.lock().await;
        if !still_attached
            || self.current_displayed_thread_id() != displayed
            || app_server.backend_id() != backend
            || store.timeline_revision != revision
        {
            color_eyre::eyre::bail!(
                "Conversation changed while refreshing history; please select it again."
            );
        }
        let timeline = app_server.timeline_snapshot(thread_id);
        if let Some(TimelineHistorySnapshot::Available(entries)) = &timeline {
            store.rebase_canonical_buffer(entries);
        }
        store.set_turns(thread.turns.clone());
        if let Some(session) = store.session.as_mut() {
            session.timeline = timeline.clone();
        }
        store.timeline_loaded_revision = revision;
        // Untouched roots use this lightweight cache instead of thread/resume.
        if let Some(blank) = self.agents_overview.blank_sessions.get_mut(&thread_id) {
            blank.session.timeline = timeline;
            blank.turns = thread.turns;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "thread_timeline_cache_tests.rs"]
mod tests;
