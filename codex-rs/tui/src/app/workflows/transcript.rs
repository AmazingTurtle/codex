//! Scoped worker chat retains stable normal history cells through refresh and older pages.
use super::*;
use crate::workflows::TranscriptGroup;
use crate::workflows::TranscriptItem;
use crate::workflows::TranscriptPage;
use crate::workflows::TranscriptTarget;
#[path = "transcript_live.rs"]
mod live;

impl App {
    pub(in crate::app) fn read_workflow_transcript(
        &mut self,
        app_server: &AppServerSession,
        target: TranscriptTarget,
        older: bool,
    ) {
        let request_id = Uuid::new_v4();
        let key = target.cache_key();
        let cursor = {
            let mut state = self
                .workflows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.attached_thread.or(state.owner) != Some(target.attachment) {
                return;
            }
            if state.transcripts.len() >= 8
                && !state.transcripts.contains_key(&key)
                && let Some(old) = state.transcripts.keys().next().cloned()
            {
                state.transcripts.remove(&old);
            }
            let transcript = state.transcripts.entry(key).or_default();
            if transcript.loading {
                if !older {
                    transcript.refresh_pending = true;
                }
                return;
            }
            if older && transcript.next_cursor.is_none() {
                return;
            }
            transcript.target = Some(target.clone());
            transcript.dirty = false;
            transcript.loading = true;
            transcript.loading_older = older;
            transcript.error = None;
            transcript.request_id = Some(request_id);
            transcript.refresh_cells();
            if older {
                transcript.next_cursor.clone()
            } else {
                None
            }
        };
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = crate::app_server_session::workflows::transcript(&handle, &target, cursor)
                .await
                .map(|response| TranscriptPage {
                    groups: response.data,
                    next_cursor: response.next_cursor,
                    limitation: response.limitation,
                });
            tx.send(AppEvent::Workflow(WorkflowEvent::TranscriptLoaded {
                target,
                request_id,
                result,
            }));
        });
    }

    pub(in crate::app) fn finish_workflow_transcript(
        &mut self,
        target: TranscriptTarget,
        request_id: Uuid,
        result: Result<TranscriptPage, String>,
    ) {
        let mut state = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.attached_thread.or(state.owner) != Some(target.attachment) {
            return;
        }
        let Some(transcript) = state.transcripts.get_mut(&target.cache_key()) else {
            return;
        };
        if transcript.request_id != Some(request_id) {
            return;
        }
        transcript.loading = false;
        match result {
            Ok(page) => {
                if page.groups.iter().any(|group| {
                    group.run_id != target.run_id || group.execution_id != target.execution_id
                }) {
                    transcript.error =
                        Some("Workflow execution changed; refresh the workflow list.".into());
                    transcript.refresh_cells();
                    return;
                }
                let (count, bytes) = {
                    let mut unique = std::collections::HashMap::new();
                    for group in &transcript.groups {
                        for item in &group.items {
                            unique.insert(
                                (
                                    &group.origin.attempt_id,
                                    &group.origin.child_thread_id,
                                    &item.entry.turn_id,
                                    item.entry.item.id(),
                                ),
                                std::borrow::Cow::Borrowed(&item.entry),
                            );
                        }
                    }
                    for group in &page.groups {
                        for item in &group.items {
                            let key = (
                                &group.attempt_id,
                                &group.child_thread_id,
                                &item.turn_id,
                                item.item.id(),
                            );
                            let merged = unique
                                .get(&key)
                                .map(|old| {
                                    std::borrow::Cow::Owned(TranscriptItem::reconcile(
                                        old.as_ref(),
                                        item,
                                    ))
                                })
                                .unwrap_or(std::borrow::Cow::Borrowed(item));
                            unique.insert(key, merged);
                        }
                    }
                    (
                        unique.len(),
                        unique
                            .values()
                            .map(|entry| {
                                serde_json::to_vec(entry).map_or(usize::MAX, |bytes| bytes.len())
                            })
                            .fold(0usize, usize::saturating_add),
                    )
                };
                if count > 1000 || bytes > 16 * 1024 * 1024 {
                    transcript.error = Some("Transcript preview limit reached. Open the worker conversation to read more.".into());
                    transcript.refresh_cells();
                    return;
                }
                transcript.retained_items = count;
                transcript.retained_bytes = bytes;
                let had_items = transcript
                    .groups
                    .iter()
                    .any(|group| !group.items.is_empty());
                let mut order = Vec::new();
                for mut origin in page.groups {
                    let group_key = (origin.attempt_id.clone(), origin.child_thread_id.clone());
                    let group_index = transcript.groups.iter().position(|group| {
                        group.origin.attempt_id == origin.attempt_id
                            && group.origin.child_thread_id == origin.child_thread_id
                    });
                    let mut old = group_index.map(|index| transcript.groups.remove(index));
                    if let Some(previous) = &old {
                        previous.reconcile_origin(&mut origin);
                    }
                    let header = if let Some(group) = &old
                        && group.origin.label == origin.label
                        && group.origin.resolved_model == origin.resolved_model
                        && group.origin.reasoning_effort == origin.reasoning_effort
                    {
                        Arc::clone(&group.header)
                    } else {
                        let mut caption = origin.clone();
                        caption.items.clear();
                        Arc::new(crate::workflows::AgentHeader { origin: caption })
                            as Arc<dyn crate::history_cell::HistoryCell>
                    };
                    let old_final = old.as_ref().map(|group| {
                        (
                            group.origin.final_turn_id.clone(),
                            group.origin.final_item_id.clone(),
                            group.origin.final_result.clone(),
                        )
                    });
                    let projection = old
                        .as_mut()
                        .map(|group| std::mem::take(&mut group.projection))
                        .unwrap_or_default();
                    let outcome = old.as_mut().and_then(|group| group.outcome.take());
                    let mut items = old.take().map_or_else(Vec::new, |group| group.items);
                    let mut item_order = items
                        .iter()
                        .map(|item| (item.entry.turn_id.clone(), item.entry.item.id().to_owned()))
                        .collect::<Vec<_>>();
                    let mut incoming = Vec::new();
                    for snapshot in &origin.items {
                        let reconciled = items
                            .iter()
                            .find(|item| {
                                item.entry.turn_id == snapshot.turn_id
                                    && item.entry.item.id() == snapshot.item.id()
                            })
                            .map(|item| TranscriptItem::reconcile(&item.entry, snapshot))
                            .unwrap_or_else(|| snapshot.clone());
                        let entry = &reconciled;
                        if let Some(index) = items.iter().position(|item| {
                            item.entry.turn_id == entry.turn_id
                                && item.entry.item.id() == entry.item.id()
                        }) {
                            let item = items.remove(index);
                            let final_changed = (origin.final_item_id.as_deref()
                                == Some(entry.item.id())
                                || old_final.as_ref().is_some_and(|(_, id, _)| {
                                    id.as_deref() == Some(entry.item.id())
                                }))
                                && old_final.as_ref()
                                    != Some(&(
                                        origin.final_turn_id.clone(),
                                        origin.final_item_id.clone(),
                                        origin.final_result.clone(),
                                    ));
                            if item.entry == *entry && !final_changed {
                                incoming.push(item);
                                continue;
                            }
                        }
                        incoming.push(TranscriptItem {
                            entry: entry.clone(),
                        });
                    }
                    // Place new snapshot items relative to stable overlapping IDs. This
                    // also inserts a recovered native prompt before its existing reply,
                    // while preserving live items newer than the durable page.
                    let mut insertion = if transcript.loading_older {
                        0
                    } else {
                        item_order.len()
                    };
                    for (index, item) in incoming.iter().enumerate() {
                        let key = (item.entry.turn_id.clone(), item.entry.item.id().to_owned());
                        if let Some(position) = item_order.iter().position(|old| old == &key) {
                            insertion = position + 1;
                            continue;
                        }
                        let next = incoming.iter().skip(index + 1).find_map(|next| {
                            item_order.iter().position(|old| {
                                old.0 == next.entry.turn_id && old.1 == next.entry.item.id()
                            })
                        });
                        let position = next.unwrap_or(insertion.min(item_order.len()));
                        item_order.insert(position, key);
                        insertion = position + 1;
                    }
                    let mut indexed = items
                        .into_iter()
                        .chain(incoming)
                        .map(|item| {
                            (
                                (item.entry.turn_id.clone(), item.entry.item.id().to_owned()),
                                item,
                            )
                        })
                        .collect::<HashMap<_, _>>();
                    let items = item_order
                        .into_iter()
                        .filter_map(|key| indexed.remove(&key))
                        .collect();
                    origin.items.clear();
                    let mut group = TranscriptGroup {
                        origin,
                        header,
                        items,
                        projection,
                        projected: Vec::new(),
                        outcome,
                    };
                    group.project(&self.config, self.local_settings.tui.tool_call_display);
                    transcript.groups.push(group);
                    order.push(group_key);
                }
                if transcript.loading_older {
                    let mut older_groups = Vec::new();
                    for (attempt, child) in order {
                        if let Some(index) = transcript.groups.iter().position(|group| {
                            group.origin.attempt_id == attempt
                                && group.origin.child_thread_id == child
                        }) {
                            older_groups.push(transcript.groups.remove(index));
                        }
                    }
                    older_groups.append(&mut transcript.groups);
                    transcript.groups = older_groups;
                }
                if transcript.loading_older || !had_items || transcript.next_cursor.is_none() {
                    transcript.next_cursor = page.next_cursor;
                }
                transcript.limitation = page.limitation;
                transcript.error = None;
            }
            Err(error) => transcript.error = Some(format!("Could not load workflow chat: {error}")),
        }
        transcript.refresh_cells();
        let pending = std::mem::take(&mut transcript.refresh_pending);
        let visible = state.center.detail_tab == crate::workflows::DetailTab::Transcript
            && !state.center.saved
            && state.selected_transcript_target().as_ref() == Some(&target);
        drop(state);
        if pending && visible && self.chat_widget.active_view_is(crate::workflows::CENTER_ID) {
            self.app_event_tx
                .send(AppEvent::Workflow(WorkflowEvent::ReadTranscript {
                    target,
                    older: false,
                }));
        }
    }
}
