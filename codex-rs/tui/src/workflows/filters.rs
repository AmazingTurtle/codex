//! Run counts and list filtering share the Agent command center's status vocabulary.
use super::*;

pub(super) const RUN_FILTERS: [&str; 5] = ["All", "Needs you", "Working", "Completed", "Inactive"];
pub(super) const LIBRARY_FILTERS: [&str; 3] = ["All", "Project", "User"];

impl WorkflowState {
    pub(super) fn row_status<'a>(&self, row: &'a WorkflowRow) -> &'a str {
        if row.owner == self.owner && matches!(row.status.as_str(), "Pending" | "Running") {
            execution_label(self.execution).unwrap_or(&row.status)
        } else {
            &row.status
        }
    }

    pub(super) fn status_group(&self, row: &WorkflowRow) -> usize {
        match self.row_status(row) {
            "Failed" | "Interrupted" | "Paused" => 1,
            "Pending" | "Running" | "Pausing" => 2,
            "Completed" => 3,
            _ => 4,
        }
    }

    pub(crate) fn visible_run_indices(&self) -> Vec<usize> {
        let mut indices: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                self.center.status_filter == 0
                    || self.status_group(row) == self.center.status_filter
            })
            .map(|(index, _)| index)
            .collect();
        indices.sort_by_key(|index| self.group_key(&self.rows[*index]));
        indices
    }

    pub(super) fn group_key(&self, row: &WorkflowRow) -> String {
        match self.center.grouping {
            WorkflowGrouping::Conversation => {
                row.owner.map(|id| id.to_string()).unwrap_or_default()
            }
            WorkflowGrouping::Status => self.status_group(row).to_string(),
            WorkflowGrouping::Project => row.project_root.clone().unwrap_or_default(),
        }
    }

    pub(super) fn group_label(&self, row: &WorkflowRow) -> String {
        match self.center.grouping {
            WorkflowGrouping::Conversation if row.owner == self.owner => {
                "Current conversation".into()
            }
            WorkflowGrouping::Conversation => {
                let id = row.owner.map(|owner| owner.to_string()).unwrap_or_default();
                format!(
                    "Conversation · {}",
                    id.get(id.len().saturating_sub(8)..).unwrap_or(&id)
                )
            }
            WorkflowGrouping::Status => RUN_FILTERS[self.status_group(row)].into(),
            WorkflowGrouping::Project => row
                .project_root
                .clone()
                .unwrap_or_else(|| "No project".into()),
        }
    }

    pub(super) fn visible_saved_indices(&self) -> Vec<usize> {
        self.saved
            .iter()
            .enumerate()
            .filter(|(_, saved)| match self.center.library_filter {
                1 => saved.scope == Some(codex_app_server_protocol::WorkflowScope::Project),
                2 => saved.scope == Some(codex_app_server_protocol::WorkflowScope::User),
                _ => true,
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub(super) fn cycle_filter(&mut self, forward: bool) {
        if self.center.saved {
            self.center.library_filter = (self.center.library_filter
                + if forward {
                    1
                } else {
                    LIBRARY_FILTERS.len() - 1
                })
                % LIBRARY_FILTERS.len();
            if !self
                .visible_saved_indices()
                .contains(&self.selected_saved_index())
            {
                self.center.saved_id = self
                    .visible_saved_indices()
                    .first()
                    .map(|index| self.saved[*index].id.clone());
            }
        } else {
            self.center.status_filter = (self.center.status_filter
                + if forward { 1 } else { RUN_FILTERS.len() - 1 })
                % RUN_FILTERS.len();
            self.reconcile_visible_selection();
        }
    }

    pub(crate) fn reconcile_saved_selection(&mut self) {
        let visible = self.visible_saved_indices();
        if !visible
            .iter()
            .any(|index| self.center.saved_id.as_ref() == Some(&self.saved[*index].id))
        {
            self.center.saved_id = visible.first().map(|index| self.saved[*index].id.clone());
        }
    }

    pub(super) fn selected_requires_owner_open(&self) -> bool {
        !self.center.saved
            && self.visible_run_indices().contains(&self.selected)
            && self.rows.get(self.selected).is_some_and(|row| {
                row.source.is_empty() && row.owner.is_some() && row.owner != self.owner
            })
    }

    pub(crate) fn reconcile_visible_selection(&mut self) {
        let visible = self.visible_run_indices();
        if !visible.contains(&self.selected) {
            self.selected = visible.first().copied().unwrap_or(0);
            self.child = None;
        }
        self.reconcile_attempt_history_selection();
    }
}

impl WorkflowGrouping {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Conversation => "Conversation",
            Self::Status => "Status",
            Self::Project => "Project",
        }
    }
    pub(super) fn next(self) -> Self {
        match self {
            Self::Conversation => Self::Status,
            Self::Status => Self::Project,
            Self::Project => Self::Conversation,
        }
    }
}
