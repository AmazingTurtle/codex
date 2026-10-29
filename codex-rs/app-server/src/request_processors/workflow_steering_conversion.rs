//! Explicit conversions keep native policy and the wire contract separate.
use super::conversion;
use codex_app_server_protocol as api;
use codex_core::workflows as engine;

pub(super) fn input(value: api::WorkflowResult) -> engine::WorkflowResult {
    match value {
        api::WorkflowResult::Absent => engine::WorkflowResult::Absent,
        api::WorkflowResult::Value { value } => engine::WorkflowResult::Value { value },
    }
}

pub(super) fn result(value: engine::WorkflowResult) -> api::WorkflowResult {
    match value {
        engine::WorkflowResult::Absent => api::WorkflowResult::Absent,
        engine::WorkflowResult::Value { value } => api::WorkflowResult::Value { value },
    }
}

pub(super) fn target(value: api::WorkflowControlTarget) -> engine::WorkflowControlTarget {
    match value {
        api::WorkflowControlTarget::Run => engine::WorkflowControlTarget::Run,
        api::WorkflowControlTarget::Phase { title } => {
            engine::WorkflowControlTarget::Phase { title }
        }
        api::WorkflowControlTarget::Step {
            logical_id,
            expected_attempt_id,
            expected_turn_id,
        } => engine::WorkflowControlTarget::Step {
            logical_id,
            expected_attempt_id,
            expected_turn_id,
        },
        api::WorkflowControlTarget::FutureStep { scope, key } => {
            engine::WorkflowControlTarget::FutureStep { scope, key }
        }
    }
}

fn output_target(value: engine::WorkflowControlTarget) -> api::WorkflowControlTarget {
    match value {
        engine::WorkflowControlTarget::Run => api::WorkflowControlTarget::Run,
        engine::WorkflowControlTarget::Phase { title } => {
            api::WorkflowControlTarget::Phase { title }
        }
        engine::WorkflowControlTarget::Step {
            logical_id,
            expected_attempt_id,
            expected_turn_id,
        } => api::WorkflowControlTarget::Step {
            logical_id,
            expected_attempt_id,
            expected_turn_id,
        },
        engine::WorkflowControlTarget::FutureStep { scope, key } => {
            api::WorkflowControlTarget::FutureStep { scope, key }
        }
    }
}

pub(super) fn operation(value: api::WorkflowControlOperation) -> engine::WorkflowControlOperation {
    match value {
        api::WorkflowControlOperation::Guide { message } => {
            engine::WorkflowControlOperation::Guide { message }
        }
        api::WorkflowControlOperation::EditQueued { prompt } => {
            engine::WorkflowControlOperation::EditQueued { prompt }
        }
        api::WorkflowControlOperation::Skip { replacement } => {
            engine::WorkflowControlOperation::Skip {
                replacement: input(replacement),
            }
        }
        api::WorkflowControlOperation::Interrupt => engine::WorkflowControlOperation::Interrupt,
    }
}

fn output_operation(value: engine::WorkflowControlOperation) -> api::WorkflowControlOperation {
    match value {
        engine::WorkflowControlOperation::Guide { message } => {
            api::WorkflowControlOperation::Guide { message }
        }
        engine::WorkflowControlOperation::EditQueued { prompt } => {
            api::WorkflowControlOperation::EditQueued { prompt }
        }
        engine::WorkflowControlOperation::Skip { replacement } => {
            api::WorkflowControlOperation::Skip {
                replacement: result(replacement),
            }
        }
        engine::WorkflowControlOperation::Interrupt => api::WorkflowControlOperation::Interrupt,
    }
}

pub(super) fn mode(value: api::WorkflowRevisionApplyMode) -> engine::RevisionApplyMode {
    match value {
        api::WorkflowRevisionApplyMode::DrainAccepted => engine::RevisionApplyMode::DrainAccepted,
        api::WorkflowRevisionApplyMode::InterruptAndApply => {
            engine::RevisionApplyMode::InterruptAndApply
        }
    }
}

fn revision_status(value: engine::WorkflowRevisionStatus) -> api::WorkflowRevisionStatus {
    match value {
        engine::WorkflowRevisionStatus::Draft => api::WorkflowRevisionStatus::Draft,
        engine::WorkflowRevisionStatus::Fencing => api::WorkflowRevisionStatus::Fencing,
        engine::WorkflowRevisionStatus::Draining => api::WorkflowRevisionStatus::Draining,
        engine::WorkflowRevisionStatus::Ready => api::WorkflowRevisionStatus::Ready,
        engine::WorkflowRevisionStatus::Active => api::WorkflowRevisionStatus::Active,
        engine::WorkflowRevisionStatus::Superseded => api::WorkflowRevisionStatus::Superseded,
        engine::WorkflowRevisionStatus::Blocked => api::WorkflowRevisionStatus::Blocked,
    }
}

pub(super) fn state(value: engine::WorkflowControlState) -> api::WorkflowControlState {
    api::WorkflowControlState {
        control_revision: value.control_revision,
        active_revision_id: value.active_revision_id,
        pending_revision_id: value.pending_revision_id,
        status: revision_status(value.status),
    }
}

pub(super) fn command(value: engine::WorkflowControlCommand) -> api::WorkflowControlCommand {
    api::WorkflowControlCommand {
        id: value.id,
        revision_id: value.revision_id,
        control_revision: value.control_revision,
        created_at: value.created_at,
        target: output_target(value.target),
        operation: output_operation(value.operation),
        status: match value.status {
            engine::WorkflowControlStatus::Pending => api::WorkflowControlCommandStatus::Pending,
            engine::WorkflowControlStatus::Applied => api::WorkflowControlCommandStatus::Applied,
            engine::WorkflowControlStatus::Rejected => api::WorkflowControlCommandStatus::Rejected,
            engine::WorkflowControlStatus::Blocked => api::WorkflowControlCommandStatus::Blocked,
        },
        error: value.error,
    }
}

pub(super) fn revision(value: engine::WorkflowRevision) -> api::WorkflowRevision {
    api::WorkflowRevision {
        id: value.id,
        parent_id: value.parent_id,
        number: value.number,
        execution_ids: value.execution_ids,
        created_at: value.created_at,
        source: value.source,
        input: value.input,
        metadata: conversion::metadata(value.metadata),
        reusable_statements: value.reusable_statements,
        statement_count: value.statement_count,
        invalidation_reason: value.invalidation_reason,
        status: revision_status(value.status),
        mode: value.mode.map(|mode| match mode {
            engine::RevisionApplyMode::DrainAccepted => {
                api::WorkflowRevisionApplyMode::DrainAccepted
            }
            engine::RevisionApplyMode::InterruptAndApply => {
                api::WorkflowRevisionApplyMode::InterruptAndApply
            }
        }),
        error: value.error,
    }
}

pub(super) fn execution(
    value: engine::WorkflowExecutionSnapshot,
) -> api::WorkflowExecutionSnapshot {
    api::WorkflowExecutionSnapshot {
        run_id: value.run_id,
        execution_id: value.execution_id,
        revision_id: value.revision_id,
        source: value.source,
        input: value.input,
        metadata: value.metadata.map(conversion::metadata),
        completion_mode: match value.completion_mode {
            engine::WorkflowCompletionMode::Display => api::WorkflowCompletionMode::Display,
            engine::WorkflowCompletionMode::Continue => api::WorkflowCompletionMode::Continue,
        },
        status: conversion::status(value.status),
        partial: value.partial,
        result: result(value.result),
        error: value.error,
        metrics: conversion::metrics(value.metrics),
        created_at: value.created_at,
    }
}
