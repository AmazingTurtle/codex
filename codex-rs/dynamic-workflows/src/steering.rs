//! Human commands address logical work, never display labels or mutable source text.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::Serialize;

use crate::WorkflowResult;
use crate::revision::WorkflowRevision;
use crate::revision::WorkflowRevisionStatus;

mod commands;
mod plans;

pub const MAX_WORKFLOW_CONTROLS: usize = 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowStepDisposition {
    #[default]
    Native,
    Skipped,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStepControlOutcome {
    pub command_id: String,
    pub revision_id: String,
    pub replacement: WorkflowResult,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowExecutionStepIndex {
    pub step_id: String,
    pub logical_id: String,
    pub label: String,
    pub phase: Option<String>,
    pub worker_sequence: u16,
    pub attempt_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowExecutionAttempt {
    pub step_id: String,
    pub logical_id: String,
    pub label: String,
    pub phase: Option<String>,
    pub options: serde_json::Value,
    pub attempt: crate::WorkflowAttempt,
    pub result: WorkflowResult,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WorkflowControlTarget {
    Run,
    Phase {
        title: String,
    },
    Step {
        logical_id: String,
        expected_attempt_id: Option<String>,
        expected_turn_id: Option<String>,
    },
    /// Keys are unique only within the complete logical execution scope.
    FutureStep {
        scope: String,
        key: String,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WorkflowControlOperation {
    Guide {
        message: String,
    },
    EditQueued {
        prompt: String,
    },
    Skip {
        #[serde(default = "default_skip")]
        replacement: WorkflowResult,
    },
    Interrupt,
}

fn default_skip() -> WorkflowResult {
    WorkflowResult::Absent
}

pub(crate) fn logical_key(scope: &str, key: &str) -> String {
    let mut encoded = String::new();
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("{scope}/key:{encoded}")
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowControlRequest {
    pub command_id: String,
    pub expected_control_revision: u64,
    pub target: WorkflowControlTarget,
    pub operation: WorkflowControlOperation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowControlStatus {
    Pending,
    Applied,
    Rejected,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowControlCommand {
    pub id: String,
    pub revision_id: String,
    pub control_revision: u64,
    pub created_at: i64,
    pub target: WorkflowControlTarget,
    pub operation: WorkflowControlOperation,
    pub status: WorkflowControlStatus,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowControlState {
    pub control_revision: u64,
    pub active_revision_id: String,
    pub pending_revision_id: Option<String>,
    pub status: WorkflowRevisionStatus,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowControlResponse {
    pub command: WorkflowControlCommand,
    pub state: WorkflowControlState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WorkflowEffectKind {
    Agent {
        step_id: String,
    },
    Nested {
        logical_id: String,
    },
    Phase {
        title: String,
    },
    Log {
        sequence: u64,
        message: String,
        phase: Option<String>,
    },
    Budget {
        logical_id: String,
    },
}

/// Stable compiler statement ownership complements scoped keys; callback timing is irrelevant.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEffectReceipt {
    pub id: String,
    pub scope: String,
    pub statement_index: u32,
    pub kind: WorkflowEffectKind,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSteeringState {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub state: WorkflowControlState,
    #[serde(default)]
    pub revisions: BTreeMap<String, WorkflowRevision>,
    #[serde(default)]
    pub commands: Vec<WorkflowControlCommand>,
    #[serde(default)]
    pub effects: BTreeMap<String, WorkflowEffectReceipt>,
    #[serde(default)]
    pub mutations: BTreeMap<String, WorkflowMutationRecord>,
    #[serde(default)]
    pub effects_by_revision: BTreeMap<String, BTreeMap<String, WorkflowEffectReceipt>>,
    #[serde(default)]
    pub next_step_sequence: u16,
    #[serde(default)]
    pub replay_frontier: u32,
    #[serde(default)]
    pub suffix_open: bool,
    #[serde(default)]
    pub known_attempt_ids: std::collections::BTreeSet<String>,
    /// Direct workers accepted across all revisions, independently of usage contributors.
    #[serde(default)]
    pub accepted_child_ids: std::collections::BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowMutationRecord {
    pub request: serde_json::Value,
    pub control_revision: u64,
    pub revision_id: Option<String>,
}

pub(crate) fn validate_command_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("workflow command ID must contain 1..=128 ASCII letters, digits, underscores or hyphens".into());
    }
    Ok(())
}

impl WorkflowControlRequest {
    pub fn validate(&self) -> Result<(), String> {
        validate_command_id(&self.command_id)?;
        let valid_target = match &self.target {
            WorkflowControlTarget::Run => true,
            WorkflowControlTarget::Phase { title } => {
                !title.trim().is_empty() && title.len() <= 320
            }
            WorkflowControlTarget::Step {
                logical_id,
                expected_attempt_id,
                expected_turn_id,
            } => {
                !logical_id.is_empty()
                    && logical_id.len() <= 2048
                    && expected_attempt_id
                        .as_ref()
                        .is_none_or(|id| !id.is_empty() && id.len() <= 128)
                    && expected_turn_id
                        .as_ref()
                        .is_none_or(|id| !id.is_empty() && id.len() <= 128)
            }
            WorkflowControlTarget::FutureStep { scope, key } => {
                !scope.is_empty() && scope.len() <= 2048 && !key.is_empty() && key.len() <= 128
            }
        };
        if !valid_target {
            return Err("workflow control target is invalid or exceeds its bounded limit".into());
        }
        if matches!(
            self.target,
            WorkflowControlTarget::Run | WorkflowControlTarget::Phase { .. }
        ) {
            return Err("workflow mutations require one exact step; run and phase targets are selection context only".into());
        }
        if matches!(
            self.operation,
            WorkflowControlOperation::Guide { .. } | WorkflowControlOperation::Interrupt
        ) && !matches!(
            &self.target,
            WorkflowControlTarget::Step {
                expected_attempt_id: Some(_),
                expected_turn_id: Some(_),
                ..
            }
        ) {
            return Err(
                "guidance and interruption require an exact active step, attempt and owned turn"
                    .into(),
            );
        }
        match &self.operation {
            WorkflowControlOperation::Guide { message }
                if message.trim().is_empty() || message.len() > 8192 =>
            {
                return Err("workflow guidance must contain 1..=8192 UTF-8 bytes".into());
            }
            WorkflowControlOperation::EditQueued { prompt } => {
                if prompt.trim().is_empty() || prompt.len() > 8192 {
                    return Err("workflow prompt must contain 1..=8192 UTF-8 bytes".into());
                }
                if !matches!(
                    self.target,
                    WorkflowControlTarget::Step { .. } | WorkflowControlTarget::FutureStep { .. }
                ) {
                    return Err("editing a queued prompt requires an exact logical step".into());
                }
            }
            WorkflowControlOperation::Skip { replacement } => {
                if matches!(self.target, WorkflowControlTarget::Run) {
                    return Err("skipping requires a phase or logical step".into());
                }
                if let WorkflowResult::Value { value } = replacement {
                    crate::types::bounded_value(value)?;
                }
            }
            WorkflowControlOperation::Guide { .. } | WorkflowControlOperation::Interrupt => {}
        }
        Ok(())
    }
}
