//! Overview renders structured values and Markdown; source and normal transcript are opt-in.
use super::*;
use ratatui::style::Stylize;
use ratatui::text::Line;
#[path = "details_phase.rs"]
mod phase;
#[path = "details_saved.rs"]
mod saved;

fn markdown(source: &str, width: u16, lines: &mut Vec<Line<'static>>) {
    crate::markdown::append_markdown(source, Some(usize::from(width.max(1))), None, lines);
}

fn scalar_field(label: &str, value: &str, width: u16, lines: &mut Vec<Line<'static>>) {
    lines.extend(crate::structured_value::render_field(label, value, width));
}

pub(super) fn lines(state: &WorkflowState, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(error) = state.error.as_ref().or(state.load_error.as_ref()) {
        lines.push(error.clone().red().into());
    }
    if state.center.saved {
        saved::lines(state, width, &mut lines);
        return crate::wrapping::word_wrap_lines(lines, usize::from(width.max(1)));
    }
    match state.center.detail_tab {
        DetailTab::Overview => {
            let (overview, fallback, _) = state.selected_overview();
            if let Some(overview) = overview {
                lines.push(overview.title.bold().into());
                for (label, value) in overview.metadata {
                    scalar_field(&label, &value, width, &mut lines);
                }
                if let Some(metrics) = overview.metrics.as_ref() {
                    for (label, value) in super::display::overview_metrics(metrics) {
                        scalar_field(&label, &value, width, &mut lines);
                    }
                }
                if let Some(error) = overview.refresh_error {
                    lines.extend(crate::wrapping::word_wrap_lines(
                        [Line::from(error.red())],
                        usize::from(width.max(1)),
                    ));
                }
                if state
                    .child
                    .and_then(|index| state.rows.get(state.selected)?.children.get(index))
                    .is_some_and(|child| child.key.starts_with("phase:"))
                {
                    lines.push(Line::default());
                    lines.extend(phase::lines(&overview.phase_steps, width));
                }
                if state.child.is_none()
                    && let Some(row) = state.rows.get(state.selected)
                {
                    if row.read_only {
                        let reason = row
                            .read_only_reason
                            .as_deref()
                            .unwrap_or("Legacy workflow · read-only");
                        if state.error.as_deref().or(state.load_error.as_deref()) != Some(reason) {
                            lines.extend([Line::default(), reason.to_owned().dim().into()]);
                        }
                    }
                    if let Some(metadata) = &row.metadata {
                        markdown(&metadata.description, width, &mut lines);
                    }
                    if let Some(metrics) = &row.metrics {
                        for (label, value) in super::display::overview_metrics(metrics) {
                            scalar_field(&label, &value, width, &mut lines);
                        }
                    }
                    if !row.phase_progress.is_empty() {
                        lines.extend([Line::default(), "Phases".bold().into()]);
                        for phase in &row.phase_progress {
                            scalar_field(
                                &phase.title,
                                super::projection::phase_status(phase.status),
                                width,
                                &mut lines,
                            );
                        }
                    }
                }
                if let Some(error) = overview.error {
                    lines.extend([Line::default(), "Error".red().bold().into()]);
                    markdown(&error, width, &mut lines);
                }
                if let Some(prompt) = overview.prompt {
                    lines.extend([Line::default(), "Prompt".bold().into()]);
                    markdown(&prompt, width, &mut lines);
                }
                if let Some(input) = overview.input {
                    lines.extend([Line::default(), "Input".bold().into()]);
                    lines.extend(crate::structured_value::render_value(&input, width));
                }
                if let Some(result) = overview.result {
                    lines.extend([
                        Line::default(),
                        overview
                            .result_label
                            .unwrap_or_else(|| "Result".into())
                            .bold()
                            .into(),
                    ]);
                    lines.extend(crate::structured_value::render_value(&result, width));
                }
                lines.extend(fallback.into_iter().map(|line| Line::from(line.dim())));
                if state.child.is_none()
                    && let Some(row) = state.rows.get(state.selected)
                    && !row.logs.is_empty()
                {
                    lines.extend([Line::default(), "Log".bold().into()]);
                    for entry in &row.logs {
                        let label = entry.phase.as_ref().map_or_else(
                            || format!("#{}", entry.sequence),
                            |phase| format!("#{} {phase}", entry.sequence),
                        );
                        scalar_field(&label, &entry.message, width, &mut lines);
                    }
                }
            } else {
                lines.extend(fallback.into_iter().map(Line::from));
            }
        }
        DetailTab::Source => {
            let (_, _, source) = state.selected_overview();
            if source.is_empty() {
                lines.push(
                    "Press o to open the owning conversation and load this workflow’s source."
                        .dim()
                        .into(),
                );
            } else {
                markdown(&format!("```javascript\n{source}\n```"), width, &mut lines);
            }
        }
        DetailTab::Transcript => {
            if let Some(target) = state.selected_transcript_target() {
                if let Some(transcript) = state.transcripts.get(&target.cache_key()) {
                    if let Some(error) = &transcript.error {
                        lines.push(error.clone().red().into());
                    }
                    if let Some(limitation) = &transcript.limitation {
                        lines.push(limitation.clone().dim().into());
                    }
                    if transcript.loading {
                        lines.push("Loading transcript…".dim().into());
                    }
                    for cell in &transcript.cells {
                        lines.extend(cell.display_lines(width));
                        lines.push(Line::default());
                    }
                    if transcript.next_cursor.is_some() {
                        lines.push("p older items · R refresh".dim().into());
                    }
                    if !transcript.loading && transcript.cells.is_empty() {
                        lines.push("No transcript items yet · R refresh".dim().into());
                    }
                } else {
                    lines.push("Loading transcript…".dim().into());
                }
            } else {
                lines.push(
                    "Select an agent attempt to inspect its transcript"
                        .dim()
                        .into(),
                );
            }
        }
    }
    crate::wrapping::word_wrap_lines(lines, usize::from(width.max(1)))
}
