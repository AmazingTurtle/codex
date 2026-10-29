//! Workflow details use the ordinary history viewport's logical reading anchors.
use super::*;
use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::transcript_view::TranscriptView;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

#[path = "detail_document.rs"]
mod document;
use document::DetailDocument;

#[cfg(test)]
#[path = "detail_chat_tests.rs"]
mod chat_tests;

#[derive(Default)]
pub(crate) struct DetailViewport {
    view: TranscriptView,
    document: Option<Arc<DetailDocument>>,
    cells: Vec<Arc<dyn HistoryCell>>,
    initialized: bool,
    document_key: Option<u64>,
    transcript_revision: Option<(String, u64)>,
    boundary: Option<bool>,
    size: (u16, u16),
}

impl DetailViewport {
    pub(super) fn render_document(
        &mut self,
        snapshot: WorkflowState,
        area: Rect,
        buffer: &mut Buffer,
    ) {
        self.render_state(&snapshot, area, buffer);
    }

    pub(super) fn render_state(&mut self, state: &WorkflowState, area: Rect, buffer: &mut Buffer) {
        let key = document::content_key(state);
        if self.document_key != Some(key) || self.transcript_revision.is_some() {
            let snapshot = snapshot(state);
            if let Some(document) = &self.document {
                document.update(snapshot);
            } else {
                self.document = Some(Arc::new(DetailDocument::new(snapshot)));
            }
            self.cells = self
                .document
                .iter()
                .map(|document| Arc::clone(document) as Arc<dyn HistoryCell>)
                .collect();
            self.document_key = Some(key);
            self.transcript_revision = None;
            self.boundary = None;
        }
        self.render(area, buffer);
    }

    pub(super) fn render_cells(
        &mut self,
        cells: Vec<Arc<dyn HistoryCell>>,
        area: Rect,
        buffer: &mut Buffer,
    ) {
        self.render_cells_revision(&cells, "", /*revision*/ 0, area, buffer);
    }

    pub(super) fn render_cells_revision(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        target_key: &str,
        revision: u64,
        area: Rect,
        buffer: &mut Buffer,
    ) {
        let changed_target = self
            .transcript_revision
            .as_ref()
            .is_none_or(|(key, _)| key != target_key);
        let changed = changed_target
            || self
                .transcript_revision
                .as_ref()
                .is_none_or(|(_, old)| *old != revision)
            || (revision == 0
                && (cells.len() != self.cells.len()
                    || cells
                        .iter()
                        .zip(&self.cells)
                        .any(|(next, old)| !Arc::ptr_eq(next, old))));
        if changed {
            self.cells = cells.to_vec();
            self.transcript_revision = Some((target_key.to_owned(), revision));
            self.document_key = None;
            self.boundary = None;
            if changed_target {
                self.initialized = false;
            }
        }
        self.render(area, buffer);
    }

    fn render(&mut self, area: Rect, buffer: &mut Buffer) {
        if self.size != (area.width, area.height) {
            self.size = (area.width, area.height);
            self.boundary = None;
        }
        if !self.initialized {
            self.view.set_presentation(
                /*detailed*/ self.transcript_revision.is_none(),
                HistoryRenderMode::Rich,
            );
            self.view.jump_to_entry(&self.cells, /*index*/ 0);
            self.initialized = true;
            self.boundary = Some(false);
        }
        self.view.render(area, buffer, &self.cells);
    }

    pub(super) fn handle_disclosure_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        keymap: &crate::keymap::RuntimeKeymap,
    ) -> bool {
        if self.transcript_revision.is_none() {
            return false;
        }
        self.view.set_keymap_bindings(keymap);
        self.view.toggle_visible_activity_key(key, &self.cells)
    }

    pub(super) fn scroll(&mut self, down: bool, height: u16) {
        if self.boundary == Some(down) {
            return;
        }
        self.boundary = None;
        let amount = height.saturating_sub(1).clamp(1, 4) as isize;
        self.view
            .scroll(&self.cells, if down { amount } else { -amount });
        if self.view.is_following() {
            self.boundary = Some(true);
        }
    }

    pub(super) fn jump_to_boundary(&mut self, bottom: bool) {
        if self.boundary == Some(bottom) {
            return;
        }
        if bottom {
            self.view.jump_to_latest();
        } else {
            self.view.jump_to_entry(&self.cells, /*index*/ 0);
        }
        self.boundary = Some(bottom);
    }
}

/// Copy only the selected document, not every discovered step or cached conversation.
pub(super) fn snapshot(state: &WorkflowState) -> WorkflowState {
    let row = state
        .visible_run_indices()
        .contains(&state.selected)
        .then(|| state.rows.get(state.selected))
        .flatten()
        .map(|row| WorkflowRow {
            title: row.title.clone(),
            read_only: row.read_only,
            read_only_reason: row.read_only_reason.clone(),
            overview: row.overview.clone(),
            metadata: row.metadata.clone(),
            metrics: row.metrics.clone(),
            phase_progress: row.phase_progress.clone(),
            logs: row.logs.clone(),
            details: row.details.clone(),
            source: row.source.clone(),
            children: state
                .child
                .and_then(|index| row.children.get(index))
                .cloned()
                .into_iter()
                .collect(),
            ..Default::default()
        });
    let saved = state
        .visible_saved_indices()
        .contains(&state.selected_saved_index())
        .then(|| state.saved.get(state.selected_saved_index()))
        .flatten()
        .cloned();
    let saved_sources = saved
        .as_ref()
        .and_then(|saved| {
            state.saved_sources.get(&saved.id).map(|source| {
                (
                    saved.id.clone(),
                    SavedWorkflowSource {
                        source: source.source.clone(),
                        loading: source.loading,
                        error: source.error.clone(),
                        request_id: source.request_id,
                    },
                )
            })
        })
        .into_iter()
        .collect();
    WorkflowState {
        rows: row.into_iter().collect(),
        child: state.child.map(|_| 0),
        saved: saved.into_iter().collect(),
        saved_sources,
        error: state.error.clone(),
        load_error: state.load_error.clone(),
        catalog_overflow: state.catalog_overflow,
        center: CenterState {
            saved: state.center.saved,
            detail_tab: state.center.detail_tab,
            saved_detail_tab: state.center.saved_detail_tab,
            ..Default::default()
        },
        ..Default::default()
    }
}
