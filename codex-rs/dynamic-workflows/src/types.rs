use codex_utils_absolute_path::AbsolutePathBuf;
use std::collections::BTreeMap;
use std::future::Future;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

pub const MAX_SOURCE_BYTES: usize = 24 * 1024;
pub const MAX_VALUE_BYTES: usize = 16 * 1024;
pub const MAX_STEPS: usize = 1024;
pub const MAX_PAGE_SIZE: usize = 50;
pub const WORKFLOW_FORMAT_VERSION: u32 = 2;
pub const MAX_AGENT_STARTS: usize = 1024;
pub const MAX_LOG_ENTRIES: usize = 128;
pub const MAX_OBSERVED_THREADS: usize = 16_384;
pub const MAX_BUDGET_OBSERVATIONS: usize = 4096;
pub const MAX_BUDGET_OBSERVATION_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BudgetObservationKind {
    Spent,
    Remaining,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BudgetObservation {
    pub kind: BudgetObservationKind,
    pub spent: u64,
    pub total: Option<u64>,
}

fn legacy_format_version() -> u32 {
    1
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLimits {
    pub max_agents: usize,
    pub max_tokens: Option<u64>,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            max_agents: 32,
            max_tokens: None,
        }
    }
}

impl RunLimits {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_AGENT_STARTS).contains(&self.max_agents) {
            return Err("workflow max_agents must be between 1 and 1024".into());
        }
        if self
            .max_tokens
            .is_some_and(|value| value == 0 || value > 9_007_199_254_740_991)
        {
            return Err(
                "workflow max_tokens must be positive and fit JavaScript's safe integer range"
                    .into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowMetrics {
    pub usage: Option<WorkflowTokenUsage>,
    pub usage_complete: bool,
    #[serde(default)]
    pub usage_incomplete_reason: Option<String>,
    pub active_duration_ms: Option<u64>,
    pub wall_duration_ms: Option<u64>,
    /// Unique usage-bearing model responses, not native turns or tool calls.
    #[serde(default)]
    pub model_turns: Option<u64>,
    /// Accepted workflow workers; private attributed contributors are excluded.
    #[serde(default)]
    pub agents_used: Option<u32>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowActivity {
    pub active_duration_ms: u64,
    #[serde(default)]
    pub execution_duration_ms: u64,
    pub active_since_ms: Option<i64>,
    pub checkpoint_at_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PhaseStatus {
    #[default]
    Pending,
    Active,
    Completed,
    Skipped,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseProgress {
    pub title: String,
    pub status: PhaseStatus,
    pub steps_discovered: u32,
    pub steps_completed: u32,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub completed_at: Option<i64>,
    #[serde(default)]
    pub metrics: WorkflowMetrics,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowLog {
    pub sequence: u64,
    pub message: String,
    pub phase: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AdmissionStatus {
    #[default]
    NotRequested,
    Reserved,
    Accepted,
    Rejected,
    Uncertain,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedWorkflow {
    pub name: String,
    pub resolved_id: String,
    pub source: String,
    #[serde(default)]
    pub source_hash: String,
    pub input: Value,
    pub metadata: crate::ScriptMetadata,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedWorkflow {
    pub id: String,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRun {
    pub id: String,
    #[serde(default)]
    pub lifecycle_version: u32,
    #[serde(default)]
    pub completion_mode: crate::WorkflowCompletionMode,
    #[serde(default)]
    pub lifecycle: crate::WorkflowLifecycleState,
    #[serde(default)]
    pub steering: crate::steering::WorkflowSteeringState,
    #[serde(default = "legacy_format_version")]
    pub format_version: u32,
    #[serde(default)]
    pub metadata: Option<crate::ScriptMetadata>,
    #[serde(default)]
    pub limits: RunLimits,
    #[serde(default)]
    pub accepted_starts: u32,
    #[serde(default)]
    pub phase_progress: Vec<PhaseProgress>,
    #[serde(default)]
    pub metrics: WorkflowMetrics,
    #[serde(default)]
    pub activity: WorkflowActivity,
    #[serde(default)]
    pub logs: Vec<WorkflowLog>,
    #[serde(default)]
    pub nested_workflows: BTreeMap<String, NestedWorkflow>,
    #[serde(default)]
    pub budget_observations: BTreeMap<String, BudgetObservation>,
    /// Stable local ordinal for readable worker paths; zero denotes legacy runs.
    #[serde(default)]
    pub worker_sequence: u64,
    /// Stable terminal notification identity for this execution/recovery revision.
    #[serde(default)]
    pub execution_id: String,
    #[serde(default)]
    pub notification_delivered: bool,
    pub owner_thread_id: String,
    #[serde(default)]
    pub project_root: Option<AbsolutePathBuf>,
    pub source: String,
    #[serde(default)]
    pub source_hash: String,
    pub input: Value,
    pub status: WorkflowStatus,
    pub phase: Option<String>,
    #[serde(default)]
    pub phases: Vec<String>,
    pub steps: BTreeMap<String, WorkflowStep>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_value"
    )]
    pub result: Option<Value>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSummary {
    pub id: String,
    #[serde(default)]
    pub execution_id: String,
    pub owner_thread_id: String,
    pub project_root: Option<AbsolutePathBuf>,
    pub format_version: u32,
    pub metadata: Option<crate::ScriptMetadata>,
    pub limits: RunLimits,
    pub accepted_starts: u32,
    pub phase: Option<String>,
    #[serde(default)]
    pub phase_count: u32,
    pub phase_progress: Vec<PhaseProgress>,
    pub metrics: WorkflowMetrics,
    pub status: WorkflowStatus,
    pub created_at: i64,
    pub updated_at: i64,
    pub step_count: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Stopped,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStep {
    pub id: String,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub completed_at: Option<i64>,
    #[serde(default)]
    pub metrics: WorkflowMetrics,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub disposition: crate::WorkflowStepDisposition,
    #[serde(default)]
    pub control_outcome: Option<crate::WorkflowStepControlOutcome>,
    #[serde(default)]
    pub effective_options: Option<Value>,
    #[serde(default)]
    pub schema_attempts: u8,
    #[serde(default)]
    pub logical_id: String,
    #[serde(default)]
    pub label: String,
    /// Creation order, retained independently of the map's lexical iteration order.
    #[serde(default)]
    pub worker_sequence: u16,
    #[serde(default)]
    pub phase: Option<String>,
    pub options: Value,
    pub attempts: Vec<WorkflowAttempt>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_value"
    )]
    pub result: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAttempt {
    pub id: String,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub completed_at: Option<i64>,
    #[serde(default)]
    pub metrics: WorkflowMetrics,
    /// Per-execution highwater for pause-excluding native attempt observations.
    #[serde(default)]
    pub activity: WorkflowActivity,
    #[serde(default)]
    pub clock_execution_id: Option<String>,
    #[serde(default)]
    pub clock_incomplete: bool,
    #[serde(default)]
    pub execution_id: String,
    #[serde(default)]
    pub owned_turn_ids: Vec<String>,
    #[serde(default)]
    pub final_turn_id: Option<String>,
    #[serde(default)]
    pub resolved_model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub validation_feedback: Option<String>,
    #[serde(default)]
    pub admission: AdmissionStatus,
    #[serde(default)]
    pub schema_error: Option<String>,
    pub child_thread_id: Option<String>,
    pub status: WorkflowStatus,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_value"
    )]
    pub usage: Option<Value>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentRequest {
    pub run_id: String,
    pub revision_id: String,
    pub run_sequence: u64,
    pub owner_thread_id: String,
    pub step_id: String,
    pub label: String,
    pub validation_feedback: Option<String>,
    pub step_sequence: u16,
    pub attempt_id: String,
    pub attempt_number: u8,
    pub options: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentStartMode {
    Start,
    Recover { child_thread_id: Option<String> },
}

/// Distinguishes a rejected operation from an attempt whose child may exist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentStartError {
    /// The host guarantees this attempt did not accept or create a child.
    NotStarted(String),
    /// Child creation or recovery is ambiguous and must be reconciled by identity.
    Uncertain(String),
}

impl std::fmt::Display for AgentStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotStarted(message) | Self::Uncertain(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AgentStartError {}

impl From<String> for AgentStartError {
    fn from(message: String) -> Self {
        Self::Uncertain(message)
    }
}

impl From<&str> for AgentStartError {
    fn from(message: &str) -> Self {
        Self::Uncertain(message.to_string())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentOutcome {
    pub result: Value,
    pub usage: Value,
    /// A confirmed terminal child failure. Transport/recovery uncertainty belongs
    /// in the host method's `Err`, so retry cannot duplicate unresolved work.
    pub error: Option<String>,
}

/// A host observation never transfers ownership of the child or its permission profile.
#[derive(Clone, Debug)]
pub enum WorkflowHostEvent {
    /// The producer exceeded its count bound; an earlier highwater is only partial.
    ModelTurnsIncomplete {
        attempt_id: String,
        thread_id: String,
    },
    ModelTurns {
        attempt_id: String,
        thread_id: String,
        model_turns: Option<u64>,
    },
    AttemptClock {
        attempt_id: String,
        active_duration_ms: u64,
        at_ms: i64,
    },
    AttemptContext {
        attempt_id: String,
        owned_turn_ids: Vec<String>,
        final_turn_id: Option<String>,
        resolved_model: Option<String>,
        reasoning_effort: Option<String>,
    },
    StartUncertain {
        attempt_id: String,
        revision_id: String,
    },
    /// A native capacity or pause refusal proves that this attempt created no child.
    /// Persist this receipt before retrying admission with the same reservation.
    StartRetryable {
        attempt_id: String,
    },
    StartAccepted {
        attempt_id: String,
        child_thread_id: String,
    },
    StartRejected {
        attempt_id: String,
        reason: String,
    },
    UsageIncomplete {
        reason: String,
    },
    ExecutionClock {
        active_duration_ms: u64,
        at_ms: i64,
    },
    Usage {
        attempt_id: String,
        thread_id: String,
        usage: WorkflowTokenUsage,
    },
    UsageComplete {
        attempt_id: String,
        thread_id: String,
    },
    ExecutionActive {
        active: bool,
        at_ms: i64,
    },
}

/// Hosts publish cumulative own-thread usage; stores de-duplicate each thread high-water mark.
pub type WorkflowObserver =
    std::sync::Arc<dyn Fn(WorkflowHostEvent) -> Result<(), String> + Send + Sync>;

/// Recheck live global accounting immediately before a fresh native acceptance.
/// A host must reconcile current own-thread usage before invoking this guard.
pub type WorkflowAdmissionGuard = std::sync::Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// Runs ordinary controlled child agents on behalf of one conversation tree.
///
/// Scheduling must use the host's shared agent budget and conversation pause gate.
/// The attempt ID is a durable idempotency key: recovery must reconnect the same
/// child, including a crash between child creation and persisting its identity.
/// An unknown recovery outcome must return an error rather than spawning a child.
/// Child transcripts belong to the normal host and survive workflow stop/retry.
/// Before cancellable native creation, publish `StartUncertain`; publish accepted
/// or known-rejected receipts before returning or dropping the scheduling future.
/// A known capacity or pause refusal may publish `StartRetryable` before retrying
/// that same attempt; an uncertain outcome must never use this receipt.
pub trait WorkflowHost: Send + Sync + 'static {
    /// Fence only fresh native admission; accepted work and its observations continue.
    fn fence_run(
        &self,
        _run_id: String,
        _expected_revision_id: String,
    ) -> impl Future<Output = Result<crate::WorkflowFenceReceipt, String>> + Send {
        async { Err("workflow revision fences are unavailable in this host".into()) }
    }
    fn resume_revision(
        &self,
        _run_id: String,
        _revision_id: String,
    ) -> impl Future<Output = Result<(), String>> + Send {
        async { Ok(()) }
    }
    fn cancel_prepared_attempt(
        &self,
        _request: AgentRequest,
    ) -> impl Future<Output = Result<crate::WorkflowPreparedDisposition, String>> + Send {
        async { Ok(crate::WorkflowPreparedDisposition::Uncertain) }
    }
    fn guide_attempt(
        &self,
        _request: AgentRequest,
        _expected_child: String,
        _expected_turn: String,
        _message: String,
    ) -> impl Future<Output = Result<(), String>> + Send {
        async { Err("workflow guidance is unavailable in this host".into()) }
    }
    fn interrupt_attempt(
        &self,
        _request: AgentRequest,
        _expected_child: String,
        _expected_turn: String,
    ) -> impl Future<Output = Result<crate::WorkflowInterruptReceipt, String>> + Send {
        async { Err("workflow targeted interruption is unavailable in this host".into()) }
    }
    /// Reports only the actual current turn proven to belong to this accepted attempt.
    fn active_owned_turn(
        &self,
        _request: AgentRequest,
        _expected_child: String,
    ) -> impl Future<Output = Result<Option<String>, String>> + Send {
        async { Ok(None) }
    }
    fn register_admission_guard(
        &self,
        _run_id: String,
        _guard: WorkflowAdmissionGuard,
    ) -> impl Future<Output = Result<(), String>> + Send {
        async { Ok(()) }
    }
    /// Flush final usage observations and release this run's observer membership.
    fn finish_run(&self, _run_id: String) -> impl Future<Output = Result<(), String>> + Send {
        async { Ok(()) }
    }
    fn register_observer(
        &self,
        _run_id: String,
        _observer: WorkflowObserver,
    ) -> impl Future<Output = Result<(), String>> + Send {
        async { Ok(()) }
    }

    /// Resolve a named saved workflow through the original caller's read authority.
    fn load_workflow(
        &self,
        _run_id: String,
        _name: String,
    ) -> impl Future<Output = Result<ResolvedWorkflow, String>> + Send {
        async { Err("nested workflows are unavailable in this host".into()) }
    }
    /// Opens scheduling for an explicitly admitted run or recovery attempt.
    fn start_run(&self, _run_id: String) -> impl Future<Output = Result<(), String>> + Send {
        async { Ok(()) }
    }

    fn schedule_agent(
        &self,
        request: AgentRequest,
        mode: AgentStartMode,
    ) -> impl Future<Output = Result<String, AgentStartError>> + Send;

    fn wait_agent(
        &self,
        request: AgentRequest,
        child_thread_id: String,
    ) -> impl Future<Output = Result<AgentOutcome, String>> + Send;

    /// Establish a scheduling barrier for this run and stop its accepted children.
    /// A concurrent in-flight schedule must either be rejected or included in
    /// teardown before this returns; children must not appear after stop returns.
    fn stop_run(&self, run_id: String) -> impl Future<Output = Result<(), String>> + Send;
}

pub(crate) fn bounded_value(value: &Value) -> Result<(), String> {
    if serde_json::to_vec(value)
        .map_err(|err| err.to_string())?
        .len()
        > MAX_VALUE_BYTES
    {
        return Err(format!("workflow JSON exceeds {MAX_VALUE_BYTES} bytes"));
    }
    Ok(())
}

// JSON null is a legitimate published/cached value. Omission represents absence
// in persisted engine state; app-server DTOs retain their own wire convention.
fn present_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

pub(crate) fn validate_source(source: &str) -> Result<(), String> {
    if source.trim().is_empty() || source.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "workflow source must contain 1..={MAX_SOURCE_BYTES} bytes"
        ));
    }
    Ok(())
}

pub(crate) fn timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
