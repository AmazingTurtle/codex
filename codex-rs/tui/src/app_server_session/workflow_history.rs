//! Canonical timeline pages retain workflow events between ordinary turn items.
use super::*;
use codex_app_server_protocol::ThreadTimelineEntry;
use codex_app_server_protocol::ThreadTimelineListParams;
use codex_app_server_protocol::ThreadTimelineListResponse;

#[derive(Clone, Default)]
pub(super) struct TimelineHistory {
    pub(super) entries: Vec<ThreadTimelineEntry>,
    pub(super) cursor: TimelinePageCursor,
    request_id: Option<uuid::Uuid>,
}

#[derive(Clone, Default, PartialEq)]
pub(super) enum TimelinePageCursor {
    #[default]
    Latest,
    Before(String),
    Complete,
}

impl TimelinePageCursor {
    fn as_cursor(&self) -> Option<&str> {
        match self {
            Self::Before(cursor) => Some(cursor),
            Self::Latest | Self::Complete => None,
        }
    }
}

fn timeline_key(entry: &ThreadTimelineEntry) -> (u64, u8, String) {
    match entry {
        ThreadTimelineEntry::Workflow { position, event } => (*position, 4, event.id.clone()),
        ThreadTimelineEntry::Item { position, item, .. } => (*position, 1, item.id().to_string()),
        ThreadTimelineEntry::Realtime { position, item } => (*position, 2, item.id.clone()),
        ThreadTimelineEntry::TurnStarted {
            position, turn_id, ..
        } => (*position, 0, turn_id.clone()),
        ThreadTimelineEntry::TurnCompleted {
            position, turn_id, ..
        } => (*position, 3, turn_id.clone()),
    }
}

impl AppServerSession {
    pub(super) async fn hydrate_timeline_history(
        &mut self,
        thread: &mut Thread,
        budget: &history::HistoryLoadBudget,
        width: u16,
        complete: bool,
    ) -> Result<()> {
        let id = ThreadId::from_string(&thread.id).map_err(color_eyre::eyre::Report::msg)?;
        if self.timeline_unsupported {
            return Ok(());
        }
        let mut remaining: HashSet<_> = thread
            .turns
            .iter()
            .flat_map(|turn| &turn.items)
            .map(|item| item.id().to_string())
            .collect();
        let mut history = TimelineHistory::default();
        let mut seen = HashSet::new();
        let mut rendered = 0;
        let mut scanned = 0;
        loop {
            let limit = match budget.next_page_size(rendered, scanned) {
                Some(limit) => limit,
                // Boundaries and workflow events can exhaust the rendered-row budget
                // before the ordinary items already loaded for this viewport. Cover
                // that anchor without overfetching past it or losing an older cursor.
                None if !remaining.is_empty() && budget.next_page_size(0, scanned).is_some() => 1,
                None => break,
            };
            let request_id = self.next_request_id();
            let response = self
                .client
                .request_typed::<ThreadTimelineListResponse>(ClientRequest::ThreadTimelineList {
                    request_id,
                    params: ThreadTimelineListParams {
                        thread_id: id.to_string(),
                        cursor: history.cursor.as_cursor().map(str::to_owned),
                        limit: Some(limit),
                    },
                })
                .await;
            let page = match response {
                Ok(page) => page,
                Err(TypedRequestError::Server { source, .. })
                    if is_history_pagination_unsupported(&source) =>
                {
                    self.timeline_unsupported = true;
                    self.timeline_history.remove(&id);
                    tracing::warn!(%id, "workflow history is unavailable from this server; using ordinary turn history");
                    return Ok(());
                }
                Err(error) => return Err(error).wrap_err("thread/timeline/list failed"),
            };
            for entry in &page.data {
                if let ThreadTimelineEntry::Item { item, .. } = entry {
                    remaining.remove(item.id());
                }
                rendered += match entry {
                    ThreadTimelineEntry::Workflow { event, .. } => {
                        crate::history_cell::HistoryCell::display_lines(
                            &crate::history_cell::WorkflowLifecycleHistoryCell::new(
                                (**event).clone(),
                            ),
                            width,
                        )
                        .len()
                    }
                    ThreadTimelineEntry::Item { item, .. } => {
                        crate::thread_transcript::thread_items_to_transcript_cells(
                            Some(id),
                            &thread.cwd,
                            [*item.clone()],
                            crate::thread_transcript::RawReasoningVisibility::Hidden,
                            None,
                        )
                        .iter()
                        .map(|cell| cell.display_lines(width).len())
                        .sum()
                    }
                    ThreadTimelineEntry::Realtime { .. }
                    | ThreadTimelineEntry::TurnStarted { .. }
                    | ThreadTimelineEntry::TurnCompleted { .. } => 1,
                };
            }
            scanned += page.data.len();
            history.entries.splice(0..0, page.data);
            history.cursor = page
                .next_cursor
                .map_or(TimelinePageCursor::Complete, TimelinePageCursor::Before);
            let Some(cursor) = history.cursor.as_cursor() else {
                break;
            };
            if !seen.insert(cursor.to_owned()) {
                color_eyre::eyre::bail!("timeline cursor did not advance");
            }
            if !complete && remaining.is_empty() {
                break;
            }
        }
        self.merge_timeline_turn_metadata(id, &history.entries, &mut thread.turns)
            .await?;
        self.timeline_history.insert(id, history);
        Ok(())
    }

    pub(crate) fn timeline_entries(&self, id: ThreadId) -> Option<Vec<ThreadTimelineEntry>> {
        self.timeline_history
            .get(&id)
            .map(|history| history.entries.clone())
    }

    pub(crate) async fn merge_timeline_turn_metadata(
        &mut self,
        id: ThreadId,
        entries: &[ThreadTimelineEntry],
        turns: &mut Vec<Turn>,
    ) -> Result<()> {
        let Some(mut state) = self.history_pagination.get(&id).cloned() else {
            return Ok(());
        };
        let original_cursor = state.next_item_cursor.clone();
        let data = entries
            .iter()
            .rev()
            .filter_map(|entry| match entry {
                ThreadTimelineEntry::Item { turn_id, item, .. } => {
                    Some(codex_app_server_protocol::ThreadItemEntry {
                        turn_id: turn_id.clone(),
                        item: *item.clone(),
                        started_at_ms: None,
                        completed_at_ms: None,
                    })
                }
                _ => None,
            })
            .collect();
        self.merge_thread_item_page(
            id,
            codex_app_server_protocol::ThreadItemsListResponse {
                data,
                next_cursor: None,
                backwards_cursor: None,
            },
            &mut state,
            turns,
        )
        .await?;
        state.next_item_cursor = original_cursor;
        self.history_pagination.insert(id, state);
        Ok(())
    }

    pub(crate) fn timeline_snapshot(
        &self,
        id: ThreadId,
    ) -> Option<crate::session_state::TimelineHistorySnapshot> {
        self.timeline_entries(id)
            .map(crate::session_state::TimelineHistorySnapshot::Available)
            .or_else(|| {
                self.timeline_unsupported
                    .then_some(crate::session_state::TimelineHistorySnapshot::Unavailable)
            })
    }

    pub(crate) fn begin_older_timeline_page(
        &mut self,
        id: ThreadId,
    ) -> Option<(Option<String>, uuid::Uuid)> {
        let history = self.timeline_history.get_mut(&id)?;
        if history.request_id.is_some() || history.cursor == TimelinePageCursor::Complete {
            return None;
        }
        let cursor = history.cursor.as_cursor().map(str::to_owned);
        let request_id = uuid::Uuid::new_v4();
        history.request_id = Some(request_id);
        Some((cursor, request_id))
    }

    pub(crate) fn is_older_timeline_page_pending(
        &self,
        id: ThreadId,
        cursor: Option<&str>,
        request_id: uuid::Uuid,
    ) -> bool {
        self.timeline_history.get(&id).is_some_and(|history| {
            history.request_id == Some(request_id) && history.cursor.as_cursor() == cursor
        })
    }

    pub(crate) fn finish_older_timeline_page(
        &mut self,
        id: ThreadId,
        cursor: Option<&str>,
        request_id: uuid::Uuid,
        page: ThreadTimelineListResponse,
    ) -> Option<Vec<ThreadTimelineEntry>> {
        let history = self.timeline_history.get_mut(&id)?;
        if history.request_id != Some(request_id) || history.cursor.as_cursor() != cursor {
            return None;
        }
        history.request_id = None;
        let known: HashSet<_> = history.entries.iter().map(timeline_key).collect();
        let entries = page
            .data
            .into_iter()
            .filter(|entry| !known.contains(&timeline_key(entry)))
            .collect::<Vec<_>>();
        history.entries.splice(0..0, entries.clone());
        history.cursor = page
            .next_cursor
            .filter(|next| Some(next.as_str()) != cursor)
            .map_or(TimelinePageCursor::Complete, TimelinePageCursor::Before);
        Some(entries)
    }

    pub(crate) fn cancel_older_timeline_page(
        &mut self,
        id: ThreadId,
        cursor: Option<&str>,
        request_id: uuid::Uuid,
    ) {
        if let Some(history) = self.timeline_history.get_mut(&id)
            && history.cursor.as_cursor() == cursor
            && history.request_id == Some(request_id)
        {
            history.request_id = None;
        }
    }
}
