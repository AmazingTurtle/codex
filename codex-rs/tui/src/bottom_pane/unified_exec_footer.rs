//! Renders and formats unified-exec background session summary text.
//!
//! This module provides one canonical summary string so the bottom pane can
//! either render a dedicated footer row or reuse the same text inline in the
//! status row without duplicating copy/grammar logic.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::live_wrap::take_prefix_by_width;
use crate::render::renderable::Renderable;

/// Tracks active unified-exec processes and renders a compact summary.
pub(crate) struct UnifiedExecFooter {
    processes: Vec<String>,
    wake_state: codex_app_server_protocol::ThreadWakeState,
}

impl UnifiedExecFooter {
    pub(crate) fn new() -> Self {
        Self {
            processes: Vec::new(),
            wake_state: Default::default(),
        }
    }

    pub(crate) fn set_processes(&mut self, processes: Vec<String>) -> bool {
        if self.processes == processes {
            return false;
        }
        self.processes = processes;
        true
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.processes.is_empty() && self.wake_state.pending_count == 0
    }

    pub(crate) fn set_wake_state(
        &mut self,
        state: codex_app_server_protocol::ThreadWakeState,
    ) -> bool {
        if self.wake_state == state {
            return false;
        }
        self.wake_state = state;
        true
    }

    /// Returns the unindented summary text used by both footer and status-row rendering.
    ///
    /// The returned string intentionally omits leading spaces and separators so
    /// callers can choose layout-specific framing (inline separator vs. row
    /// indentation). Returning `None` means there is nothing to surface.
    pub(crate) fn summary_text(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut summaries = Vec::new();
        let count = self.wake_state.pending_count;
        if count > 0 {
            let plural = if count == 1 { "" } else { "s" };
            summaries.push(if self.wake_state.waiting {
                format!("Waiting for {count} wake signal{plural}")
            } else {
                format!("{count} wake signal{plural} pending")
            });
        }
        let count = self.processes.len();
        let plural = if count == 1 { "" } else { "s" };
        if count > 0 {
            summaries.push(format!(
                "{count} background terminal{plural} running · /ps to view · /stop to close"
            ));
        }
        Some(summaries.join(" · "))
    }

    fn render_lines(&self, width: u16) -> Vec<Line<'static>> {
        if width < 4 {
            return Vec::new();
        }
        let Some(summary) = self.summary_text() else {
            return Vec::new();
        };
        let message = format!("  {summary}");
        let (truncated, _, _) = take_prefix_by_width(&message, width as usize);
        vec![Line::from(truncated.dim())]
    }
}

impl Renderable for UnifiedExecFooter {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        Paragraph::new(self.render_lines(area.width)).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.render_lines(width).len() as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;

    #[test]
    fn desired_height_empty() {
        let footer = UnifiedExecFooter::new();
        assert_eq!(footer.desired_height(/*width*/ 40), 0);
    }

    #[test]
    fn render_more_sessions() {
        let mut footer = UnifiedExecFooter::new();
        footer.set_processes(vec!["rg \"foo\" src".to_string()]);
        let width = 50;
        let height = footer.desired_height(width);
        let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
        footer.render(Rect::new(0, 0, width, height), &mut buf);
        assert_snapshot!("render_more_sessions", format!("{buf:?}"));
    }

    #[test]
    fn render_many_sessions() {
        let mut footer = UnifiedExecFooter::new();
        footer.set_processes((0..123).map(|idx| format!("cmd {idx}")).collect());
        let width = 50;
        let height = footer.desired_height(width);
        let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
        footer.render(Rect::new(0, 0, width, height), &mut buf);
        assert_snapshot!("render_many_sessions", format!("{buf:?}"));
    }
}
