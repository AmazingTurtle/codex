//! Saved scripts have metadata and explicit source inspection, never a fabricated transcript.
use super::*;

pub(super) fn lines(state: &WorkflowState, width: u16, lines: &mut Vec<Line<'static>>) {
    if state.catalog_overflow {
        lines.push(Line::from("catalog limit reached; use /workflows").style(
            crate::style::status_style(crate::style::StatusTone::Attention),
        ));
    }
    if !state
        .visible_saved_indices()
        .contains(&state.selected_saved_index())
    {
        return;
    }
    let Some(saved) = state.saved.get(state.selected_saved_index()) else {
        return;
    };
    match state.center.saved_detail_tab {
        DetailTab::Overview => {
            let title = saved
                .metadata
                .as_ref()
                .map_or(&saved.name, |metadata| &metadata.name);
            lines.push(title.clone().bold().into());
            if title != &saved.name {
                scalar_field("Saved as", &saved.name, width, lines);
            }
            lines.extend(saved.description.iter().cloned().map(Line::from));
            lines.extend(
                saved
                    .command_notice
                    .iter()
                    .cloned()
                    .map(|notice| notice.dim().into()),
            );
            if saved.read_only {
                lines.push(
                    saved
                        .read_only_reason
                        .clone()
                        .unwrap_or_else(|| "Legacy saved workflow · read-only".into())
                        .dim()
                        .into(),
                );
                lines.push("3 Source to inspect this script".dim().into());
            } else {
                lines.push("Enter to provide input and run".dim().into());
            }
        }
        DetailTab::Source => {
            if let Some(source) = state.saved_sources.get(&saved.id) {
                if let Some(error) = &source.error {
                    lines.push(error.clone().red().into());
                }
                if source.loading {
                    lines.push("Loading saved source…".dim().into());
                }
                if let Some(source) = &source.source {
                    markdown(&format!("```javascript\n{source}\n```"), width, lines);
                }
            } else {
                lines.push("Loading saved source…".dim().into());
            }
        }
        DetailTab::Transcript => {
            lines.push(
                "Saved scripts have no transcript. Choose a workflow run to inspect its agents."
                    .dim()
                    .into(),
            );
        }
    }
}
