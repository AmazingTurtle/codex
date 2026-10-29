//! Durable workflow events are human history, independent of assistant turns.
use super::HistoryCell;
use codex_app_server_protocol::WorkflowLifecycleEvent;
use codex_app_server_protocol::WorkflowLifecycleKind;
use codex_app_server_protocol::WorkflowResult;
use codex_app_server_protocol::WorkflowStatus;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;

#[derive(Debug, Clone)]
pub(crate) struct WorkflowLifecycleHistoryCell {
    events: Vec<WorkflowLifecycleEvent>,
}

impl WorkflowLifecycleHistoryCell {
    pub(crate) fn new(event: WorkflowLifecycleEvent) -> Self {
        Self {
            events: vec![event],
        }
    }

    pub(crate) fn is_completed_task(&self) -> bool {
        matches!(
            self.events[0].kind,
            WorkflowLifecycleKind::StepCompleted {
                status: WorkflowStatus::Succeeded,
                error: None,
                ..
            }
        )
    }

    /// Presentation is bounded; every retained fact keeps its original identity and order.
    pub(crate) fn append(&mut self, newer: &Self) -> bool {
        let first = &self.events[0];
        let next = &newer.events[0];
        if !self.is_completed_task()
            || !newer.is_completed_task()
            || self.events.len() + newer.events.len() > 64
            || first.owner_thread_id != next.owner_thread_id
            || first.run_id != next.run_id
            || first.execution_id != next.execution_id
            || self
                .events
                .last()
                .is_none_or(|last| last.sequence >= next.sequence)
        {
            return false;
        }
        self.events.extend(newer.events.iter().cloned());
        true
    }

    pub(crate) fn event_ids(&self) -> impl Iterator<Item = &str> {
        self.events.iter().map(|event| event.id.as_str())
    }

    /// Page boundaries may join only when the canonical timeline proves fact adjacency.
    pub(crate) fn append_from_timeline(
        &mut self,
        newer: &Self,
        entries: &[codex_app_server_protocol::ThreadTimelineEntry],
    ) -> bool {
        use codex_app_server_protocol::ThreadTimelineEntry;
        let ids = self
            .event_ids()
            .chain(newer.event_ids())
            .collect::<Vec<_>>();
        let Some(start) = entries.iter().position(|entry| {
            matches!(entry, ThreadTimelineEntry::Workflow { event, .. } if event.id == ids[0])
        }) else {
            return false;
        };
        if !entries[start..].iter().take(ids.len()).zip(&ids).all(|(entry, id)| {
            matches!(entry, ThreadTimelineEntry::Workflow { event, .. } if event.id == *id)
        }) || entries.len() - start < ids.len() {
            return false;
        }
        self.append(newer)
    }

    fn heading(&self) -> Line<'static> {
        Self::event_heading(&self.events[0])
    }

    fn event_heading(event: &WorkflowLifecycleEvent) -> Line<'static> {
        let workflow = &event.workflow_name;
        let mut spans = vec!["• ".into()];
        spans.extend(match &event.kind {
            WorkflowLifecycleKind::Started { .. } => {
                vec!["Started workflow ".bold(), name(workflow)]
            }
            WorkflowLifecycleKind::StepCompleted {
                label,
                status,
                active_duration_ms,
                ..
            } => {
                let mut spans = vec![
                    format!("{} task ", verb(*status)).bold(),
                    name(label),
                    " in workflow ".bold(),
                    name(workflow),
                ];
                spans.extend(duration(*active_duration_ms));
                spans
            }
            WorkflowLifecycleKind::StepSkipped { label, .. } => vec![
                "Skipped task ".bold(),
                name(label),
                " in workflow ".bold(),
                name(workflow),
            ],
            WorkflowLifecycleKind::Completed { status, .. } => vec![
                format!("{} workflow ", verb(*status)).bold(),
                name(workflow),
            ],
            WorkflowLifecycleKind::Interrupted { .. } => {
                vec!["Interrupted workflow ".bold(), name(workflow)]
            }
            WorkflowLifecycleKind::ResultShown { .. } => {
                vec!["Result of workflow ".bold(), name(workflow)]
            }
        });
        spans.into()
    }

    fn output(&self) -> Option<&WorkflowResult> {
        match &self.events[0].kind {
            WorkflowLifecycleKind::Completed { result, .. }
            | WorkflowLifecycleKind::ResultShown { result, .. } => Some(result),
            WorkflowLifecycleKind::Started { .. }
            | WorkflowLifecycleKind::StepCompleted { .. }
            | WorkflowLifecycleKind::StepSkipped { .. }
            | WorkflowLifecycleKind::Interrupted { .. } => None,
        }
    }

    fn error(&self) -> Option<&str> {
        match &self.events[0].kind {
            WorkflowLifecycleKind::StepCompleted { error, .. }
            | WorkflowLifecycleKind::Completed { error, .. }
            | WorkflowLifecycleKind::ResultShown { error, .. } => error.as_deref(),
            WorkflowLifecycleKind::Interrupted { reason, .. } => Some(reason),
            WorkflowLifecycleKind::Started { .. } | WorkflowLifecycleKind::StepSkipped { .. } => {
                None
            }
        }
    }
}

fn name(text: &str) -> Span<'static> {
    Span::from(text.to_owned())
        .fg(crate::style::accent_color())
        .not_bold()
}

fn duration(active_duration_ms: Option<u64>) -> Vec<Span<'static>> {
    active_duration_ms.map_or_else(Vec::new, |milliseconds| {
        let elapsed = crate::status_indicator_widget::fmt_elapsed_compact(milliseconds / 1000);
        vec![" after ".bold(), elapsed.not_bold()]
    })
}

fn verb(status: WorkflowStatus) -> &'static str {
    match status {
        WorkflowStatus::Succeeded => "Completed",
        WorkflowStatus::Failed => "Failed",
        WorkflowStatus::Stopped => "Stopped",
        WorkflowStatus::Interrupted => "Interrupted",
        WorkflowStatus::Pending => "Pending",
        WorkflowStatus::Running => "Running",
    }
}

impl HistoryCell for WorkflowLifecycleHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = if self.events.len() == 1 {
            vec![self.heading()]
        } else {
            let mut lines = vec![
                vec![
                    "• ".into(),
                    "Completed tasks".bold(),
                    " in workflow ".bold(),
                    name(&self.events[0].workflow_name),
                ]
                .into(),
            ];
            for event in &self.events {
                if let WorkflowLifecycleKind::StepCompleted {
                    label,
                    active_duration_ms,
                    ..
                } = &event.kind
                {
                    let mut spans = vec!["  └ ".dim(), name(label)];
                    spans.extend(duration(*active_duration_ms));
                    lines.push(spans.into());
                }
            }
            lines
        };
        if let Some(error) = self.error() {
            lines.extend(crate::wrapping::word_wrap_lines(
                vec![Line::from(error.to_string().red())],
                usize::from(width.max(1)),
            ));
        }
        if let Some(WorkflowResult::Value { value }) = self.output() {
            lines.push(Line::default());
            lines.push(Line::from("Result".bold()));
            lines.extend(crate::structured_value::render_value(value, width));
        }
        crate::wrapping::word_wrap_lines(lines, usize::from(width.max(1)))
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = self
            .events
            .iter()
            .map(|event| Line::from(Self::event_heading(event).to_string()))
            .collect::<Vec<_>>();
        if let Some(error) = self.error() {
            lines.push(Line::from(error.to_string()));
        }
        if let Some(WorkflowResult::Value { value }) = self.output() {
            lines.extend(
                serde_json::to_string_pretty(value)
                    .unwrap_or_default()
                    .lines()
                    .map(|line| Line::from(line.to_string())),
            );
        }
        lines
    }
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "workflow_group_tests.rs"]
mod group_tests;
