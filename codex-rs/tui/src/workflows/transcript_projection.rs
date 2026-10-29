//! Retain native cell groups by their actual ordered members, rebuilding only a changed block.
use crate::history_cell::HistoryCell;
use crate::history_cell::StreamingAgentTailCell;
use crate::legacy_core::config::Config;
use crate::streaming::controller::StreamController;
use crate::thread_transcript::ProjectionOptions;
use crate::thread_transcript::RawReasoningVisibility;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadItemEntry;
use codex_app_server_protocol::WorkflowTranscriptGroup;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct AgentHeader {
    pub(crate) origin: WorkflowTranscriptGroup,
}

impl HistoryCell for AgentHeader {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let style = super::super::agent_style(
            &self.origin.run_id,
            &self.origin.step_id,
            /*background*/ None,
        );
        let caption = format!(
            "{} · {}{}",
            self.origin.label,
            self.origin
                .resolved_model
                .as_deref()
                .unwrap_or("model unrecorded"),
            self.origin
                .reasoning_effort
                .as_ref()
                .map_or(String::new(), |effort| format!(" · {effort}"))
        );
        vec![
            Line::from("─".repeat(usize::from(width))).style(style),
            Line::from(caption).style(style),
        ]
    }
    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(/*width*/ 0)
    }
}

#[derive(Debug)]
pub(crate) struct OutcomeCell {
    pub(crate) value: serde_json::Value,
    pub(crate) original: String,
}
impl HistoryCell for OutcomeCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = vec![Line::default(), "Outcome".bold().into()];
        lines.extend(crate::structured_value::render_value(&self.value, width));
        lines
    }
    fn raw_lines(&self) -> Vec<Line<'static>> {
        crate::history_cell::raw_lines_from_source(&self.original)
    }
    fn copy_source(&self) -> Option<&str> {
        Some(&self.original)
    }
}

#[derive(Default)]
pub(crate) struct OrderedProjection {
    blocks: Vec<Block>,
}

struct Block {
    entries: Vec<ThreadItemEntry>,
    options: ProjectionOptions,
    cells: Vec<Arc<dyn HistoryCell>>,
    stream: Option<StreamController>,
    committed: Vec<Arc<dyn HistoryCell>>,
    command: Option<Arc<crate::thread_transcript::LiveCommandCell>>,
    raw_reasoning: bool,
}

impl std::fmt::Debug for OrderedProjection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrderedProjection")
            .field("blocks", &self.blocks.len())
            .finish()
    }
}

fn activity(item: &ThreadItem) -> bool {
    matches!(
        item,
        ThreadItem::CommandExecution { .. }
            | ThreadItem::McpToolCall { .. }
            | ThreadItem::Reasoning { .. }
    )
}

impl OrderedProjection {
    pub(crate) fn update(
        &mut self,
        origin: &WorkflowTranscriptGroup,
        entries: &[&ThreadItemEntry],
        config: &Config,
        options: ProjectionOptions,
    ) -> Vec<Arc<dyn HistoryCell>> {
        let mut previous = std::mem::take(&mut self.blocks);
        let mut offset = 0;
        while offset < entries.len() {
            let mut end = offset + 1;
            if activity(&entries[offset].item) {
                while end < entries.len()
                    && end - offset < 32
                    && activity(&entries[end].item)
                    && entries[end].turn_id == entries[offset].turn_id
                {
                    end += 1;
                }
            }
            let source = &entries[offset..end];
            let mut options = options;
            if !source.iter().any(|entry| match &entry.item {
                ThreadItem::AgentMessage { workflow: None, .. } => entry.completed_at_ms.is_none(),
                ThreadItem::CommandExecution { status, .. } => {
                    *status == codex_app_server_protocol::CommandExecutionStatus::InProgress
                }
                ThreadItem::McpToolCall { status, .. } => {
                    *status == codex_app_server_protocol::McpToolCallStatus::InProgress
                }
                _ => false,
            }) {
                options.source = crate::thread_transcript::ProjectionSource::History;
            }
            let old = previous
                .iter()
                .position(|block| {
                    block.entries.first().is_some_and(|first| {
                        first.turn_id == source[0].turn_id && first.item.id() == source[0].item.id()
                    })
                })
                .map(|index| previous.remove(index));
            if old.as_ref().is_some_and(|block| {
                block.options == options
                    && block.raw_reasoning == config.show_raw_agent_reasoning
                    && block.entries.iter().eq(source.iter().copied())
            }) {
                // Move the unchanged block, preserving every Arc and stream continuation.
                if let Some(block) = old {
                    self.blocks.push(block);
                }
            } else {
                let mut block = old
                    .filter(|block| block.options.tool_call_display == options.tool_call_display)
                    .unwrap_or_else(|| Block {
                        entries: Vec::new(),
                        options,
                        cells: Vec::new(),
                        stream: None,
                        committed: Vec::new(),
                        command: None,
                        raw_reasoning: config.show_raw_agent_reasoning,
                    });
                block.options = options;
                block.raw_reasoning = config.show_raw_agent_reasoning;
                let live = source.len() == 1 && source[0].completed_at_ms.is_none();
                let streaming = if let ThreadItem::AgentMessage { text, .. } = &source[0].item {
                    live.then_some(text)
                } else {
                    None
                };
                let command = (source.len() == 1)
                    .then(|| {
                        block
                            .command
                            .as_ref()
                            .filter(|cell| cell.update(&source[0].item))
                            .cloned()
                            .or_else(|| {
                                (options.source == crate::thread_transcript::ProjectionSource::Live)
                                    .then(|| {
                                        crate::thread_transcript::LiveCommandCell::new(
                                            &source[0].item,
                                            options.tool_call_display,
                                        )
                                    })
                                    .flatten()
                                    .map(Arc::new)
                            })
                    })
                    .flatten();
                if let Some(command) = command {
                    block.cells = vec![Arc::clone(&command) as Arc<dyn HistoryCell>];
                    block.command = Some(command);
                } else if let Some(text) = streaming.filter(|_| {
                    options.source == crate::thread_transcript::ProjectionSource::Live
                        && !matches!(
                            &source[0].item,
                            ThreadItem::AgentMessage {
                                workflow: Some(_),
                                ..
                            }
                        )
                }) {
                    block.command = None;
                    let old_text = block
                        .entries
                        .first()
                        .and_then(|entry| {
                            if let ThreadItem::AgentMessage { text, .. } = &entry.item {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default();
                    if block.stream.is_none() || !text.starts_with(old_text) {
                        block.stream = None;
                        block.committed.clear();
                    }
                    let delta = if text.starts_with(old_text) && !block.entries.is_empty() {
                        &text[old_text.len()..]
                    } else {
                        text
                    };
                    let stream = block.stream.get_or_insert_with(|| {
                        StreamController::new_with_inline_visualizations(
                            /*width*/ None,
                            origin.cwd.as_path(),
                            crate::history_cell::HistoryRenderMode::Rich,
                            /*inline_visualization_context*/ None,
                        )
                    });
                    stream.push(delta);
                    if let (Some(cell), _) = stream.on_commit_tick_batch(usize::MAX) {
                        block.committed.push(Arc::from(cell));
                    }
                    block.cells = block.committed.clone();
                    let tail = stream.current_tail_lines();
                    if !tail.is_empty() {
                        block.cells.push(Arc::new(StreamingAgentTailCell::new(
                            tail,
                            stream.tail_starts_stream(),
                        )));
                    }
                } else {
                    block.command = None;
                    block.stream = None;
                    block.committed.clear();
                    block.cells = crate::thread_transcript::project_items(
                        codex_protocol::ThreadId::from_string(&origin.child_thread_id).ok(),
                        &origin.cwd,
                        source.iter().map(|entry| entry.item.clone()),
                        if config.show_raw_agent_reasoning {
                            RawReasoningVisibility::Visible
                        } else {
                            RawReasoningVisibility::Hidden
                        },
                        Some(config),
                        options,
                    );
                }
                block.entries = source.iter().map(|entry| (*entry).clone()).collect();
                self.blocks.push(block);
            }
            offset = end;
        }
        self.blocks
            .iter()
            .flat_map(|block| block.cells.iter().cloned())
            .collect()
    }
}
