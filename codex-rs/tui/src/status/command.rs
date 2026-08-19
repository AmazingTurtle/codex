//! Command echo for status cards, including asynchronously refreshed account limits.

use ratatui::prelude::*;
use ratatui::style::Stylize;

use crate::history_cell::HistoryCell;
use crate::history_cell::plain_lines;

use super::account_limits::StatusAccountLimits;

#[derive(Debug)]
pub(super) struct StatusCommandHistoryCell(pub(super) StatusAccountLimits);

impl HistoryCell for StatusCommandHistoryCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let command = if self.0.is_enabled() {
            "/status all"
        } else {
            "/status"
        };
        vec![command.magenta().into()]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.display_lines(u16::MAX))
    }
}
