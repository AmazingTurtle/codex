//! A scoped observer uses the same incremental ExecCell as the native worker widget.
use super::tools::CommandHistory;
use super::tools::command_execution_command_and_parsed;
use crate::exec_cell::ExecCell;
use crate::exec_cell::new_active_exec_command_with_display;
use crate::history_cell::ActivityDisclosure;
use crate::history_cell::HistoryCell;
use crate::terminal_hyperlinks::HyperlinkLine;
use codex_app_server_protocol::CommandExecutionStatus;
use codex_app_server_protocol::ThreadItem;
use codex_config::types::ToolCallDisplay;
use ratatui::text::Line;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

#[derive(Debug)]
pub(crate) struct LiveCommandCell {
    state: Mutex<State>,
    revision: AtomicU64,
}
#[derive(Debug)]
struct State {
    cell: ExecCell,
    item: ThreadItem,
}

impl LiveCommandCell {
    pub(crate) fn new(item: &ThreadItem, display: ToolCallDisplay) -> Option<Self> {
        let ThreadItem::CommandExecution {
            id,
            command,
            command_actions,
            source,
            status: CommandExecutionStatus::InProgress,
            aggregated_output,
            ..
        } = item
        else {
            return None;
        };
        let (command, parsed) = command_execution_command_and_parsed(command, command_actions);
        let mut cell = new_active_exec_command_with_display(
            id.clone(),
            command,
            parsed,
            *source,
            /*interaction_input*/ None,
            /*animations_enabled*/ false,
            display,
        );
        if let Some(output) = aggregated_output {
            cell.append_output(id, output);
        }
        Some(Self {
            state: Mutex::new(State {
                cell,
                item: item.clone(),
            }),
            revision: AtomicU64::new(0),
        })
    }

    pub(crate) fn update(&self, item: &ThreadItem) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.item == *item {
            return true;
        }
        let ThreadItem::CommandExecution {
            id,
            status,
            aggregated_output,
            ..
        } = item
        else {
            return false;
        };
        if state.item.id() != id {
            return false;
        }
        if *status == CommandExecutionStatus::InProgress {
            let previous = match &state.item {
                ThreadItem::CommandExecution {
                    aggregated_output, ..
                } => aggregated_output.as_deref().unwrap_or_default(),
                _ => return false,
            };
            let output = aggregated_output.as_deref().unwrap_or_default();
            let Some(delta) = output.strip_prefix(previous) else {
                return false;
            };
            let delta = delta.to_owned();
            state.cell.append_output(id, &delta);
        } else {
            let Some(complete) = CommandHistory::from_item(item.clone()) else {
                return false;
            };
            state.cell.complete_call(
                id,
                crate::exec_cell::CommandOutput::new(
                    complete.exit_code,
                    complete.aggregated_output,
                ),
                complete.duration,
            );
        }
        state.item = item.clone();
        self.revision.fetch_add(1, Ordering::Relaxed);
        true
    }
}

impl HistoryCell for LiveCommandCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cell
            .display_lines(width)
    }
    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cell
            .raw_lines()
    }
    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cell
            .compact_hyperlink_lines(width)
    }
    fn expanded_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cell
            .expanded_hyperlink_lines(width)
    }
    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.expanded_hyperlink_lines(width)
    }
    fn activity_ids(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cell
            .activity_ids()
    }
    fn activity_disclosure(&self, width: u16) -> Option<ActivityDisclosure> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cell
            .activity_disclosure(width)
    }
    fn transcript_animation_tick(&self) -> Option<u64> {
        Some(self.revision.load(Ordering::Relaxed))
    }
}
