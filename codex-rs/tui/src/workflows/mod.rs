//! Conversation-scoped workflow presentation shared by the strip and command center.
pub(crate) mod actions;
mod agent_style;
mod center;
pub(crate) use agent_style::agent_style;
mod detail_viewport;
mod details;
mod display;
mod filters;
mod navigation;
mod progress;
mod projection;
#[path = "saved/commands.rs"]
pub(crate) mod saved_commands;
mod strip;
mod transcript;
pub(crate) use transcript::AgentHeader;
pub(crate) use transcript::TranscriptGroup;
pub(crate) use transcript::TranscriptItem;
pub(crate) use transcript::TranscriptPage;
pub(crate) use transcript::TranscriptTarget;
pub(crate) use transcript::WorkflowTranscript;

pub(crate) use center::WorkflowCenter;
pub(crate) use projection::project;
pub(crate) use projection::summary;
pub(crate) use strip::WorkflowStrip;

use codex_features::DynamicWorkflowsMode;
use codex_protocol::ThreadId;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;

pub(crate) const CENTER_ID: &str = "workflow-center";
/// Shared only by UI-thread views; native incremental parsers never cross into RPC tasks.
/// Keep the existing mutex guard and poison handling while making that ownership explicit.
pub(crate) type SharedWorkflows = Rc<Mutex<WorkflowState>>;

#[derive(Clone, Debug)]
pub(crate) struct OverviewTarget {
    pub(crate) owner: ThreadId,
    pub(crate) caller: ThreadId,
    pub(crate) run_id: String,
    pub(crate) execution_id: String,
    pub(crate) node_key: String,
    pub(crate) detail_revision: u64,
    pub(crate) request_id: uuid::Uuid,
}

pub(crate) fn execution_label(
    state: Option<codex_protocol::protocol::ExecutionControlState>,
) -> Option<&'static str> {
    use codex_protocol::protocol::ExecutionControlState;
    match state {
        Some(ExecutionControlState::Pausing) => Some("Pausing"),
        Some(ExecutionControlState::Paused) => Some("Paused"),
        Some(ExecutionControlState::Running) | None => None,
    }
}

#[derive(Debug)]
pub(crate) enum WorkflowEvent {
    OverviewLoaded {
        target: OverviewTarget,
        result: Result<Box<codex_app_server_protocol::WorkflowRun>, String>,
    },
    Open,
    ActionsOpen {
        owner: ThreadId,
        caller: ThreadId,
        run_id: String,
        execution_id: String,
        target: actions::ActionTarget,
    },
    ControlIntent(Box<actions::WorkflowUiIntent>),
    ControlResult {
        scope: actions::ActionScope,
        outcome: Box<actions::WorkflowUiOutcome>,
    },
    ChooseMode,
    SetMode(DynamicWorkflowsMode),
    Pause,
    Unpause,
    ConfirmExecution {
        root: ThreadId,
        pause: bool,
    },
    ExecutionSnapshot {
        request_id: uuid::Uuid,
        snapshot: codex_app_server_protocol::ThreadExecutionControlChangedNotification,
    },
    Refresh,
    RefreshSavedCatalog,
    ChooseScope,
    SetScope(WorkflowListScope),
    NextPage {
        saved: bool,
    },
    PreviousPage {
        saved: bool,
    },
    OpenOwner(ThreadId),
    Loaded {
        owner: ThreadId,
        request_id: uuid::Uuid,
        scope: WorkflowListScope,
        result: Result<WorkflowPage, String>,
    },
    OperationFinished {
        owner: ThreadId,
        request_id: uuid::Uuid,
        result: Result<(), String>,
    },
    SelectAgent(ThreadId),
    AgentOpened(ThreadId),
    Action {
        owner: ThreadId,
        id: String,
        action: WorkflowAction,
    },
    StartSaved {
        owner: ThreadId,
        caller: ThreadId,
        id: String,
        input: serde_json::Value,
        request_id: uuid::Uuid,
    },
    ChooseSaveScope {
        owner: ThreadId,
        id: String,
        name: String,
    },
    SaveNamed {
        owner: ThreadId,
        id: String,
        name: String,
        scope: codex_app_server_protocol::WorkflowScope,
    },
    ReadTranscript {
        target: TranscriptTarget,
        older: bool,
    },
    ReadSavedSource {
        id: String,
        refresh: bool,
    },
    SavedSourceLoaded {
        id: String,
        request_id: uuid::Uuid,
        result: Result<String, String>,
    },
    SavedCatalogLoaded {
        owner: ThreadId,
        request_id: uuid::Uuid,
        result: Result<saved_commands::SavedCatalog, String>,
    },
    TranscriptLoaded {
        target: TranscriptTarget,
        request_id: uuid::Uuid,
        result: Result<TranscriptPage, String>,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum WorkflowAction {
    Stop,
    Retry,
    Recover,
    Save,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct WorkflowRow {
    pub(crate) detail_revision: u64,
    pub(crate) owner: Option<ThreadId>,
    pub(crate) id: String,
    pub(crate) execution_id: String,
    pub(crate) title: String,
    pub(crate) status: String,
    pub(crate) read_only: bool,
    pub(crate) read_only_reason: Option<String>,
    pub(crate) metadata: Option<codex_app_server_protocol::WorkflowMetadata>,
    pub(crate) phase: Option<String>,
    pub(crate) phase_progress: Vec<codex_app_server_protocol::WorkflowPhaseProgress>,
    pub(crate) metrics: Option<codex_app_server_protocol::WorkflowMetrics>,
    pub(crate) logs: Vec<codex_app_server_protocol::WorkflowLog>,
    pub(crate) completed: Option<usize>,
    pub(crate) discovered: usize,
    pub(crate) agents_used: Option<usize>,
    pub(crate) details: Vec<String>,
    pub(crate) children: Vec<WorkflowChild>,
    pub(crate) overview: Option<WorkflowOverview>,
    pub(crate) source: String,
    pub(crate) project_root: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct WorkflowChild {
    pub(crate) key: String,
    /// Earlier attempts belong to this stable step key and are hidden by default.
    pub(crate) parent_step_key: Option<String>,
    pub(crate) label: String,
    pub(crate) thread_id: Option<ThreadId>,
    pub(crate) transcript_target: Option<codex_app_server_protocol::WorkflowTranscriptTarget>,
    pub(crate) action_target: Option<actions::ActionTarget>,
    pub(crate) resolved_model: Option<String>,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) updated_at: Option<i64>,
    pub(crate) details: Vec<String>,
    pub(crate) depth: u16,
    pub(crate) overview: Option<WorkflowOverview>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct WorkflowOverview {
    pub(crate) refresh_error: Option<String>,
    pub(crate) title: String,
    pub(crate) metadata: Vec<(String, String)>,
    pub(crate) prompt: Option<String>,
    pub(crate) input: Option<serde_json::Value>,
    pub(crate) result: Option<serde_json::Value>,
    pub(crate) error: Option<String>,
    pub(crate) phase_steps: Vec<WorkflowPhaseStep>,
    pub(crate) result_label: Option<String>,
    pub(crate) metrics: Option<codex_app_server_protocol::WorkflowMetrics>,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkflowPhaseStep {
    pub(crate) label: String,
    pub(crate) model: Option<String>,
    pub(crate) reasoning: Option<String>,
    pub(crate) prompt: String,
    pub(crate) status: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum DetailTab {
    #[default]
    Overview,
    Transcript,
    Source,
}

#[derive(Default)]
pub(crate) struct CenterState {
    pub(crate) saved: bool,
    pub(crate) saved_id: Option<String>,
    pub(crate) details_focused: bool,
    pub(crate) detail_tab: DetailTab,
    pub(crate) saved_detail_tab: DetailTab,
    pub(crate) detail_page_height: u16,
    pub(crate) detail_viewports:
        std::collections::HashMap<(String, DetailTab), detail_viewport::DetailViewport>,
    pub(crate) show_attempt_history: bool,
    pub(crate) status_filter: usize,
    pub(crate) library_filter: usize,
    pub(crate) grouping: WorkflowGrouping,
    pub(crate) help: bool,
    pub(crate) help_scroll: u16,
    pub(crate) page_height: usize,
    pub(crate) list_scroll: usize,
    pub(crate) narrow: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WorkflowGrouping {
    #[default]
    Conversation,
    Status,
    Project,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SavedWorkflowRow {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) scope: Option<codex_app_server_protocol::WorkflowScope>,
    pub(crate) metadata: Option<codex_app_server_protocol::WorkflowMetadata>,
    pub(crate) read_only: bool,
    pub(crate) read_only_reason: Option<String>,
    pub(crate) command_notice: Option<String>,
}

#[derive(Default)]
pub(crate) struct SavedWorkflowSource {
    pub(crate) source: Option<String>,
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    pub(crate) request_id: Option<uuid::Uuid>,
}

#[derive(Debug)]
pub(crate) struct WorkflowPage {
    pub(crate) root: ThreadId,
    pub(crate) rows: Vec<WorkflowRow>,
    pub(crate) saved: Vec<SavedWorkflowRow>,
    pub(crate) next_cursor: Option<String>,
    pub(crate) saved_next_cursor: Option<String>,
}

#[derive(Default)]
pub(crate) struct WorkflowPagination {
    pub(crate) cursor: Option<String>,
    pub(crate) previous: Vec<Option<String>>,
    pub(crate) next: Option<String>,
}

#[derive(Default)]
pub(crate) struct WorkflowState {
    pub(crate) overview_request: Option<uuid::Uuid>,
    pub(crate) overview_dirty: bool,
    pub(crate) action_context_id: Option<uuid::Uuid>,
    pub(crate) action_snapshot: Option<Box<actions::ActionSnapshot>>,
    pub(crate) action_pending: bool,
    pub(crate) action_refresh_pending: bool,
    /// The displayed input origin is distinct from the root that owns the workflow library.
    pub(crate) displayed_thread: Option<ThreadId>,
    pub(crate) command_catalog: Vec<saved_commands::SavedCommandCandidate>,
    pub(crate) catalog_overflow: bool,
    pub(crate) catalog_loading: bool,
    pub(crate) catalog_request_id: Option<uuid::Uuid>,
    pub(crate) catalog_cancel: Option<tokio_util::sync::CancellationToken>,
    pub(crate) scope: WorkflowListScope,
    pub(crate) attached_thread: Option<ThreadId>,
    pub(crate) owner: Option<ThreadId>,
    pub(crate) rows: Vec<WorkflowRow>,
    pub(crate) saved: Vec<SavedWorkflowRow>,
    pub(crate) saved_sources: std::collections::HashMap<String, SavedWorkflowSource>,
    pub(crate) selected: usize,
    pub(crate) child: Option<usize>,
    pub(crate) strip_focused: bool,
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    pub(crate) load_error: Option<String>,
    pub(crate) execution: Option<codex_protocol::protocol::ExecutionControlState>,
    pub(crate) can_pause: bool,
    pub(crate) execution_notice_state: Option<codex_protocol::protocol::ExecutionControlState>,
    pub(crate) center: CenterState,
    pub(crate) transcripts: std::collections::HashMap<String, WorkflowTranscript>,
    pub(crate) hidden_from_strip: HashSet<String>,
    pub(crate) refresh_at: Option<std::time::Instant>,
    pub(crate) operation_pending: bool,
    pub(crate) operation_id: Option<uuid::Uuid>,
    pub(crate) operation_result: Option<Result<(), String>>,
    pub(crate) refresh_pending: bool,
    pub(crate) inspected_agent: Option<ThreadId>,
    pub(crate) updates_during_refresh: std::collections::HashMap<String, WorkflowRow>,
    pub(crate) request_id: Option<uuid::Uuid>,
    pub(crate) page: WorkflowPagination,
    pub(crate) saved_page: WorkflowPagination,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WorkflowListScope {
    #[default]
    Current,
    Project,
    All,
}

impl WorkflowListScope {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Current => "Current conversation",
            Self::Project => "Project",
            Self::All => "All conversations",
        }
    }
}

impl WorkflowState {
    pub(crate) fn apply_update(&mut self, mut row: WorkflowRow) {
        if let Some(previous) = self.rows.iter().find(|previous| previous.id == row.id) {
            row.preserve_accounting_from(previous);
            row.detail_revision = previous.detail_revision.wrapping_add(1);
        }
        if self.loading
            && (self.updates_during_refresh.len() < 50
                || self.updates_during_refresh.contains_key(&row.id))
        {
            self.updates_during_refresh
                .insert(row.id.clone(), row.clone());
        }
        if let Some(selected) = self.rows.get(self.selected)
            && selected.id == row.id
        {
            self.child = self
                .child
                .and_then(|index| selected.children.get(index))
                .and_then(|selected| {
                    row.children
                        .iter()
                        .position(|child| child.key == selected.key)
                });
        }
        if let Some(existing) = self.rows.iter_mut().find(|existing| existing.id == row.id) {
            *existing = row;
        } else if self.rows.len() < 100 {
            self.rows.push(row);
        }
        self.reconcile_visible_selection();
    }
    pub(crate) fn current_indices(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                (row.owner.is_none() || row.owner == self.owner)
                    && !self.hidden_from_strip.contains(&row.id)
            })
            .map(|(index, _)| index)
            .collect()
    }
    pub(crate) fn merge(&mut self, incoming: Vec<WorkflowRow>) {
        let selected = self.rows.get(self.selected).map(|row| row.id.clone());
        let child = self
            .rows
            .get(self.selected)
            .and_then(|row| self.child.and_then(|index| row.children.get(index)))
            .map(|child| child.key.clone());
        let mut incoming: std::collections::HashMap<_, _> = incoming
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect();
        incoming.extend(std::mem::take(&mut self.updates_during_refresh));
        self.rows.retain_mut(|row| {
            if let Some(mut updated) = incoming.remove(&row.id) {
                updated.preserve_accounting_from(row);
                updated.detail_revision = row.detail_revision.wrapping_add(1);
                *row = updated;
                true
            } else {
                false
            }
        });
        let mut added: Vec<_> = incoming.into_values().collect();
        added.sort_by(|a, b| a.id.cmp(&b.id));
        self.rows.extend(added);
        self.selected = selected
            .and_then(|id| self.rows.iter().position(|row| row.id == id))
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1));
        self.child = child.and_then(|key| {
            self.rows
                .get(self.selected)
                .and_then(|row| row.children.iter().position(|child| child.key == key))
        });
        if self.rows.is_empty() {
            self.strip_focused = false;
            self.child = None;
        }
        self.reconcile_visible_selection();
    }
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
