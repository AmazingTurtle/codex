//! Read-only raw-history receipts; compaction does not remove earlier delivery identities.

use std::collections::BTreeSet;
use std::io;

use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::ReverseJsonlScanner;
use codex_rollout::RolloutItem;
use codex_rollout::ScanOutcome;

use super::LocalThreadStore;
use super::rollout_lineage::RolloutLineage;
use super::rollout_lineage::RolloutLineageSegment;
use crate::FindPersistedInputIdsParams;
use crate::PersistedInputId;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;
use crate::persisted_input::PersistedInputSearch;

#[cfg(test)]
#[path = "persisted_input_tests.rs"]
mod tests;

pub(super) async fn find(
    store: &LocalThreadStore,
    params: FindPersistedInputIdsParams,
) -> ThreadStoreResult<BTreeSet<PersistedInputId>> {
    let search = PersistedInputSearch::new(params.ids)?;
    if search.is_complete() {
        return Ok(search.finish());
    }
    // A known newly created writer has never committed any history. Do not
    // mistake a missing or unreadable previously persisted rollout for this case.
    if super::live_writer::unpersisted_session_meta(store, params.thread_id)
        .await
        .is_some()
    {
        return Ok(search.finish());
    }
    let resolved =
        super::thread_rollout_resolver::resolve_current_including_archived(store, params.thread_id)
            .await?
            .ok_or(ThreadStoreError::ThreadNotFound {
                thread_id: params.thread_id,
            })?;
    let meta = codex_rollout::read_session_meta_line(&resolved.path)
        .await
        .map_err(|error| ThreadStoreError::Internal {
            message: format!("failed to read persisted-input metadata: {error}"),
        })?;
    if meta.meta.id != params.thread_id {
        return Err(ThreadStoreError::InvalidRequest {
            message: "persisted-input rollout belongs to another thread".into(),
        });
    }
    let lineage = match meta.meta.history_mode {
        ThreadHistoryMode::Paginated => {
            store
                .resolve_rollout_lineage(params.thread_id, Some(resolved.path))
                .await?
        }
        ThreadHistoryMode::Legacy => RolloutLineage {
            segments: vec![RolloutLineageSegment {
                rollout_id: resolved.rollout_id,
                owner_thread_id: params.thread_id,
                rollout_path: resolved.path,
                start_ordinal: 1,
                end: None,
            }],
        },
    };
    let (stop, keep_running) = tokio::sync::oneshot::channel::<()>();
    let result = tokio::task::spawn_blocking(move || scan(&lineage, search, || stop.is_closed()))
        .await
        .map_err(|error| ThreadStoreError::Internal {
            message: format!("failed to join persisted-input scan: {error}"),
        })?;
    drop(keep_running);
    result.map_err(|error| ThreadStoreError::Internal {
        message: format!("failed to scan persisted input identities: {error}"),
    })
}

fn scan(
    lineage: &RolloutLineage,
    mut search: PersistedInputSearch,
    is_cancelled: impl Fn() -> bool,
) -> io::Result<BTreeSet<PersistedInputId>> {
    for segment in lineage.segments().iter().rev() {
        let file = codex_rollout::open_rollout_seekable_reader(
            segment.rollout_path.as_path(),
            &is_cancelled,
        )?;
        let mut scanner = match segment.end.map(|end| end.end_byte_offset) {
            Some(end) => ReverseJsonlScanner::new_at(file, end)?,
            None => ReverseJsonlScanner::new(file)?,
        };
        while let Some(outcome) = scanner.scan_next_rollout_line()? {
            if is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "persisted-input lookup cancelled",
                ));
            }
            let line = match outcome {
                ScanOutcome::Parsed(line) => line,
                ScanOutcome::Rejected(error) => {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, error));
                }
            };
            // The inherited prefix is scanned through its own frozen lineage segment.
            if matches!(line.item, RolloutItem::SessionMeta(_)) {
                break;
            }
            search.observe(&line.item);
            if search.is_complete() {
                return Ok(search.finish());
            }
        }
    }
    Ok(search.finish())
}
