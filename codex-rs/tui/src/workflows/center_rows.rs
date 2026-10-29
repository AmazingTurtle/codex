//! Worker settings share the existing list space and full-row selection.
use super::columns::columns;
use super::*;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;

enum Entry {
    Group(String),
    Node(usize),
    Saved(usize),
}

fn node_style(state: &WorkflowState, row: usize, child: Option<usize>, selected: bool) -> Style {
    let base = if selected {
        crate::bottom_pane::selection_style()
    } else {
        Style::default()
    };
    let Some(child) = child.and_then(|index| state.rows[row].children.get(index)) else {
        return base.bold();
    };
    if !child.key.starts_with("step:") && child.parent_step_key.is_none() {
        return base;
    }
    let step_key = child.parent_step_key.as_ref().unwrap_or(&child.key);
    let step_id = step_key.strip_prefix("step:").unwrap_or(step_key);
    let style = base.patch(super::super::agent_style(
        &state.rows[row].id,
        step_id,
        base.bg,
    ));
    if child.parent_step_key.is_some() {
        style.dim()
    } else {
        style
    }
}

pub(super) fn render(state: &mut WorkflowState, area: Rect, buf: &mut Buffer) {
    if area.is_empty() {
        return;
    }
    let headings = columns(Rect { height: 1, ..area }, state.center.saved);
    render::line(
        if state.center.saved {
            "Workflow"
        } else {
            "Workflow / step / attempt"
        }
        .dim()
        .into(),
        headings.title,
        buf,
    );
    render::line(
        if headings.combined_settings {
            "Model / reasoning"
        } else {
            "Model"
        }
        .dim()
        .into(),
        headings.model,
        buf,
    );
    render::line("Reasoning".dim().into(), headings.reasoning, buf);
    render::line("Status".dim().into(), headings.status, buf);
    render::line("Updated".dim().into(), headings.updated, buf);
    let viewport = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    state.center.page_height = usize::from(viewport.height);
    let nodes = state.nodes();
    let mut entries = Vec::new();
    let mut group = None;
    if state.center.saved {
        entries.extend(state.visible_saved_indices().into_iter().map(Entry::Saved));
    } else {
        for (index, node) in nodes.iter().enumerate() {
            let row = &state.rows[node.row];
            let next = state.group_key(row);
            if node.child.is_none() && group.as_ref() != Some(&next) {
                entries.push(Entry::Group(state.group_label(row)));
                group = Some(next);
            }
            entries.push(Entry::Node(index));
        }
    }
    let selected = entries
        .iter()
        .position(|entry| match entry {
            Entry::Node(index) => {
                nodes[*index].row == state.selected && nodes[*index].child == state.child
            }
            Entry::Saved(index) => *index == state.selected_saved_index(),
            Entry::Group(_) => false,
        })
        .unwrap_or(0);
    let height = usize::from(viewport.height);
    let mut offset = state
        .center
        .list_scroll
        .min(entries.len().saturating_sub(height));
    if selected < offset {
        offset = selected;
    }
    if selected >= offset + height {
        offset = selected.saturating_add(1).saturating_sub(height);
    }
    if height > 1
        && offset > 0
        && selected == offset
        && matches!(entries.get(offset - 1), Some(Entry::Group(_)))
    {
        offset -= 1;
    }
    state.center.list_scroll = offset;
    let now = chrono::Utc::now();
    for (position, entry) in entries.iter().enumerate().skip(offset).take(height) {
        let area = Rect::new(
            viewport.x,
            viewport.y + (position - offset) as u16,
            viewport.width,
            1,
        );
        if let Entry::Group(label) = entry {
            render::line(label.clone().dim().bold().into(), area, buf);
            continue;
        }
        let selected = position == selected;
        let style = if selected {
            crate::bottom_pane::selection_style()
        } else {
            Style::default()
        };
        buf.set_style(area, style);
        let columns = columns(area, state.center.saved);
        let (label, status, depth, updated) = match entry {
            Entry::Node(index) => {
                let node = &nodes[*index];
                (
                    &node.label,
                    node.status.clone(),
                    node.depth,
                    node.child.map_or(state.rows[node.row].updated_at, |index| {
                        state.rows[node.row].children[index].updated_at.unwrap_or(0)
                    }),
                )
            }
            Entry::Saved(index) => (
                state.saved[*index]
                    .metadata
                    .as_ref()
                    .map_or(&state.saved[*index].name, |metadata| &metadata.name),
                state.saved[*index]
                    .scope
                    .map_or_else(String::new, |scope| format!("{scope:?}")),
                0,
                0,
            ),
            Entry::Group(_) => continue,
        };
        let suffix = if columns.status.is_empty() && !status.is_empty() {
            format!(" · {status}")
        } else {
            String::new()
        };
        let badge = match entry {
            Entry::Node(index)
                if nodes[*index].child.is_none()
                    && state
                        .hidden_from_strip
                        .contains(&state.rows[nodes[*index].row].id) =>
            {
                " [footer hidden]"
            }
            Entry::Node(index)
                if nodes[*index].child.is_none() && state.rows[nodes[*index].row].read_only =>
            {
                " [legacy]"
            }
            Entry::Saved(index) if state.saved[*index].read_only => " [read-only]",
            _ => "",
        };
        let title_style = match entry {
            Entry::Node(index) => {
                node_style(state, nodes[*index].row, nodes[*index].child, selected)
            }
            Entry::Saved(_) => style,
            Entry::Group(_) => continue,
        };
        let status_style = if selected {
            style
        } else {
            match status.as_str() {
                "Failed" => crate::style::status_style(crate::style::StatusTone::Failure),
                "Paused" | "Interrupted" => {
                    crate::style::status_style(crate::style::StatusTone::Attention)
                }
                "Completed" => crate::style::status_style(crate::style::StatusTone::Success),
                "Running" | "Pending" | "Pausing" | "Active" => crate::style::accent_style(),
                _ => crate::style::secondary_text_style(),
            }
        };
        render::line(
            Line::from(vec![
                Span::styled(
                    format!(
                        "{}{}{label}{badge}",
                        if selected { "› " } else { "  " },
                        "  ".repeat(usize::from(depth))
                    ),
                    title_style,
                ),
                Span::styled(suffix, status_style),
            ])
            .style(style),
            columns.title,
            buf,
        );
        render::line(Line::from(status).style(status_style), columns.status, buf);
        if let Entry::Node(index) = entry
            && let Some(child) = nodes[*index]
                .child
                .and_then(|child_index| state.rows[nodes[*index].row].children.get(child_index))
            && matches!(
                &child.transcript_target,
                Some(
                    codex_app_server_protocol::WorkflowTranscriptTarget::Step { .. }
                        | codex_app_server_protocol::WorkflowTranscriptTarget::Attempt { .. }
                )
            )
        {
            let model = child.resolved_model.as_deref().unwrap_or("Unknown");
            let reasoning = child.reasoning_effort.as_deref().unwrap_or("Unknown");
            let model = if columns.combined_settings {
                let suffix = format!(" / {reasoning}");
                let mut model = crate::line_truncation::truncate_line_with_ellipsis_if_overflow(
                    Line::from(model.to_owned()),
                    usize::from(columns.model.width)
                        .saturating_sub(crate::width::display_width(&suffix)),
                );
                model.spans.push(suffix.into());
                model
            } else {
                Line::from(model.to_owned())
            };
            render::line(model.style(title_style), columns.model, buf);
            render::line(
                Line::from(reasoning.to_owned()).style(title_style),
                columns.reasoning,
                buf,
            );
        }
        let timestamp = (updated > 0)
            .then(|| chrono::DateTime::from_timestamp(updated, 0))
            .flatten();
        render::line(
            Line::from(crate::resume_picker::format_relative_time(now, timestamp)).style(
                if selected {
                    style
                } else {
                    crate::style::secondary_text_style()
                },
            ),
            columns.updated,
            buf,
        );
    }
    if entries.is_empty() {
        render::line(
            if state.loading {
                "Loading workflows…"
            } else {
                "No matching workflows"
            }
            .dim()
            .into(),
            viewport,
            buf,
        );
    }
}
