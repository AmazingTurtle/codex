//! Compact phase membership uses resolved attempt metadata, never the parent's current model.
use super::*;

pub(super) fn lines(steps: &[WorkflowPhaseStep], width: u16) -> Vec<Line<'static>> {
    if steps.is_empty() {
        return vec!["No agent steps discovered in this phase".dim().into()];
    }
    let mut lines = Vec::new();
    if width >= 70 {
        let label_width = (usize::from(width) / 5).max(12);
        let model_width = 18;
        let reasoning_width = 10;
        let status_width = 11;
        let prompt_width = usize::from(width)
            .saturating_sub(label_width + model_width + reasoning_width + status_width + 8)
            .max(4);
        let clip = |value: &str, budget| {
            crate::line_truncation::truncate_line_with_ellipsis_if_overflow(
                Line::from(value.to_owned()),
                budget,
            )
            .to_string()
        };
        lines.push(format!("{:<label_width$}  {:<model_width$}  {:<reasoning_width$}  {:<prompt_width$}  Status","Step","Model","Reasoning","Prompt").dim().into());
        for step in steps {
            // Pad by terminal cells after clipping; Rust's string padding counts characters.
            let field = |value: &str, budget| {
                let text = clip(value, budget);
                format!(
                    "{text}{}",
                    " ".repeat(budget.saturating_sub(crate::width::display_width(&text)))
                )
            };
            lines.push(
                format!(
                    "{}  {}  {}  {}  {}",
                    field(&step.label, label_width),
                    field(step.model.as_deref().unwrap_or("Unknown"), model_width),
                    field(
                        step.reasoning.as_deref().unwrap_or("Unknown"),
                        reasoning_width
                    ),
                    field(
                        &step.prompt.split_whitespace().collect::<Vec<_>>().join(" "),
                        prompt_width
                    ),
                    step.status
                )
                .into(),
            );
        }
    } else {
        for step in steps {
            lines.push(
                vec![
                    step.label.clone().bold(),
                    format!(" · {}", step.status).dim(),
                ]
                .into(),
            );
            let context = format!(
                "{} · {}",
                step.model.as_deref().unwrap_or("Model unknown"),
                step.reasoning.as_deref().unwrap_or("Reasoning unknown")
            );
            lines.extend(crate::wrapping::word_wrap_lines(
                [Line::from(context.dim())],
                usize::from(width.max(1)),
            ));
            lines.push(
                crate::line_truncation::truncate_line_with_ellipsis_if_overflow(
                    Line::from(step.prompt.split_whitespace().collect::<Vec<_>>().join(" ")),
                    usize::from(width),
                ),
            );
            lines.push(Line::default());
        }
    }
    lines
}
