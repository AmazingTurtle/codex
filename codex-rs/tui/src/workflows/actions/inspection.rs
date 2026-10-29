//! Immutable revision/execution inspection preserves the live center selection underneath.
use super::model::*;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::render_filled_tab_bar;
use crate::render::renderable::Renderable;
use crate::workflows::CenterState;
use crate::workflows::DetailTab;
use crate::workflows::SharedWorkflows;
use crate::workflows::TranscriptTarget;
use crate::workflows::WorkflowEvent;
use crate::workflows::WorkflowOverview;
use crate::workflows::WorkflowRow;
use crate::workflows::WorkflowState;
use codex_app_server_protocol::WorkflowExecutionSnapshot;
use codex_app_server_protocol::WorkflowResult;
use codex_app_server_protocol::WorkflowTranscriptTarget;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use std::collections::HashMap;
use std::sync::Mutex;

enum Document {
    Revision(Box<RevisionPreview>),
    Execution(Box<WorkflowExecutionSnapshot>),
}

pub(crate) struct InspectionView {
    document: Document,
    workflows: Option<SharedWorkflows>,
    tx: Option<AppEventSender>,
    target: Option<TranscriptTarget>,
    tab: DetailTab,
    viewports: Mutex<HashMap<DetailTab, crate::workflows::detail_viewport::DetailViewport>>,
    height: std::cell::Cell<u16>,
    complete: bool,
}

impl InspectionView {
    pub(crate) fn revision(revision: RevisionPreview) -> Self {
        Self {
            document: Document::Revision(Box::new(revision)),
            workflows: None,
            tx: None,
            target: None,
            tab: DetailTab::Source,
            viewports: Mutex::new(HashMap::new()),
            height: std::cell::Cell::new(0),
            complete: false,
        }
    }

    pub(crate) fn execution(
        scope: ActionScope,
        execution: WorkflowExecutionSnapshot,
        workflows: SharedWorkflows,
        tx: AppEventSender,
    ) -> Self {
        let attachment = workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .attached_thread
            .unwrap_or(scope.owner);
        let target = TranscriptTarget {
            attachment,
            owner: scope.owner,
            run_id: execution.run_id.clone(),
            execution_id: execution.execution_id.clone(),
            target: WorkflowTranscriptTarget::Run,
        };
        Self {
            document: Document::Execution(Box::new(execution)),
            workflows: Some(workflows),
            tx: Some(tx),
            target: Some(target),
            tab: DetailTab::Overview,
            viewports: Mutex::new(HashMap::new()),
            height: std::cell::Cell::new(0),
            complete: false,
        }
    }

    fn request_chat(&self, older: bool, refresh: bool) {
        let (Some(target), Some(workflows), Some(tx)) = (&self.target, &self.workflows, &self.tx)
        else {
            return;
        };
        let state = workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if refresh || !state.transcripts.contains_key(&target.cache_key()) {
            tx.send(AppEvent::Workflow(WorkflowEvent::ReadTranscript {
                target: target.clone(),
                older,
            }));
        }
    }

    fn snapshot(&self) -> WorkflowState {
        let (title, source, input, result, error, metrics, metadata) = match &self.document {
            Document::Revision(revision) => (
                format!("Revision {} · {}", revision.number, revision.status),
                revision.source.clone(),
                revision.input.clone(),
                None,
                revision.error.clone(),
                None,
                vec![
                    (
                        "Reused statements".into(),
                        format!(
                            "{} / {}",
                            revision.reusable_statements, revision.statement_count
                        ),
                    ),
                    ("Rerun reason".into(), revision.invalidation_reason.clone()),
                ],
            ),
            Document::Execution(execution) => {
                let result = match &execution.result {
                    WorkflowResult::Absent => None,
                    WorkflowResult::Value { value } => Some(value.clone()),
                };
                (
                    execution.metadata.as_ref().map_or_else(
                        || "Historical workflow execution".into(),
                        |metadata| format!("{} · historical execution", metadata.name),
                    ),
                    execution.source.clone(),
                    execution.input.clone(),
                    result,
                    execution.error.clone(),
                    Some(execution.metrics.clone()),
                    vec![
                        (
                            "Status".into(),
                            crate::workflows::projection::status(execution.status).into(),
                        ),
                        ("Partial output".into(), execution.partial.to_string()),
                    ],
                )
            }
        };
        let row = WorkflowRow {
            title: title.clone(),
            source,
            metrics,
            overview: Some(WorkflowOverview {
                title,
                metadata,
                input: Some(input),
                result,
                error,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut state = WorkflowState {
            rows: vec![row],
            center: CenterState {
                detail_tab: self.tab,
                ..Default::default()
            },
            ..Default::default()
        };
        if self.tab == DetailTab::Transcript {
            state.rows[0].overview = None;
            state.rows[0].details=vec![if self.target.is_some() {"Loading this execution’s owned agent chat…"} else {"A revision can have several executions. Return and choose an explicit execution to read its agent chat."}.into()];
            state.center.detail_tab = DetailTab::Overview;
        }
        state
    }
}

impl BottomPaneView for InspectionView {
    fn view_id(&self) -> Option<&'static str> {
        Some(super::INSPECTION_ID)
    }
    fn is_complete(&self) -> bool {
        self.complete
    }
    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }
    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.complete = true;
        CancellationEvent::Handled
    }
    fn handle_key_event(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release || !key.modifiers.is_empty() {
            return;
        }
        match key.code {
            KeyCode::Esc => self.complete = true,
            KeyCode::Char('1' | '2' | '3') | KeyCode::Left | KeyCode::Right => {
                self.tab = match key.code {
                    KeyCode::Char('1') => DetailTab::Overview,
                    KeyCode::Char('2') => DetailTab::Transcript,
                    KeyCode::Char('3') => DetailTab::Source,
                    KeyCode::Right => self.tab.step(true),
                    _ => self.tab.step(false),
                };
                if self.tab == DetailTab::Transcript {
                    self.request_chat(/*older*/ false, /*refresh*/ false);
                }
            }
            KeyCode::PageUp | KeyCode::PageDown | KeyCode::Char('j' | 'k') => {
                if let Some(view) = self
                    .viewports
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_mut(&self.tab)
                {
                    if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
                        view.jump_to_boundary(key.code == KeyCode::PageDown);
                    } else {
                        view.scroll(key.code == KeyCode::Char('k'), self.height.get());
                    }
                }
            }
            KeyCode::Char('p') if self.tab == DetailTab::Transcript => {
                self.request_chat(/*older*/ true, /*refresh*/ true)
            }
            KeyCode::Char('R') if self.tab == DetailTab::Transcript => {
                self.request_chat(/*older*/ false, /*refresh*/ true)
            }
            _ => {}
        }
    }
}

impl Renderable for InspectionView {
    fn desired_height(&self, _width: u16) -> u16 {
        24
    }
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        Clear.render(area, buffer);
        if area.height == 0 {
            return;
        }
        let inner = area.inner(ratatui::layout::Margin::new(2, 0));
        let title = match &self.document {
            Document::Revision(revision) => {
                format!("Revision {} · read-only source", revision.number)
            }
            Document::Execution(_) => "Historical execution · read-only".into(),
        };
        Paragraph::new(Line::from(title.bold())).render(Rect { height: 1, ..inner }, buffer);
        if inner.height < 3 {
            return;
        }
        let selected = match self.tab {
            DetailTab::Overview => 0,
            DetailTab::Transcript => 1,
            DetailTab::Source => 2,
        };
        render_filled_tab_bar(
            &["1 Overview", "2 Agent chat", "3 Source"],
            selected,
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
            buffer,
        );
        let body = Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            inner.height.saturating_sub(3),
        );
        self.height.set(body.height);
        let cells = if self.tab == DetailTab::Transcript {
            self.target
                .as_ref()
                .zip(self.workflows.as_ref())
                .and_then(|(target, state)| {
                    state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .transcripts
                        .get(&target.cache_key())
                        .map(|transcript| transcript.cells.clone())
                })
                .filter(|cells| !cells.is_empty())
        } else {
            None
        };
        let mut views = self
            .viewports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let view = views.entry(self.tab).or_default();
        if let Some(cells) = cells {
            view.render_cells(cells, body, buffer);
        } else {
            view.render_document(self.snapshot(), body, buffer);
        }
        Paragraph::new(Line::from(vec![
            "Esc".bold(),
            " back · ".dim(),
            "1/2/3".bold(),
            " view · ".dim(),
            "j↑ k↓".bold(),
            " scroll · ".dim(),
            "PgUp/PgDn".bold(),
            " top/bottom".dim(),
        ]))
        .render(
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
            buffer,
        );
    }
}
