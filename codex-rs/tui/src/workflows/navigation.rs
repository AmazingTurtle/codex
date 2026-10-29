//! A single stable-key sequence makes every visible workflow node arrow-reachable.
use super::*;

#[derive(Clone)]
pub(super) struct TreeNode {
    pub(super) row: usize,
    pub(super) child: Option<usize>,
    pub(super) key: String,
    pub(super) label: String,
    pub(super) depth: u16,
    pub(super) status: String,
}

impl WorkflowState {
    pub(super) fn nodes(&self) -> Vec<TreeNode> {
        let show_history = self.center.show_attempt_history;
        self.visible_run_indices()
            .into_iter()
            .flat_map(|index| {
                let row = &self.rows[index];
                std::iter::once(TreeNode {
                    row: index,
                    child: None,
                    key: format!("run:{}", row.id),
                    label: row.title.clone(),
                    depth: 0,
                    status: self.row_status(row).into(),
                })
                .chain(
                    row.children
                        .iter()
                        .enumerate()
                        .filter(move |(_, node)| show_history || node.parent_step_key.is_none())
                        .map(move |(child, node)| {
                            let (label, status) = node
                                .label
                                .rsplit_once(" · ")
                                .filter(|(_, status)| {
                                    matches!(
                                        *status,
                                        "Completed"
                                            | "Running"
                                            | "Pending"
                                            | "Failed"
                                            | "Interrupted"
                                            | "Stopped"
                                            | "Discovered"
                                            | "Planned"
                                            | "Active"
                                            | "Skipped"
                                            | "Unknown"
                                    )
                                })
                                .unwrap_or((&node.label, ""));
                            TreeNode {
                                row: index,
                                child: Some(child),
                                key: format!("{}:{}", row.id, node.key),
                                label: label.trim().into(),
                                depth: node.depth.max(1),
                                status: status.into(),
                            }
                        }),
                )
            })
            .collect()
    }

    pub(crate) fn selected_node_key(&self) -> String {
        self.nodes()
            .into_iter()
            .find(|node| node.row == self.selected && node.child == self.child)
            .map(|node| node.key)
            .unwrap_or_default()
    }

    pub(crate) fn reconcile_attempt_history_selection(&mut self) {
        if self.center.show_attempt_history {
            return;
        }
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if let Some(parent) = self
            .child
            .and_then(|index| row.children.get(index))
            .and_then(|child| child.parent_step_key.as_ref())
        {
            self.child = row.children.iter().position(|child| &child.key == parent);
        }
    }

    pub(super) fn navigate_tree(&mut self, down: bool) {
        let nodes = self.nodes();
        let selected = nodes
            .iter()
            .position(|node| node.row == self.selected && node.child == self.child)
            .unwrap_or(0);
        let next = if down {
            (selected + 1).min(nodes.len().saturating_sub(1))
        } else {
            selected.saturating_sub(1)
        };
        if let Some(node) = nodes.get(next) {
            self.selected = node.row;
            self.child = node.child;
        }
    }

    pub(crate) fn selected_thread(&self) -> Option<ThreadId> {
        if !self.visible_run_indices().contains(&self.selected) {
            return None;
        }
        let row = self.rows.get(self.selected)?;
        let Some(index) = self.child else {
            return row.owner.or(self.owner);
        };
        let child = row.children.get(index)?;
        if child.parent_step_key.is_some() && !self.center.show_attempt_history {
            return None;
        }
        // A step without a recorded latest child must never open an older attempt.
        if child.key.starts_with("step:") || child.parent_step_key.is_some() {
            return child.thread_id;
        }
        None
    }

    pub(super) fn selected_worker_thread(&self) -> Option<ThreadId> {
        if self.center.saved || !self.visible_run_indices().contains(&self.selected) {
            return None;
        }
        let child = self.rows.get(self.selected)?.children.get(self.child?)?;
        if child.parent_step_key.is_some() && !self.center.show_attempt_history {
            return None;
        }
        if matches!(
            &child.transcript_target,
            Some(
                codex_app_server_protocol::WorkflowTranscriptTarget::Step { .. }
                    | codex_app_server_protocol::WorkflowTranscriptTarget::Attempt { .. }
            )
        ) {
            child.thread_id
        } else {
            None
        }
    }

    pub(crate) fn selected_transcript_target(&self) -> Option<TranscriptTarget> {
        if self.center.saved || !self.visible_run_indices().contains(&self.selected) {
            return None;
        }
        let row = self.rows.get(self.selected)?;
        let target = if let Some(index) = self.child {
            let child = row.children.get(index)?;
            if child.parent_step_key.is_some() && !self.center.show_attempt_history {
                return None;
            }
            child.transcript_target.clone()?
        } else {
            codex_app_server_protocol::WorkflowTranscriptTarget::Run
        };
        Some(TranscriptTarget {
            attachment: self.attached_thread.or(self.owner)?,
            owner: row.owner.or(self.owner)?,
            run_id: row.id.clone(),
            execution_id: row.execution_id.clone(),
            target,
        })
    }

    pub(super) fn selected_overview(&self) -> (Option<WorkflowOverview>, Vec<String>, String) {
        if !self.visible_run_indices().contains(&self.selected) {
            return (None, Vec::new(), String::new());
        }
        let Some(row) = self.rows.get(self.selected) else {
            return (None, Vec::new(), String::new());
        };
        let child = self.child.and_then(|index| row.children.get(index));
        (
            child
                .and_then(|child| child.overview.clone())
                .or_else(|| row.overview.clone()),
            child.map_or_else(|| row.details.clone(), |child| child.details.clone()),
            row.source.clone(),
        )
    }

    pub(super) fn selected_saved_index(&self) -> usize {
        self.center
            .saved_id
            .as_ref()
            .and_then(|id| self.saved.iter().position(|saved| &saved.id == id))
            .unwrap_or(0)
    }

    pub(crate) fn active_detail_tab(&self) -> DetailTab {
        if self.center.saved {
            self.center.saved_detail_tab
        } else {
            self.center.detail_tab
        }
    }

    pub(super) fn detail_key(&self) -> (String, DetailTab) {
        let key = if self.center.saved {
            self.saved
                .get(self.selected_saved_index())
                .map_or_else(String::new, |saved| format!("saved:{}", saved.id))
        } else {
            self.selected_node_key()
        };
        (key, self.active_detail_tab())
    }
}

impl DetailTab {
    pub(super) fn step(self, right: bool) -> Self {
        match (self, right) {
            (Self::Overview, true) | (Self::Source, false) => Self::Transcript,
            (Self::Transcript, true) | (Self::Overview, false) => Self::Source,
            (Self::Source, true) | (Self::Transcript, false) => Self::Overview,
        }
    }
}
