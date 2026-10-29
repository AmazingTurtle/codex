//! The Agent command center's three-row header, filled tabs, split body and bottom footer.
use super::*;
use crate::bottom_pane::render_filled_tab_bar;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Margin;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::WidgetRef;

struct CenterLayout {
    header: Rect,
    body: Rect,
    list: Rect,
    divider: Rect,
    details: Rect,
    footer: Rect,
}

pub(super) fn line(text: Line<'static>, area: Rect, buf: &mut Buffer) {
    if !area.is_empty() {
        truncate_line_with_ellipsis_if_overflow(text, usize::from(area.width))
            .render(Rect { height: 1, ..area }, buf);
    }
}

impl WorkflowCenter {
    fn layout(&self, area: Rect) -> CenterLayout {
        let area = area.inner(Margin::new(2, 0));
        let header_height = if area.height >= 8 { 3 } else { 1 };
        let footer_height = if area.height >= 8 && area.width < 90 {
            2
        } else {
            1
        };
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(header_height),
            Constraint::Min(0),
            Constraint::Length(footer_height),
        ])
        .areas(area);
        let focused = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .center
            .details_focused;
        let (list, divider, details) = if body.width >= 90 {
            let list_width = (body.width.saturating_sub(3) * 30 / 100).max(28);
            let [list, divider, details] = Layout::horizontal([
                Constraint::Length(list_width),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .areas(body);
            (list, divider, details)
        } else if focused || self.input.is_some() {
            (Rect::default(), Rect::default(), body)
        } else {
            (body, Rect::default(), Rect::default())
        };
        CenterLayout {
            header,
            body,
            list,
            divider,
            details,
            footer,
        }
    }

    fn input_area(details: Rect, has_error: bool, compact: bool) -> Rect {
        let label = u16::from(!compact);
        Rect::new(
            details.x,
            details.y.saturating_add(label),
            details.width,
            details.height.saturating_sub(label + u16::from(has_error)),
        )
    }

    fn footer(&self, width: u16) -> Vec<Line<'static>> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.input.is_some() {
            return vec![
                vec![
                    "Esc".bold(),
                    " cancel · ".dim(),
                    "Enter".bold(),
                    " run".dim(),
                ]
                .into(),
            ];
        }
        if state.center.help {
            return vec![
                vec![
                    "Esc".bold(),
                    " back · ".dim(),
                    "↑/↓".bold(),
                    " scroll".dim(),
                ]
                .into(),
            ];
        }
        if state.center.saved {
            let read_only = state
                .saved
                .get(state.selected_saved_index())
                .is_some_and(|saved| saved.read_only);
            return vec![
                vec![
                    "Esc".bold(),
                    if state.center.narrow && state.center.details_focused {
                        " library · ".dim()
                    } else {
                        " Runs · ".dim()
                    },
                    "1/3".bold(),
                    " view · ".dim(),
                    if read_only {
                        "read-only".dim()
                    } else {
                        "Enter".bold()
                    },
                    if read_only { "".into() } else { " run".dim() },
                    " · ".dim(),
                    "↑/↓".bold(),
                    " move".dim(),
                ]
                .into(),
                if state.catalog_overflow {
                    Line::from("catalog limit reached; use /workflows").style(
                        crate::style::status_style(crate::style::StatusTone::Attention),
                    )
                } else {
                    vec!["Tab".bold(), " filter · ".dim(), "?".bold(), " help".dim()].into()
                },
            ];
        }
        let cold = state.selected_requires_owner_open();
        let worker = state.selected_worker_thread().is_some();
        let conversation =
            state.child.is_none() && state.visible_run_indices().contains(&state.selected);
        let opens_chat = worker || conversation;
        let chat_label = if worker { " chat · " } else { " Main · " };
        let tool_hint = (state.active_detail_tab() == DetailTab::Transcript)
            .then(|| {
                self.runtime_keymap
                    .app
                    .focus_activity
                    .first()
                    .map(crate::key_hint::KeyBinding::display_label)
            })
            .flatten();
        if state.center.narrow && state.center.details_focused {
            let mut primary = vec!["Esc".bold(), " list · ".dim()];
            if opens_chat {
                primary.extend(["Enter".bold(), chat_label.dim()]);
            }
            primary.extend(["1/2/3".bold(), " view".dim()]);
            if !opens_chat || width >= 50 {
                primary.extend([" · ".dim(), "Space".bold(), " Actions".dim()]);
            }
            if cold {
                primary.extend([" · ".dim(), "o".bold(), " open conversation".dim()]);
            }
            return vec![
                primary.into(),
                if let Some(hint) = tool_hint {
                    vec![
                        "j↑ k↓".bold(),
                        " · ".dim(),
                        "PgUp/PgDn".bold(),
                        " ends · ".dim(),
                        hint.bold(),
                        " tools".dim(),
                    ]
                    .into()
                } else {
                    vec![
                        "j↑ k↓".bold(),
                        " scroll · ".dim(),
                        "PgUp/PgDn".bold(),
                        " ends · ".dim(),
                        "?".bold(),
                        " help".dim(),
                    ]
                    .into()
                },
            ];
        }
        if width < 90 {
            vec![
                vec![
                    "?".bold(),
                    " help · ".dim(),
                    "Esc".bold(),
                    " back · ".dim(),
                    "↑/↓".bold(),
                    " move".dim(),
                ]
                .into(),
                {
                    let mut secondary = vec![];
                    if opens_chat {
                        secondary.extend(["Enter".bold(), chat_label.dim()]);
                    }
                    secondary.extend(["1/2/3".bold(), " view".dim()]);
                    if !opens_chat || width >= 50 {
                        secondary.extend([" · ".dim(), "Space".bold(), " Actions".dim()]);
                    }
                    secondary.into()
                },
            ]
        } else {
            let mut spans = vec![
                "Esc".bold(),
                " close · ".dim(),
                "↑/↓".bold(),
                " select · ".dim(),
            ];
            if opens_chat {
                spans.extend(["Enter".bold(), chat_label.dim()]);
            }
            spans.extend([
                "j↑ k↓".bold(),
                " scroll · ".dim(),
                "PgUp/PgDn".bold(),
                " ends · ".dim(),
                "1/2/3".bold(),
                " view · ".dim(),
            ]);
            if let Some(hint) = tool_hint {
                spans.extend([hint.bold(), " tools · ".dim()]);
            }
            spans.extend([
                "Space".bold(),
                " Actions · ".dim(),
                "?".bold(),
                " help".dim(),
            ]);
            if cold {
                spans.extend([" · ".dim(), "o".bold(), " open conversation".dim()]);
            }
            vec![spans.into()]
        }
    }

    fn render_header(&self, state: &WorkflowState, area: Rect, buf: &mut Buffer) {
        let library = state.center.saved;
        let title = if library {
            "Saved workflows"
        } else {
            "Workflow command center"
        };
        let metadata = if library {
            "  Tab / Shift+Tab filter".into()
        } else if area.width >= 80 {
            format!(
                "  Group: {} g  Scope: {} f",
                state.center.grouping.label(),
                state.scope.label()
            )
        } else {
            format!(" · Scope: {} f", state.scope.label())
        };
        line(
            vec![
                if area.width < 60 && !library {
                    "Workflows".bold()
                } else {
                    title.bold()
                },
                metadata.dim(),
            ]
            .into(),
            Rect { height: 1, ..area },
            buf,
        );
        if area.height < 3 {
            return;
        }
        let (labels, active) = if library {
            let labels = super::super::filters::LIBRARY_FILTERS
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    let count = state
                        .saved
                        .iter()
                        .filter(|saved| match index {
                            1 => {
                                saved.scope
                                    == Some(codex_app_server_protocol::WorkflowScope::Project)
                            }
                            2 => {
                                saved.scope == Some(codex_app_server_protocol::WorkflowScope::User)
                            }
                            _ => true,
                        })
                        .count();
                    format!("{label} {count}")
                })
                .collect::<Vec<_>>();
            (labels, state.center.library_filter)
        } else {
            let labels = super::super::filters::RUN_FILTERS
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    format!(
                        "{label} {}",
                        state
                            .rows
                            .iter()
                            .filter(|row| index == 0 || state.status_group(row) == index)
                            .count()
                    )
                })
                .collect::<Vec<_>>();
            (labels, state.center.status_filter)
        };
        let page = if library {
            &state.saved_page
        } else {
            &state.page
        };
        let hint = if page.next.is_some() || !page.previous.is_empty() {
            format!("Tab filter · Page {} [/ ]", page.previous.len() + 1)
        } else {
            "Tab / Shift+Tab filter".into()
        };
        let mut tabs = Rect::new(area.x, area.y + 1, area.width, 1);
        if tabs.width >= 80 {
            let width = hint.len() as u16;
            line(
                hint.dim().into(),
                Rect::new(tabs.right().saturating_sub(width), tabs.y, width, 1),
                buf,
            );
            tabs.width = tabs.width.saturating_sub(width + 2);
        }
        render_filled_tab_bar(
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
            active,
            tabs,
            buf,
        );
        line(
            "─".repeat(usize::from(area.width)).dim().into(),
            Rect::new(area.x, area.y + 2, area.width, 1),
            buf,
        );
    }
}

impl Renderable for WorkflowCenter {
    fn desired_height(&self, _width: u16) -> u16 {
        24
    }
    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.input.as_ref()?.cursor_pos(Self::input_area(
            self.layout(area).details,
            self.input_error.is_some(),
            area.height < 8,
        ))
    }
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let layout = self.layout(area);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.center.narrow = layout.body.width < 90;
        self.render_header(&state, layout.header, buf);
        if state.center.help {
            let lines = help::lines(self, layout.body.width);
            let scroll = state
                .center
                .help_scroll
                .min(lines.len().saturating_sub(usize::from(layout.body.height)) as u16);
            Paragraph::new(lines)
                .scroll((scroll, 0))
                .render(layout.body, buf);
        } else {
            rows::render(&mut state, layout.list, buf);
            for y in layout.divider.y..layout.divider.bottom() {
                line(
                    " │ ".dim().into(),
                    Rect::new(layout.divider.x, y, layout.divider.width, 1),
                    buf,
                );
            }
            if !layout.details.is_empty() {
                if let Some(input) = &self.input {
                    if area.height >= 8 {
                        line(
                            if state.operation_pending {
                                "Starting workflow…"
                            } else {
                                "Input · JSON · empty means {}"
                            }
                            .bold()
                            .into(),
                            layout.details,
                            buf,
                        );
                    }
                    input.render_ref(
                        Self::input_area(
                            layout.details,
                            self.input_error.is_some(),
                            area.height < 8,
                        ),
                        buf,
                    );
                    if let Some(error) = &self.input_error {
                        line(
                            error.clone().red().into(),
                            Rect::new(
                                layout.details.x,
                                layout.details.bottom().saturating_sub(1),
                                layout.details.width,
                                1,
                            ),
                            buf,
                        );
                    }
                } else {
                    {
                        let tab = match state.active_detail_tab() {
                            DetailTab::Overview => 0,
                            DetailTab::Transcript => 1,
                            DetailTab::Source => 2,
                        };
                        render_filled_tab_bar(
                            if state.center.saved {
                                &["1 Overview", "2 Unavailable", "3 Source"]
                            } else {
                                &["1 Overview", "2 Transcript", "3 Source"]
                            },
                            tab,
                            Rect::new(layout.details.x, layout.details.y, layout.details.width, 1),
                            buf,
                        );
                    }
                    let body = Rect::new(
                        layout.details.x,
                        layout.details.y + 1,
                        layout.details.width,
                        layout.details.height.saturating_sub(1),
                    );
                    let scroll_key = state.detail_key();
                    state.center.detail_page_height = body.height;
                    if state.center.detail_viewports.len() >= 32
                        && !state.center.detail_viewports.contains_key(&scroll_key)
                        && let Some(old) = state.center.detail_viewports.keys().next().cloned()
                    {
                        state.center.detail_viewports.remove(&old);
                    }
                    let mut viewport = state
                        .center
                        .detail_viewports
                        .remove(&scroll_key)
                        .unwrap_or_default();
                    if !state.center.saved
                        && state.active_detail_tab() == DetailTab::Transcript
                        && let Some(cells) = state.selected_transcript_cells()
                        && let Some(target) = state.selected_transcript_target()
                    {
                        viewport.render_cells_revision(
                            cells,
                            &target.cache_key(),
                            state.selected_transcript_revision().unwrap_or_default(),
                            body,
                            buf,
                        );
                    } else {
                        viewport.render_state(&state, body, buf);
                    }
                    state.center.detail_viewports.insert(scroll_key, viewport);
                }
            }
        }
        drop(state);
        Paragraph::new(self.footer(layout.footer.width)).render(layout.footer, buf);
    }
}
