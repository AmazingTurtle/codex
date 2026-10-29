//! Workflow JSON uses compact literal Markdown without changing its original copy source.
use ratatui::style::Stylize;
use ratatui::text::Line;
use serde_json::Value;
use std::borrow::Cow;

mod compact_markdown;

fn markdown(text: &str, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    crate::markdown::append_markdown(text, Some(usize::from(width.max(1))), None, &mut lines);
    lines
}

/// Render ordinary workflow metadata with a display-cell-aligned continuation.
pub(crate) fn render_field(label: &str, value: &str, width: u16) -> Vec<Line<'static>> {
    let prefix = format!("{label}: ");
    let prefix_width = crate::width::display_width(&prefix);
    if prefix_width >= usize::from(width.max(1)) || value.contains('\n') {
        let mut lines = crate::wrapping::word_wrap_lines(
            [Line::from(format!("{label}:").dim())],
            usize::from(width.max(1)),
        );
        let padding = " ".repeat(usize::from(width.saturating_sub(1).min(2)));
        lines.extend(
            markdown(value, width.saturating_sub(2))
                .into_iter()
                .map(|mut line| {
                    line.spans.insert(0, padding.clone().into());
                    line
                }),
        );
        return lines;
    }
    let mut content = markdown(value, width.saturating_sub(prefix_width as u16));
    if content.is_empty() {
        content.push(Line::default());
    }
    for (index, line) in content.iter_mut().enumerate() {
        line.spans.insert(
            0,
            if index == 0 {
                prefix.clone().dim()
            } else {
                " ".repeat(prefix_width).into()
            },
        );
    }
    content
}

/// Share the same source with native agent-message cells and workflow detail rendering.
/// Callers own the Outcome/Result caption. Typed strings stay ordinary Markdown.
pub(crate) fn value_markdown(value: &Value) -> Cow<'_, str> {
    match value {
        Value::String(text) => Cow::Borrowed(text),
        _ => Cow::Owned(compact_markdown::format(
            value,
            compact_markdown::Options {
                title: None,
                ..Default::default()
            },
        )),
    }
}

/// Render an immutable value without changing its original answer or model context.
pub(crate) fn render_value(value: &Value, width: u16) -> Vec<Line<'static>> {
    markdown(&value_markdown(value), width)
}

#[cfg(test)]
#[path = "structured_value_tests.rs"]
mod tests;
