//! Older canonical pages share the existing transcript insertion and viewport bookkeeping.
use super::*;
use codex_app_server_protocol::ThreadTimelineEntry;
use codex_app_server_protocol::ThreadTimelineListParams;
use codex_app_server_protocol::ThreadTimelineListResponse;

impl App {
    pub(super) fn request_older_workflow_timeline(
        &self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) -> bool {
        let Some((cursor, generation)) = app_server.begin_older_timeline_page(thread_id) else {
            return false;
        };
        let request_id = app_server.next_request_id();
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = handle
                .request_typed::<ThreadTimelineListResponse>(ClientRequest::ThreadTimelineList {
                    request_id,
                    params: ThreadTimelineListParams {
                        thread_id: thread_id.to_string(),
                        cursor: cursor.clone(),
                        limit: Some(HISTORY_ITEM_PAGE_LIMIT),
                    },
                })
                .await
                .map_err(|error| error.to_string());
            tx.send(AppEvent::OlderWorkflowTimelineLoaded {
                thread_id,
                cursor,
                request_id: generation,
                result,
            });
        });
        true
    }

    pub(in crate::app) async fn handle_older_workflow_timeline(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        cursor: &Option<String>,
        request_id: uuid::Uuid,
        result: Result<ThreadTimelineListResponse, String>,
    ) -> Result<()> {
        if !app_server.is_older_timeline_page_pending(thread_id, cursor.as_deref(), request_id) {
            return Ok(());
        }
        if self.chat_widget.thread_id() != Some(thread_id) {
            app_server.cancel_older_timeline_page(thread_id, cursor.as_deref(), request_id);
            return Ok(());
        }
        let page = match result {
            Ok(page) => page,
            Err(error) => {
                app_server.cancel_older_timeline_page(thread_id, cursor.as_deref(), request_id);
                self.transcript_view.history = TranscriptHistoryState::Failed;
                return Err(color_eyre::eyre::eyre!(error));
            }
        };
        let Some(channel) = self.thread_event_channels.get(&thread_id) else {
            return Ok(());
        };
        let store = Arc::clone(&channel.store);
        let (cwd, mut turns, delegated) = {
            let store = store.lock().await;
            (
                store
                    .session
                    .as_ref()
                    .map_or_else(|| self.config.cwd.clone(), |session| session.cwd.clone()),
                store.turns.clone(),
                store.delegated_turns.iter().cloned().collect::<Vec<_>>(),
            )
        };
        app_server
            .merge_timeline_turn_metadata(thread_id, &page.data, &mut turns)
            .await?;
        let Some(entries) =
            app_server.finish_older_timeline_page(thread_id, cursor.as_deref(), request_id, page)
        else {
            return Ok(());
        };
        let known_entries = app_server
            .timeline_entries(thread_id)
            .unwrap_or_else(|| entries.clone());
        let adjacency = known_entries.clone();
        let cells = self.workflow_timeline_cells(
            thread_id,
            entries,
            &turns,
            &cwd,
            known_entries,
            &delegated,
        );
        let inserted = self.prepend_older_transcript_cells(cells);
        self.transcript_view
            .history_loaded(&self.transcript_cells, inserted.clone());
        if !inserted.is_empty() {
            self.join_older_workflow_group(inserted.end, &adjacency);
        }
        self.scrollback_has_older_history = app_server.has_older_history(thread_id);
        let mut store = store.lock().await;
        merge_older_turns(&mut store.turns, turns);
        if let Some(session) = &mut store.session {
            session.timeline = app_server.timeline_snapshot(thread_id);
        }
        drop(store);
        if tui.is_owned_screen() {
            self.finish_owned_history_page(tui, app_server, thread_id);
        } else {
            self.finish_inline_history_page(
                tui,
                app_server,
                thread_id,
                tui.terminal.last_known_screen_size.width,
            );
        }
        Ok(())
    }

    fn workflow_timeline_cells(
        &mut self,
        thread_id: ThreadId,
        mut entries: Vec<ThreadTimelineEntry>,
        turns: &[Turn],
        cwd: &AbsolutePathBuf,
        mut known_entries: Vec<ThreadTimelineEntry>,
        delegated: &[String],
    ) -> Vec<Arc<dyn HistoryCell>> {
        let adjacency = known_entries.clone();
        crate::thread_transcript::hide_private_timeline_items(&mut known_entries, turns, delegated);
        let visible: HashSet<_> = known_entries
            .iter()
            .filter_map(|entry| match entry {
                ThreadTimelineEntry::Item { turn_id, item, .. } => {
                    Some((turn_id.clone(), item.id().to_string()))
                }
                _ => None,
            })
            .collect();
        entries.retain(|entry| match entry {
            ThreadTimelineEntry::Item { turn_id, item, .. } => {
                visible.contains(&(turn_id.clone(), item.id().to_string()))
            }
            _ => true,
        });
        let visibility = if self.config.show_raw_agent_reasoning {
            RawReasoningVisibility::Visible
        } else {
            RawReasoningVisibility::Hidden
        };
        let mut cells: Vec<Arc<dyn HistoryCell>> = Vec::new();
        let hidden = hidden_review_item_ids(turns);
        let mut items = Vec::new();
        let mut previous_workflow = false;
        for entry in entries {
            match entry {
                ThreadTimelineEntry::Item { item, .. } => {
                    previous_workflow = false;
                    items.push(*item);
                }
                other => {
                    cells.extend(self.project_older_history_cells(
                        std::mem::take(&mut items),
                        turns,
                        &hidden,
                        thread_id,
                        cwd,
                        visibility,
                    ));
                    match other {
                        ThreadTimelineEntry::Workflow { event, .. } => {
                            if self.chat_widget.remember_workflow_event(&event.id) {
                                let cell = history_cell::WorkflowLifecycleHistoryCell::new(*event);
                                if previous_workflow
                                    && let Some(mut group) = cells.last()
                                        .and_then(|last| last.as_any().downcast_ref::<history_cell::WorkflowLifecycleHistoryCell>())
                                        .cloned()
                                    && group.append_from_timeline(&cell, &adjacency)
                                {
                                    cells.pop();
                                    cells.push(Arc::new(group));
                                } else {
                                    cells.push(Arc::new(cell));
                                }
                            }
                            previous_workflow = true;
                        }
                        ThreadTimelineEntry::Realtime { item, .. } => {
                            previous_workflow = false;
                            if let codex_app_server_protocol::ThreadRealtimeItemContent::TranscriptSegment { text, .. } = item.content { cells.push(Arc::new(history_cell::PlainHistoryCell::new(vec![Line::from(text)]))); }
                        }
                        ThreadTimelineEntry::TurnStarted { .. }
                        | ThreadTimelineEntry::TurnCompleted { .. } => {
                            previous_workflow = false;
                        }
                        ThreadTimelineEntry::Item { .. } => unreachable!(),
                    }
                }
            }
        }
        cells.extend(
            self.project_older_history_cells(items, turns, &hidden, thread_id, cwd, visibility),
        );
        cells
    }
}

#[cfg(test)]
#[path = "workflow_history_pagination_tests.rs"]
mod tests;
