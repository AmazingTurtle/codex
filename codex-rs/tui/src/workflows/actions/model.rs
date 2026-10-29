//! Captured workflow action intents separate terminal UI state from backend requests.
use codex_protocol::ThreadId;
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ActionTarget {
    Run,
    Phase {
        title: String,
    },
    Step {
        logical_id: String,
        expected_attempt_id: Option<String>,
        expected_turn_id: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActionScope {
    pub(crate) context_id: uuid::Uuid,
    pub(crate) owner: ThreadId,
    pub(crate) caller: ThreadId,
    pub(crate) workflow_id: String,
    pub(crate) execution_id: String,
    pub(crate) expected_control_revision: u64,
    pub(crate) target: ActionTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Guide,
    EditQueued,
    Skip,
    Interrupt,
    Revise,
    ApplyDrain,
    ApplyInterrupt,
    Resume,
    ShowResult,
    Revisions,
}

impl Action {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Guide => "Guide active agent",
            Self::EditQueued => "Edit queued prompt",
            Self::Skip => "Skip step",
            Self::Interrupt => "Interrupt selected work",
            Self::Revise => "Revise workflow",
            Self::ApplyDrain => "Apply revision after accepted work",
            Self::ApplyInterrupt => "Interrupt and apply revision",
            Self::Resume => "Resume workflow",
            Self::ShowResult => "Show result in chat",
            Self::Revisions => "Revision history",
        }
    }
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Guide => "Send guidance to the current accepted agent turn",
            Self::EditQueued => "Replace a prompt that has not started",
            Self::Skip => "Replace the selected step with null or a JSON value",
            Self::Interrupt => "Interrupt only the selected workflow work",
            Self::Revise => "Stage JavaScript source and arguments before applying",
            Self::ApplyDrain => "Keep accepted work, then apply the staged revision",
            Self::ApplyInterrupt => "Interrupt accepted work and apply the staged revision",
            Self::Resume => "Continue the current revision under its remaining budget",
            Self::ShowResult => "Insert the immutable result directly in chat, once",
            Self::Revisions => "Inspect revision status, preserved work, and rerun reasons",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActionAvailability {
    pub(crate) action: Action,
    /// None means the service snapshot explicitly permits this action.
    pub(crate) unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ActionSnapshot {
    pub(crate) scope: ActionScope,
    pub(crate) title: String,
    pub(crate) availability: Vec<ActionAvailability>,
    pub(crate) queued_prompt: Option<String>,
    pub(crate) source: String,
    pub(crate) input: Value,
    pub(crate) staged_revision_id: Option<String>,
    pub(crate) active_revision_id: Option<String>,
    pub(crate) budget_summary: String,
    pub(crate) footer_hidden: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RevisionApplyMode {
    DrainAccepted,
    InterruptAndApply,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ControlOperation {
    Guide { message: String },
    EditQueued { prompt: String },
    Skip { replacement: Value },
    Interrupt,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum WorkflowUiIntent {
    SetFooterVisibility {
        scope: ActionScope,
        visible: bool,
    },
    OpenForm {
        snapshot: Box<ActionSnapshot>,
        action: Action,
    },
    Control {
        scope: ActionScope,
        operation: ControlOperation,
    },
    RevisionArguments {
        scope: ActionScope,
        source: String,
        initial_input: Value,
    },
    StageRevision {
        scope: ActionScope,
        source: String,
        input: Value,
    },
    ApplyRevision {
        scope: ActionScope,
        revision_id: String,
        mode: RevisionApplyMode,
    },
    Resume {
        scope: ActionScope,
        revision_id: String,
    },
    ShowResult {
        scope: ActionScope,
    },
    RevisionHistory {
        scope: ActionScope,
        cursor: Option<String>,
    },
    InspectRevision {
        scope: ActionScope,
        revision: Box<RevisionPreview>,
    },
    ViewRevisionSource {
        scope: ActionScope,
        revision: Box<RevisionPreview>,
    },
    ViewExecution {
        scope: ActionScope,
        revision_id: String,
        execution_id: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RevisionPreview {
    pub(crate) id: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) number: u32,
    pub(crate) execution_ids: Vec<String>,
    pub(crate) status: String,
    pub(crate) source: String,
    pub(crate) input: Value,
    pub(crate) reusable_statements: u32,
    pub(crate) statement_count: u32,
    pub(crate) invalidation_reason: String,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct RevisionPage {
    pub(crate) data: Vec<RevisionPreview>,
    pub(crate) next_cursor: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct MutationReceipt {
    pub(crate) revision: Option<RevisionPreview>,
    pub(crate) state: Option<codex_app_server_protocol::WorkflowControlState>,
}

#[derive(Clone, Debug)]
pub(crate) enum WorkflowUiOutcome {
    Actions(Result<ActionSnapshot, String>),
    Mutation {
        intent: Box<WorkflowUiIntent>,
        result: Result<MutationReceipt, String>,
    },
    Revisions(Result<RevisionPage, String>),
    Execution(Result<codex_app_server_protocol::WorkflowExecutionSnapshot, String>),
}

pub(crate) type IntentSender = Arc<dyn Fn(WorkflowUiIntent) + Send + Sync>;

impl From<ActionTarget> for codex_app_server_protocol::WorkflowControlTarget {
    fn from(target: ActionTarget) -> Self {
        match target {
            ActionTarget::Run => Self::Run,
            ActionTarget::Phase { title } => Self::Phase { title },
            ActionTarget::Step {
                logical_id,
                expected_attempt_id,
                expected_turn_id,
            } => Self::Step {
                logical_id,
                expected_attempt_id,
                expected_turn_id,
            },
        }
    }
}

impl From<ControlOperation> for codex_app_server_protocol::WorkflowControlOperation {
    fn from(operation: ControlOperation) -> Self {
        match operation {
            ControlOperation::Guide { message } => Self::Guide { message },
            ControlOperation::EditQueued { prompt } => Self::EditQueued { prompt },
            ControlOperation::Skip { replacement } => Self::Skip {
                replacement: codex_app_server_protocol::WorkflowResult::Value {
                    value: replacement,
                },
            },
            ControlOperation::Interrupt => Self::Interrupt,
        }
    }
}

impl From<RevisionApplyMode> for codex_app_server_protocol::WorkflowRevisionApplyMode {
    fn from(mode: RevisionApplyMode) -> Self {
        match mode {
            RevisionApplyMode::DrainAccepted => Self::DrainAccepted,
            RevisionApplyMode::InterruptAndApply => Self::InterruptAndApply,
        }
    }
}

impl From<codex_app_server_protocol::WorkflowRevision> for RevisionPreview {
    fn from(revision: codex_app_server_protocol::WorkflowRevision) -> Self {
        Self {
            id: revision.id,
            parent_id: revision.parent_id,
            number: revision.number,
            execution_ids: revision.execution_ids,
            status: format!("{:?}", revision.status),
            source: revision.source,
            input: revision.input,
            reusable_statements: revision.reusable_statements,
            statement_count: revision.statement_count,
            invalidation_reason: revision.invalidation_reason,
            error: revision.error,
        }
    }
}
