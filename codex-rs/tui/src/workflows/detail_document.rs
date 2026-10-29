//! Selected-document revisions feed the ordinary history layout cache without rendering to measure.
use super::super::*;
use crate::history_cell::HistoryCell;
use ratatui::text::Line;
use std::hash::Hash;
use std::hash::Hasher;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

pub(super) struct DetailDocument {
    state: Mutex<DocumentSnapshot>,
    revision: AtomicU64,
}

/// Immutable rendering inputs exclude the UI-owned incremental transcript parsers and viewports.
struct DocumentSnapshot {
    rows: Vec<WorkflowRow>,
    child: Option<usize>,
    saved: Vec<SavedWorkflowRow>,
    saved_sources: std::collections::HashMap<String, SavedWorkflowSource>,
    error: Option<String>,
    load_error: Option<String>,
    catalog_overflow: bool,
    saved_view: bool,
    detail_tab: DetailTab,
    saved_detail_tab: DetailTab,
}

impl From<WorkflowState> for DocumentSnapshot {
    fn from(state: WorkflowState) -> Self {
        Self {
            rows: state.rows,
            child: state.child,
            saved: state.saved,
            saved_sources: state.saved_sources,
            error: state.error,
            load_error: state.load_error,
            catalog_overflow: state.catalog_overflow,
            saved_view: state.center.saved,
            detail_tab: state.center.detail_tab,
            saved_detail_tab: state.center.saved_detail_tab,
        }
    }
}

impl DetailDocument {
    pub(super) fn new(state: WorkflowState) -> Self {
        Self {
            state: Mutex::new(state.into()),
            revision: AtomicU64::new(0),
        }
    }

    pub(super) fn update(&self, state: WorkflowState) {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = state.into();
        self.revision.fetch_add(1, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for DetailDocument {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkflowDetails")
            .finish_non_exhaustive()
    }
}

impl HistoryCell for DetailDocument {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let snapshot = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // This transient adapter never enters a retained HistoryCell or its shared cache.
        let state = WorkflowState {
            rows: snapshot.rows.clone(),
            child: snapshot.child,
            saved: snapshot.saved.clone(),
            saved_sources: snapshot
                .saved_sources
                .iter()
                .map(|(id, source)| {
                    (
                        id.clone(),
                        SavedWorkflowSource {
                            source: source.source.clone(),
                            loading: source.loading,
                            error: source.error.clone(),
                            request_id: source.request_id,
                        },
                    )
                })
                .collect(),
            error: snapshot.error.clone(),
            load_error: snapshot.load_error.clone(),
            catalog_overflow: snapshot.catalog_overflow,
            center: CenterState {
                saved: snapshot.saved_view,
                detail_tab: snapshot.detail_tab,
                saved_detail_tab: snapshot.saved_detail_tab,
                ..Default::default()
            },
            ..Default::default()
        };
        super::super::details::lines(&state, width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(/*width*/ 4096)
    }

    fn transcript_animation_tick(&self) -> Option<u64> {
        Some(self.revision.load(Ordering::Relaxed))
    }
}

/// Hash borrowed rendering inputs; copying source, JSON results and phase prompts happens only
/// when they change. Width, theme and terminal colors are handled by TranscriptView's cache.
pub(super) fn content_key(state: &WorkflowState) -> u64 {
    let mut hash = std::hash::DefaultHasher::new();
    state
        .error
        .as_ref()
        .or(state.load_error.as_ref())
        .hash(&mut hash);
    state.center.saved.hash(&mut hash);
    state.active_detail_tab().hash(&mut hash);
    if state.center.saved {
        state.catalog_overflow.hash(&mut hash);
        if let Some(saved) = state.saved.get(state.selected_saved_index()).filter(|_| {
            state
                .visible_saved_indices()
                .contains(&state.selected_saved_index())
        }) {
            match state.active_detail_tab() {
                DetailTab::Overview => {
                    (
                        &saved.name,
                        &saved.description,
                        &saved.command_notice,
                        saved.read_only,
                        &saved.read_only_reason,
                    )
                        .hash(&mut hash);
                    saved
                        .metadata
                        .as_ref()
                        .map(|metadata| &metadata.name)
                        .hash(&mut hash);
                }
                DetailTab::Source => {
                    if let Some(source) = state.saved_sources.get(&saved.id) {
                        (&source.source, source.loading, &source.error).hash(&mut hash);
                    }
                }
                DetailTab::Transcript => {}
            }
        }
    } else if let Some(row) = state
        .rows
        .get(state.selected)
        .filter(|_| state.visible_run_indices().contains(&state.selected))
    {
        match state.active_detail_tab() {
            DetailTab::Source => row.source.hash(&mut hash),
            DetailTab::Overview => {
                let child = state.child.and_then(|index| row.children.get(index));
                let overview = child
                    .and_then(|child| child.overview.as_ref())
                    .or(row.overview.as_ref());
                child
                    .map_or(&row.details, |child| &child.details)
                    .hash(&mut hash);
                if let Some(overview) = overview {
                    if let Some(metrics) = &overview.metrics {
                        metrics
                            .usage
                            .as_ref()
                            .map(|usage| {
                                (
                                    usage.input_tokens,
                                    usage.output_tokens,
                                    usage.cached_input_tokens,
                                    usage.total_tokens,
                                )
                            })
                            .hash(&mut hash);
                        (
                            metrics.usage_complete,
                            &metrics.usage_incomplete_reason,
                            metrics.active_duration_ms,
                            metrics.wall_duration_ms,
                            metrics.model_turns,
                            metrics.agents_used,
                        )
                            .hash(&mut hash);
                    }
                    (
                        &overview.title,
                        &overview.metadata,
                        &overview.prompt,
                        &overview.input,
                        &overview.result,
                        &overview.error,
                        &overview.refresh_error,
                        &overview.result_label,
                    )
                        .hash(&mut hash);
                    let phase = child.is_some_and(|child| child.key.starts_with("phase:"));
                    phase.hash(&mut hash);
                    if phase {
                        for step in &overview.phase_steps {
                            (
                                &step.label,
                                &step.model,
                                &step.reasoning,
                                &step.prompt,
                                &step.status,
                            )
                                .hash(&mut hash);
                        }
                    }
                    if state.child.is_none() {
                        (row.read_only, &row.read_only_reason).hash(&mut hash);
                        row.metadata
                            .as_ref()
                            .map(|metadata| &metadata.description)
                            .hash(&mut hash);
                        if let Some(metrics) = &row.metrics {
                            metrics
                                .usage
                                .as_ref()
                                .map(|usage| (usage.input_tokens, usage.output_tokens))
                                .hash(&mut hash);
                            (
                                metrics.usage_complete,
                                &metrics.usage_incomplete_reason,
                                metrics.active_duration_ms,
                            )
                                .hash(&mut hash);
                        }
                        for phase in &row.phase_progress {
                            (&phase.title, std::mem::discriminant(&phase.status)).hash(&mut hash);
                        }
                        for entry in &row.logs {
                            (entry.sequence, &entry.message, &entry.phase).hash(&mut hash);
                        }
                    }
                }
            }
            DetailTab::Transcript => {
                if let Some(target) = state.selected_transcript_target() {
                    target.cache_key().hash(&mut hash);
                    if let Some(transcript) = state.transcripts.get(&target.cache_key()) {
                        (
                            &transcript.error,
                            &transcript.limitation,
                            transcript.loading,
                            &transcript.next_cursor,
                            transcript.content_revision,
                        )
                            .hash(&mut hash);
                    }
                }
            }
        }
    }
    hash.finish()
}
