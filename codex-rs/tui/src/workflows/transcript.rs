//! Stable scoped targets and retained normal history cells for workflow chat pages.
use super::*;
use crate::legacy_core::config::Config;
use codex_app_server_protocol::ThreadItemEntry;
use codex_app_server_protocol::WorkflowTranscriptGroup;
use codex_app_server_protocol::WorkflowTranscriptTarget;
#[path = "transcript_merge.rs"]
mod merge;
#[path = "transcript_projection.rs"]
mod projection;
pub(crate) use projection::AgentHeader;
pub(crate) use projection::OrderedProjection;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TranscriptTarget {
    pub(crate) attachment: ThreadId,
    pub(crate) owner: ThreadId,
    pub(crate) run_id: String,
    pub(crate) execution_id: String,
    pub(crate) target: WorkflowTranscriptTarget,
}

impl TranscriptTarget {
    pub(crate) fn cache_key(&self) -> String {
        serde_json::json!([
            self.attachment.to_string(),
            self.owner.to_string(),
            self.run_id,
            self.execution_id,
            self.target
        ])
        .to_string()
    }
}

#[derive(Debug)]
pub(crate) struct TranscriptPage {
    pub(crate) groups: Vec<WorkflowTranscriptGroup>,
    pub(crate) next_cursor: Option<String>,
    pub(crate) limitation: Option<String>,
}

#[derive(Debug)]
pub(crate) struct TranscriptItem {
    pub(crate) entry: ThreadItemEntry,
}
impl TranscriptItem {
    pub(crate) fn reconcile(old: &ThreadItemEntry, new: &ThreadItemEntry) -> ThreadItemEntry {
        merge::entry(old, new)
    }
}

#[derive(Debug)]
pub(crate) struct TranscriptGroup {
    pub(crate) origin: WorkflowTranscriptGroup,
    pub(crate) header: Arc<dyn crate::history_cell::HistoryCell>,
    pub(crate) items: Vec<TranscriptItem>,
    pub(crate) projection: OrderedProjection,
    pub(crate) projected: Vec<Arc<dyn crate::history_cell::HistoryCell>>,
    pub(crate) outcome: Option<(serde_json::Value, Arc<dyn crate::history_cell::HistoryCell>)>,
}

impl TranscriptGroup {
    pub(crate) fn reconcile_origin(&self, new: &mut WorkflowTranscriptGroup) {
        merge::origin(&self.origin, new);
    }
    pub(crate) fn project(
        &mut self,
        config: &Config,
        display: codex_config::types::ToolCallDisplay,
    ) {
        let mut entries = self
            .items
            .iter()
            .map(|item| std::borrow::Cow::Borrowed(&item.entry))
            .collect::<Vec<_>>();
        for source in &mut entries {
            if self.origin.final_turn_id.as_ref() == Some(&source.turn_id)
                && self.origin.final_item_id.as_deref() == Some(source.item.id())
            {
                let entry = source.to_mut();
                if let codex_app_server_protocol::ThreadItem::AgentMessage { id, workflow, .. } =
                    &mut entry.item
                    && let Some(codex_app_server_protocol::WorkflowTranscriptResult::Value {
                        value,
                    }) = &self.origin.final_result
                {
                    *workflow = Some(codex_app_server_protocol::WorkflowAgentMessageProvenance {
                        run_id: self.origin.run_id.clone(),
                        execution_id: self.origin.execution_id.clone(),
                        attempt_id: self.origin.attempt_id.clone(),
                        child_thread_id: self.origin.child_thread_id.clone(),
                        turn_id: entry.turn_id.clone(),
                        item_id: id.clone(),
                        format: if value.is_string() {
                            codex_app_server_protocol::WorkflowAgentMessageFormat::Text
                        } else {
                            codex_app_server_protocol::WorkflowAgentMessageFormat::Json
                        },
                    });
                }
            }
        }
        self.projected = self.projection.update(
            &self.origin,
            &entries
                .iter()
                .map(std::borrow::Cow::as_ref)
                .collect::<Vec<_>>(),
            config,
            crate::thread_transcript::ProjectionOptions {
                tool_call_display: display,
                source: if self.origin.final_turn_id.is_some() {
                    crate::thread_transcript::ProjectionSource::History
                } else {
                    crate::thread_transcript::ProjectionSource::Live
                },
            },
        );
        let value = self
            .origin
            .final_result
            .as_ref()
            .map(|result| match result {
                codex_app_server_protocol::WorkflowTranscriptResult::Value { value } => value,
            });
        if self.outcome.as_ref().map(|(value, _)| value) != value {
            self.outcome = value.map(|value| {
                (
                    value.clone(),
                    Arc::new(projection::OutcomeCell {
                        value: value.clone(),
                        original: value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default()),
                    }) as Arc<dyn crate::history_cell::HistoryCell>,
                )
            });
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct WorkflowTranscript {
    pub(crate) target: Option<TranscriptTarget>,
    pub(crate) dirty: bool,
    pub(crate) groups: Vec<TranscriptGroup>,
    pub(crate) cells: Vec<Arc<dyn crate::history_cell::HistoryCell>>,
    pub(crate) next_cursor: Option<String>,
    pub(crate) limitation: Option<String>,
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    pub(crate) request_id: Option<uuid::Uuid>,
    pub(crate) loading_older: bool,
    pub(crate) refresh_pending: bool,
    pub(crate) content_revision: u64,
    pub(crate) retained_items: usize,
    pub(crate) retained_bytes: usize,
    pub(crate) status_cell: Option<(String, Arc<dyn crate::history_cell::HistoryCell>)>,
    pub(crate) cell_revisions: Vec<Option<u64>>,
}

impl WorkflowTranscript {
    pub(crate) fn refresh_cells(&mut self) {
        use ratatui::style::Stylize;
        let status = self.error.as_ref().or(self.limitation.as_ref());
        if self.status_cell.as_ref().map(|(text, _)| text) != status {
            self.status_cell = status.map(|text| {
                (
                    text.clone(),
                    Arc::new(crate::history_cell::PlainHistoryCell::new(vec![
                        text.clone().dim().into(),
                    ])) as Arc<dyn crate::history_cell::HistoryCell>,
                )
            });
        }
        let cells: Vec<_> = self
            .groups
            .iter()
            .flat_map(|group| {
                std::iter::once(Arc::clone(&group.header))
                    .chain(group.projected.iter().cloned())
                    .chain(group.outcome.iter().map(|(_, cell)| Arc::clone(cell)))
            })
            .chain(self.status_cell.iter().map(|(_, cell)| Arc::clone(cell)))
            .collect();
        let revisions = cells
            .iter()
            .map(|cell| cell.transcript_animation_tick())
            .collect::<Vec<_>>();
        if revisions != self.cell_revisions
            || cells.len() != self.cells.len()
            || cells
                .iter()
                .zip(&self.cells)
                .any(|(next, old)| !Arc::ptr_eq(next, old))
        {
            self.content_revision = self.content_revision.wrapping_add(1);
            self.cells = cells;
            self.cell_revisions = revisions;
        }
    }
}

impl WorkflowState {
    pub(crate) fn selected_transcript_cells(
        &self,
    ) -> Option<&[Arc<dyn crate::history_cell::HistoryCell>]> {
        let target = self.selected_transcript_target()?;
        self.transcripts
            .get(&target.cache_key())
            .filter(|transcript| !transcript.cells.is_empty())
            .map(|transcript| transcript.cells.as_slice())
    }

    pub(crate) fn selected_transcript_revision(&self) -> Option<u64> {
        let target = self.selected_transcript_target()?;
        self.transcripts
            .get(&target.cache_key())
            .map(|transcript| transcript.content_revision)
    }
}
