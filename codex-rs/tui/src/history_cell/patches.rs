//! Patch summaries and image-tool transcript helpers.

use super::*;
use codex_utils_path_uri::LegacyAppPathString;

#[derive(Debug)]
pub(crate) struct PatchHistoryCell {
    changes: HashMap<PathBuf, FileChange>,
    cwd: PathBuf,
}

impl HistoryCell for PatchHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        create_diff_summary(&self.changes, &self.cwd, width as usize)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(create_diff_summary(
            &self.changes,
            &self.cwd,
            RAW_DIFF_SUMMARY_WIDTH,
        ))
    }
}
/// Create a new `PendingPatch` cell that lists the file‑level summary of
/// a proposed patch. The summary lines should already be formatted (e.g.
/// "A path/to/file.rs").
pub(crate) fn new_patch_event(
    changes: HashMap<PathBuf, FileChange>,
    cwd: &Path,
) -> PatchHistoryCell {
    PatchHistoryCell {
        changes,
        cwd: cwd.to_path_buf(),
    }
}

pub(crate) fn new_patch_apply_failure(stderr: String) -> PlainHistoryCell {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Failure title
    lines.push(Line::from("✘ Failed to apply patch".magenta().bold()));

    if !stderr.trim().is_empty() {
        let output = output_lines(
            Some(&CommandOutput::new(/*exit_code*/ 1, stderr)),
            OutputLinesParams {
                line_limit: TOOL_CALL_MAX_LINES,
                only_err: true,
                include_angle_pipe: true,
                include_prefix: true,
            },
        );
        lines.extend(output.lines);
    }

    PlainHistoryCell { lines }
}

fn display_image_path(path: LegacyAppPathString, cwd: &Path) -> String {
    path.to_inferred_path_uri()
        .and_then(|path| path.to_abs_path().ok())
        .map(|path| display_path_for(path.as_path(), cwd))
        .unwrap_or_else(|| path.into_string())
}

#[derive(Debug)]
pub(crate) struct ViewImageCell {
    display_paths: Vec<String>,
}

impl ViewImageCell {
    pub(crate) fn add_path(&mut self, path: LegacyAppPathString, cwd: &Path) {
        self.display_paths.push(display_image_path(path, cwd));
    }

    fn header(&self) -> &'static str {
        if self.display_paths.len() == 1 {
            "Viewed Image"
        } else {
            "Viewed Images"
        }
    }
}

impl HistoryCell for ViewImageCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = vec![vec!["• ".dim(), self.header().bold()].into()];
        let mut path_lines = Vec::new();
        for path in &self.display_paths {
            let path_line = Line::from(path.clone()).dim();
            let wrapped =
                adaptive_wrap_line(&path_line, RtOptions::new(width.saturating_sub(4) as usize));
            push_owned_lines(&wrapped, &mut path_lines);
        }
        lines.extend(prefix_lines(path_lines, "  └ ".dim(), "    ".into()));
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![vec!["• ".into(), self.header().into()].into()];
        lines.extend(prefix_lines(
            self.display_paths.iter().cloned().map(Line::from).collect(),
            "  └ ".into(),
            "    ".into(),
        ));
        lines
    }
}

pub(crate) fn new_view_image_tool_call(path: LegacyAppPathString, cwd: &Path) -> ViewImageCell {
    ViewImageCell {
        display_paths: vec![display_image_path(path, cwd)],
    }
}

pub(crate) fn new_image_generation_call(
    call_id: String,
    status: &str,
    revised_prompt: Option<String>,
    saved_path: Option<AbsolutePathBuf>,
) -> PlainHistoryCell {
    let detail = revised_prompt.unwrap_or(call_id);
    let heading = if status == "failed" {
        vec!["✗ ".red().bold(), "Image generation failed".bold()].into()
    } else {
        vec!["• ".dim(), "Generated Image:".bold()].into()
    };
    let mut lines: Vec<Line<'static>> = vec![heading, vec!["  └ ".dim(), detail.dim()].into()];
    if let Some(saved_path) = saved_path {
        let saved_path = Url::from_file_path(saved_path.as_path())
            .map(|url| url.to_string())
            .unwrap_or_else(|_| saved_path.display().to_string());
        lines.push(vec!["  └ ".dim(), "Saved to: ".dim(), saved_path.into()].into());
    }

    PlainHistoryCell { lines }
}
