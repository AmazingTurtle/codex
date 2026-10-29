//! Lazily projects native task inputs omitted by older paginated-history writers.
//! Only true visible turn ranges are read; source rollouts and projection checkpoints stay intact.
use super::super::LocalThreadStore;
use super::super::rollout_lineage::RolloutLineage;
use super::thread_history_error;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;
use codex_app_server_protocol::ThreadHistoryChangeSet;
use codex_app_server_protocol::ThreadHistoryItemChange;
use codex_state::SqlitePoolWriteExt;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;

const MAX_TURNS: usize = 32;
const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_INPUTS: usize = 256;

struct NativeInput {
    ordinal: i64,
    start: i64,
    end: i64,
    at_ms: Option<i64>,
    change: ThreadHistoryItemChange,
}

pub(super) async fn backfill(
    store: &LocalThreadStore,
    lineage: &RolloutLineage,
    turn_id: Option<&str>,
) -> ThreadStoreResult<()> {
    let pool = store.thread_history_db().await?;
    let turns = if let Some(turn) = turn_id {
        vec![turn.to_owned()]
    } else {
        let mut turns = Vec::new();
        for segment in lineage.segments().iter().rev() {
            let remaining = MAX_TURNS.saturating_sub(turns.len());
            if remaining == 0 {
                break;
            }
            let rows = sqlx::query_scalar::<_,String>("SELECT turn_id FROM thread_turns WHERE thread_id=? AND first_user_item_id IS NULL AND rollout_ordinal>=? AND (? IS NULL OR rollout_ordinal<?) ORDER BY rollout_ordinal DESC LIMIT ?")
                .bind(segment.rollout_id().to_string())
                .bind(super::sqlite_integer(segment.start_ordinal(),"rollout ordinal")?)
                .bind(segment.end_ordinal().map(|ordinal| super::sqlite_integer(ordinal,"rollout ordinal")).transpose()?)
                .bind(segment.end_ordinal().map(|ordinal| super::sqlite_integer(ordinal,"rollout ordinal")).transpose()?)
                .bind(remaining as i64).fetch_all(pool).await.map_err(thread_history_error)?;
            turns.extend(rows);
        }
        turns
    };
    for turn in turns {
        let row = match super::turn_lookup::find_visible_turn(pool, lineage, &turn).await {
            Ok(row) => row,
            Err(ThreadStoreError::InvalidRequest { .. }) => continue,
            Err(error) => return Err(error),
        };
        if row.first_user_item_id.is_some() {
            continue;
        }
        let Some(start) = row
            .rollout_byte_offset
            .and_then(|offset| u64::try_from(offset).ok())
        else {
            continue;
        };
        let Some(segment) = lineage.segments().iter().find(|segment| {
            segment.rollout_id() == row.rollout_id
                && row.rollout_ordinal >= segment.start_ordinal() as i64
                && segment
                    .end_ordinal()
                    .is_none_or(|end| row.rollout_ordinal < end as i64)
        }) else {
            continue;
        };
        let end = row
            .rollout_end_byte_offset
            .and_then(|offset| u64::try_from(offset).ok())
            .into_iter()
            .chain(segment.end.map(|end| end.end_byte_offset))
            .min();
        let path = segment.rollout_path.clone();
        let start_ordinal = segment.start_ordinal();
        let end_ordinal = segment.end_ordinal();
        let expected_turn = turn.clone();
        let inputs = tokio::task::spawn_blocking(move || {
            read_inputs(
                &path,
                &expected_turn,
                start,
                end,
                start_ordinal,
                end_ordinal,
            )
        })
        .await
        .map_err(thread_history_error)??;
        if inputs.is_empty() {
            continue;
        }
        let mut transaction = pool.begin_immediate().await.map_err(thread_history_error)?;
        for input in inputs {
            super::apply_change_set(
                &mut transaction,
                &row.rollout_id.to_string(),
                input.ordinal,
                input.start,
                input.end,
                input.at_ms,
                ThreadHistoryChangeSet {
                    changed_items: vec![input.change],
                    ..Default::default()
                },
            )
            .await?;
        }
        transaction.commit().await.map_err(thread_history_error)?;
    }
    Ok(())
}

fn read_inputs(
    path: &std::path::Path,
    turn: &str,
    start: u64,
    end: Option<u64>,
    start_ordinal: u64,
    end_ordinal: Option<u64>,
) -> ThreadStoreResult<Vec<NativeInput>> {
    let mut file = codex_rollout::open_rollout_seekable_reader(path, &|| false)
        .map_err(thread_history_error)?;
    let file_end = file.seek(SeekFrom::End(0)).map_err(thread_history_error)?;
    let end = end.unwrap_or(file_end).min(file_end);
    let length = end
        .checked_sub(start)
        .ok_or_else(|| ThreadStoreError::InvalidRequest {
            message: "native input turn range is invalid".into(),
        })?;
    if length > MAX_SOURCE_BYTES {
        return Err(ThreadStoreError::InvalidRequest {
            message: "native input compatibility projection exceeds the 8MiB turn limit".into(),
        });
    }
    file.seek(SeekFrom::Start(start))
        .map_err(thread_history_error)?;
    let mut reader = BufReader::new(file.take(length));
    let mut bytes = Vec::new();
    let mut position = start;
    let mut inputs = Vec::new();
    loop {
        bytes.clear();
        if reader
            .read_until(b'\n', &mut bytes)
            .map_err(thread_history_error)?
            == 0
        {
            break;
        }
        let line_start = position;
        position += bytes.len() as u64;
        if !bytes.ends_with(b"\n") {
            break;
        }
        let Ok(value) = serde_json::from_slice(&bytes) else {
            continue;
        };
        let Ok(line) = codex_rollout::decode_rollout_line(value) else {
            continue;
        };
        let Some(ordinal) = line.ordinal else {
            continue;
        };
        if ordinal < start_ordinal || end_ordinal.is_some_and(|end| ordinal >= end) {
            continue;
        }
        let codex_rollout::RolloutItem::ResponseItem(item) = &line.item else {
            continue;
        };
        let Some(change) = codex_app_server_protocol::project_native_agent_input(&item.item) else {
            continue;
        };
        if change.turn_id != turn {
            continue;
        }
        if inputs.len() >= MAX_INPUTS {
            return Err(ThreadStoreError::InvalidRequest {
                message: "native input compatibility projection exceeds the 256-input turn limit"
                    .into(),
            });
        }
        inputs.push(NativeInput {
            ordinal: super::sqlite_integer(ordinal, "rollout ordinal")?,
            start: super::sqlite_integer(line_start, "rollout byte offset")?,
            end: super::sqlite_integer(position, "rollout byte offset")?,
            at_ms: chrono::DateTime::parse_from_rfc3339(&line.timestamp)
                .ok()
                .map(|time| time.timestamp_millis()),
            change,
        });
    }
    Ok(inputs)
}
