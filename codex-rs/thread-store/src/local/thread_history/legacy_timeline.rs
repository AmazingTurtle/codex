//! Read-only compatibility projection; legacy history retains its existing full-scan cost.
use super::super::LocalThreadStore;
use super::realtime::TimelineCursor;
use super::realtime::entry_key;
use super::thread_history_error;
use crate::ListTimelineParams;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;
use crate::TimelinePage;
use codex_app_server_protocol::ThreadHistoryBuilder;
use codex_app_server_protocol::ThreadTimelineEntry;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::RolloutItem;
use std::collections::BTreeMap;
use std::collections::HashMap;

pub(super) async fn read_if_legacy(
    store: &LocalThreadStore,
    params: &ListTimelineParams,
) -> ThreadStoreResult<Option<TimelinePage>> {
    let Some(resolved) =
        super::super::thread_rollout_resolver::resolve_current(store, params.thread_id).await?
    else {
        return Ok(None);
    };
    let meta = codex_rollout::read_session_meta_line(&resolved.path)
        .await
        .map_err(thread_history_error)?;
    if meta.meta.history_mode != ThreadHistoryMode::Legacy {
        return Ok(None);
    }
    super::segment_paging::validate_page_size(params.page_size)?;
    let cursor = params
        .cursor
        .as_deref()
        .map(serde_json::from_str::<TimelineCursor>)
        .transpose()
        .map_err(|_| ThreadStoreError::InvalidRequest {
            message: "invalid thread timeline cursor".into(),
        })?;
    if cursor
        .as_ref()
        .is_some_and(|cursor| cursor.thread_id != params.thread_id)
    {
        return Err(ThreadStoreError::InvalidRequest {
            message: "thread timeline cursor belongs to another thread".into(),
        });
    }
    let mut reader = codex_rollout::open_rollout_line_reader(&resolved.path)
        .await
        .map_err(thread_history_error)?;
    let mut builder = ThreadHistoryBuilder::new();
    let mut positions = HashMap::<(String, String), u64>::new();
    let mut starts = HashMap::<String, u64>::new();
    let mut ends = HashMap::<String, u64>::new();
    let mut workflow =
        BTreeMap::<String, (u64, codex_app_server_protocol::WorkflowLifecycleEvent)>::new();
    let mut position = 0u64;
    let mut origins = super::workflow_origin::WorkflowOrigins::new(params.thread_id);
    while let Some(raw) = reader.next_line().await.map_err(thread_history_error)? {
        position = position.saturating_add(1);
        let line = codex_rollout::parse_rollout_line(&raw).map_err(thread_history_error)?;
        if let Some(boundary) =
            super::workflow_origin::WorkflowSourceBoundary::from_item(&line.item)
        {
            origins.observe(boundary)?;
        }
        if let RolloutItem::EventMsg(EventMsg::WorkflowLifecycle(event)) = &line.item {
            let event = origins.authorize(event.clone().into())?.into_event();
            if let Some((_, existing)) = workflow.get(&event.id) {
                if existing != &event {
                    return Err(ThreadStoreError::Conflict {
                        message: "workflow event ID has conflicting immutable content".into(),
                    });
                }
            } else {
                workflow.insert(event.id.clone(), (position, event));
            }
        }
        let changes = builder.handle_rollout_item_with_changes(&line.item);
        for turn in changes.removed_turn_ids {
            starts.remove(&turn);
            ends.remove(&turn);
            positions.retain(|(owner, _), _| owner != &turn);
        }
        for change in changes.changed_items {
            positions
                .entry((change.turn_id, change.item.id().to_string()))
                .or_insert(position);
        }
        for turn in changes.changed_turns {
            starts.entry(turn.turn_id.clone()).or_insert(position);
            if turn.status != codex_app_server_protocol::TurnStatus::InProgress {
                ends.insert(turn.turn_id, position);
            }
        }
    }
    let mut rows = Vec::new();
    for turn in builder.finish() {
        let start = starts
            .get(&turn.id)
            .copied()
            .or_else(|| {
                turn.items
                    .iter()
                    .filter_map(|item| positions.get(&(turn.id.clone(), item.id().to_string())))
                    .copied()
                    .min()
            })
            .unwrap_or_default();
        rows.push(ThreadTimelineEntry::TurnStarted {
            position: start,
            turn_id: turn.id.clone(),
            started_at: turn.started_at,
        });
        for item in turn.items {
            if let Some(position) = positions.get(&(turn.id.clone(), item.id().to_string())) {
                rows.push(ThreadTimelineEntry::Item {
                    position: *position,
                    turn_id: turn.id.clone(),
                    item: Box::new(item),
                });
            }
        }
        if let Some(position) = ends.get(&turn.id) {
            rows.push(ThreadTimelineEntry::TurnCompleted {
                position: *position,
                turn_id: turn.id,
                status: turn.status,
                error: turn.error,
                started_at: turn.started_at,
                completed_at: turn.completed_at,
                duration_ms: turn.duration_ms,
            });
        }
    }
    rows.extend(
        workflow
            .into_values()
            .map(|(position, event)| ThreadTimelineEntry::Workflow {
                position,
                event: Box::new(event),
            }),
    );
    rows.sort_by(|left, right| entry_key(left).cmp(&entry_key(right)));
    if let Some(cursor) = cursor {
        rows.retain(|entry| entry_key(entry) < (cursor.position, cursor.kind, cursor.id.as_str()));
    }
    let next_cursor = if rows.len() > params.page_size {
        let first = rows.len() - params.page_size;
        rows.drain(..first);
        let (position, kind, id) = entry_key(&rows[0]);
        Some(
            serde_json::to_string(&TimelineCursor {
                thread_id: params.thread_id,
                position,
                kind,
                id: id.into(),
            })
            .map_err(thread_history_error)?,
        )
    } else {
        None
    };
    Ok(Some(TimelinePage {
        items: rows,
        next_cursor,
        active_realtime_session_at_page_start: None,
    }))
}
