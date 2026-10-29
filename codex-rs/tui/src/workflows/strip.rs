//! One physical row per current-conversation workflow, with bounded overflow.

use super::SharedWorkflows;
use super::WorkflowRow;
use crate::line_truncation::line_width;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::render::renderable::Renderable;
use crate::style::StatusTone;
use crate::style::secondary_text_style;
use crate::style::status_style;
use crate::width::display_width;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;

pub(crate) struct WorkflowStrip(pub(crate) SharedWorkflows);

struct Entry {
    title: String,
    status: String,
    progress: [String; 2],
    bar: String,
    phase: Option<String>,
    usage: [String; 2],
    elapsed: Option<String>,
    agents: Option<String>,
    read_only: bool,
}

fn active(status: &str) -> bool {
    matches!(status, "Pending" | "Running" | "Pausing" | "Paused")
}

impl Entry {
    fn new(row: &WorkflowRow, status: &str) -> Self {
        let title = row.title.split_whitespace().collect::<Vec<_>>().join(" ");
        Self {
            title: if title.is_empty() {
                "Workflow".into()
            } else {
                title
            },
            status: status.into(),
            progress: [
                super::display::progress(row, /*compact*/ false),
                super::display::progress(row, /*compact*/ true),
            ],
            bar: super::display::bar(row),
            phase: row
                .phase
                .as_ref()
                .map(|phase| phase.split_whitespace().collect::<Vec<_>>().join(" ")),
            usage: [
                super::display::usage(row, /*compact*/ false),
                super::display::usage(row, /*compact*/ true),
            ],
            elapsed: super::display::elapsed(row),
            read_only: row.read_only,
            agents: row.agents_used.map(|count| {
                format!(
                    "{count} {} used",
                    if count == 1 { "agent" } else { "agents" }
                )
            }),
        }
    }

    fn render(&self, width: usize) -> Line<'static> {
        let running = active(&self.status);
        let title_style = if running {
            Style::default().bold()
        } else {
            secondary_text_style()
        };
        let status = match self.status.as_str() {
            "Running" | "Pending" => crate::style::accent_style().bold(),
            "Paused" | "Pausing" => status_style(StatusTone::Attention),
            "Completed" => status_style(StatusTone::Success).remove_modifier(Modifier::BOLD),
            "Failed" => status_style(StatusTone::Failure).remove_modifier(Modifier::BOLD),
            "Interrupted" | "Stopped" => {
                status_style(StatusTone::Attention).remove_modifier(Modifier::BOLD)
            }
            _ => secondary_text_style(),
        };
        let field_width = |fields: &[Span<'_>]| {
            fields
                .iter()
                .map(|field| 3 + display_width(&field.content))
                .sum::<usize>()
        };
        let mut fields = Vec::new();
        for mode in 0..=8 {
            fields.clear();
            if mode < 5 {
                let progress = &self.progress[usize::from(mode >= 3)];
                fields.push(Span::styled(
                    if mode < 2 {
                        format!("{} {progress}", self.bar)
                    } else {
                        progress.clone()
                    },
                    status,
                ));
            }
            if mode == 0
                && let Some(phase) = &self.phase
            {
                fields.push(Span::styled(phase.clone(), secondary_text_style()));
            }
            if self.read_only {
                fields.push(Span::styled(
                    "Legacy · read-only",
                    status_style(StatusTone::Attention),
                ));
            } else if self.status != "Running" {
                fields.push(Span::styled(self.status.clone(), status));
            }
            if mode < 7 {
                fields.push(Span::styled(
                    self.usage[usize::from(mode >= 3)].clone(),
                    secondary_text_style(),
                ));
            }
            if mode < 6
                && let Some(elapsed) = &self.elapsed
            {
                fields.push(Span::styled(elapsed.clone(), secondary_text_style()));
            }
            if mode == 0
                && let Some(agents) = &self.agents
            {
                fields.push(Span::styled(agents.clone(), secondary_text_style()));
            }
            if display_width(&self.title).min(if mode >= 3 { 4 } else { 8 }) + field_width(&fields)
                <= width
            {
                break;
            }
            if mode == 8 {
                fields.clear();
            }
        }
        let mut title = truncate_line_with_ellipsis_if_overflow(
            Line::from(Span::styled(self.title.clone(), title_style)),
            width.saturating_sub(field_width(&fields)),
        );
        for field in fields {
            title
                .spans
                .push(Span::styled(" · ", secondary_text_style()));
            title.spans.push(field);
        }
        title
    }
}

impl Renderable for WorkflowStrip {
    fn desired_height(&self, _width: u16) -> u16 {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = state.current_indices().len();
        if count == 0 {
            u16::from(state.inspected_agent.is_some())
        } else {
            count.min(4) as u16 + u16::from(count > 4)
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut indices = state.current_indices();
        indices.sort_by(|left, right| {
            let left = &state.rows[*left];
            let right = &state.rows[*right];
            let left_active = active(state.row_status(left));
            let right_active = active(state.row_status(right));
            right_active.cmp(&left_active).then_with(|| {
                let time = if left_active {
                    right.created_at.cmp(&left.created_at)
                } else {
                    right.updated_at.cmp(&left.updated_at)
                };
                time.then_with(|| left.id.cmp(&right.id))
            })
        });
        let width = usize::from(area.width);
        if indices.is_empty() {
            if state.inspected_agent.is_some() {
                truncate_line_with_ellipsis_if_overflow(
                    Line::from("  Workflow agent · ↓ /workflows").style(secondary_text_style()),
                    width,
                )
                .render(Rect::new(area.x, area.y, area.width, 1), buf);
            }
            return;
        }
        let prefix = if width < 10 {
            ""
        } else if state.inspected_agent.is_some() {
            "↳ "
        } else {
            "  "
        };
        let prefix_width = display_width(prefix);
        let height = usize::from(area.height);
        let needs_overflow = indices.len() > height.min(4);
        let overflow_row = needs_overflow && height > 1;
        let visible = indices
            .len()
            .min(4)
            .min(height.saturating_sub(usize::from(overflow_row)));
        let hidden = indices.len().saturating_sub(visible);
        for (position, index) in indices.iter().take(visible).enumerate() {
            let suffix = if hidden > 0 && !overflow_row {
                format!(" … +{hidden}")
            } else {
                String::new()
            };
            let mut line = Line::from(Span::styled(prefix, secondary_text_style()));
            line.spans.extend(
                Entry::new(&state.rows[*index], state.row_status(&state.rows[*index]))
                    .render(width.saturating_sub(prefix_width + display_width(&suffix)))
                    .spans,
            );
            line.spans
                .push(Span::styled(suffix, secondary_text_style()));
            let hint = if state.inspected_agent.is_some() {
                " · Agent view · ↓ open"
            } else {
                " · ↓ open"
            };
            if position == 0 && line_width(&line) + display_width(hint) <= width {
                line.spans.push(Span::styled(hint, secondary_text_style()));
            }
            truncate_line_with_ellipsis_if_overflow(line, width).render(
                Rect::new(area.x, area.y + position as u16, area.width, 1),
                buf,
            );
        }
        if overflow_row {
            let noun = if hidden == 1 { "workflow" } else { "workflows" };
            truncate_line_with_ellipsis_if_overflow(
                Line::from(format!("{prefix}… +{hidden} {noun} · ↓ open"))
                    .style(secondary_text_style()),
                width,
            )
            .render(
                Rect::new(area.x, area.y + visible as u16, area.width, 1),
                buf,
            );
        }
    }
}

#[cfg(test)]
#[path = "strip_tests.rs"]
mod tests;
