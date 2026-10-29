//! Model fields stay snake_case while engine requests retain their public typed contract.
use codex_dynamic_workflows::RevisionApplyMode;
use codex_dynamic_workflows::RevisionApplyRequest;
use codex_dynamic_workflows::RevisionResumeRequest;
use codex_dynamic_workflows::RevisionStageRequest;
use codex_dynamic_workflows::WorkflowControlOperation;
use codex_dynamic_workflows::WorkflowControlRequest;
use codex_dynamic_workflows::WorkflowControlTarget;
use codex_dynamic_workflows::WorkflowResult;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Target {
    Run,
    Phase {
        title: String,
    },
    Step {
        logical_id: String,
        expected_attempt_id: Option<String>,
        expected_turn_id: Option<String>,
    },
    FutureStep {
        scope: String,
        key: String,
    },
}

impl From<Target> for WorkflowControlTarget {
    fn from(target: Target) -> Self {
        match target {
            Target::Run => Self::Run,
            Target::Phase { title } => Self::Phase { title },
            Target::Step {
                logical_id,
                expected_attempt_id,
                expected_turn_id,
            } => Self::Step {
                logical_id,
                expected_attempt_id,
                expected_turn_id,
            },
            Target::FutureStep { scope, key } => Self::FutureStep { scope, key },
        }
    }
}

struct Mutation {
    run_id: String,
    command_id: String,
    expected_control_revision: u64,
    target: Target,
}

impl Mutation {
    fn request(self, operation: WorkflowControlOperation) -> ControlCommand {
        ControlCommand::Submit {
            run_id: self.run_id,
            request: WorkflowControlRequest {
                command_id: self.command_id,
                expected_control_revision: self.expected_control_revision,
                target: self.target.into(),
                operation,
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum ControlArguments {
    Read {
        run_id: String,
        offset: Option<usize>,
        target: Option<Target>,
    },
    Guide {
        run_id: String,
        command_id: String,
        expected_control_revision: u64,
        target: Target,
        message: String,
    },
    EditQueued {
        run_id: String,
        command_id: String,
        expected_control_revision: u64,
        target: Target,
        prompt: String,
    },
    Skip {
        run_id: String,
        command_id: String,
        expected_control_revision: u64,
        target: Target,
        #[serde(default, deserialize_with = "super::present_args")]
        replacement: Option<Value>,
    },
    Interrupt {
        run_id: String,
        command_id: String,
        expected_control_revision: u64,
        target: Target,
    },
}

pub(super) enum ControlCommand {
    Read {
        run_id: String,
        offset: usize,
        target: Option<WorkflowControlTarget>,
    },
    Submit {
        run_id: String,
        request: WorkflowControlRequest,
    },
}

pub(super) fn parse_control(arguments: &str) -> Result<ControlCommand, String> {
    let command = match serde_json::from_str(arguments).map_err(|error| error.to_string())? {
        ControlArguments::Read {
            run_id,
            offset,
            target,
        } => ControlCommand::Read {
            run_id,
            offset: offset.unwrap_or(0),
            target: target.map(Into::into),
        },
        ControlArguments::Guide {
            run_id,
            command_id,
            expected_control_revision,
            target,
            message,
        } => Mutation {
            run_id,
            command_id,
            expected_control_revision,
            target,
        }
        .request(WorkflowControlOperation::Guide { message }),
        ControlArguments::EditQueued {
            run_id,
            command_id,
            expected_control_revision,
            target,
            prompt,
        } => Mutation {
            run_id,
            command_id,
            expected_control_revision,
            target,
        }
        .request(WorkflowControlOperation::EditQueued { prompt }),
        ControlArguments::Skip {
            run_id,
            command_id,
            expected_control_revision,
            target,
            replacement,
        } => Mutation {
            run_id,
            command_id,
            expected_control_revision,
            target,
        }
        .request(WorkflowControlOperation::Skip {
            replacement: replacement.map_or(WorkflowResult::Absent, |value| {
                WorkflowResult::Value { value }
            }),
        }),
        ControlArguments::Interrupt {
            run_id,
            command_id,
            expected_control_revision,
            target,
        } => Mutation {
            run_id,
            command_id,
            expected_control_revision,
            target,
        }
        .request(WorkflowControlOperation::Interrupt),
    };
    if let ControlCommand::Submit { request, .. } = &command {
        request.validate()?;
    }
    Ok(command)
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ApplyMode {
    #[default]
    DrainAccepted,
    InterruptAndApply,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum RevisionArguments {
    List {
        run_id: String,
        offset: Option<usize>,
    },
    Stage {
        run_id: String,
        command_id: String,
        expected_control_revision: u64,
        source: String,
        #[serde(default, deserialize_with = "super::present_args")]
        args: Option<Value>,
    },
    Apply {
        run_id: String,
        command_id: String,
        expected_control_revision: u64,
        revision_id: String,
        #[serde(default)]
        mode: ApplyMode,
    },
    Resume {
        run_id: String,
        command_id: String,
        expected_control_revision: u64,
        revision_id: String,
    },
}

pub(super) enum RevisionCommand {
    List {
        run_id: String,
        offset: usize,
    },
    Stage {
        run_id: String,
        request: RevisionStageRequest,
    },
    Apply {
        run_id: String,
        request: RevisionApplyRequest,
    },
    Resume {
        run_id: String,
        request: RevisionResumeRequest,
    },
}

pub(super) fn parse_revision(arguments: &str) -> Result<RevisionCommand, serde_json::Error> {
    Ok(match serde_json::from_str(arguments)? {
        RevisionArguments::List { run_id, offset } => RevisionCommand::List {
            run_id,
            offset: offset.unwrap_or(0),
        },
        RevisionArguments::Stage {
            run_id,
            command_id,
            expected_control_revision,
            source,
            args,
        } => RevisionCommand::Stage {
            run_id,
            request: RevisionStageRequest {
                command_id,
                expected_control_revision,
                source,
                input: args.map_or(WorkflowResult::Absent, |value| WorkflowResult::Value {
                    value,
                }),
            },
        },
        RevisionArguments::Apply {
            run_id,
            command_id,
            expected_control_revision,
            revision_id,
            mode,
        } => RevisionCommand::Apply {
            run_id,
            request: RevisionApplyRequest {
                command_id,
                expected_control_revision,
                revision_id,
                mode: match mode {
                    ApplyMode::DrainAccepted => RevisionApplyMode::DrainAccepted,
                    ApplyMode::InterruptAndApply => RevisionApplyMode::InterruptAndApply,
                },
            },
        },
        RevisionArguments::Resume {
            run_id,
            command_id,
            expected_control_revision,
            revision_id,
        } => RevisionCommand::Resume {
            run_id,
            request: RevisionResumeRequest {
                command_id,
                expected_control_revision,
                revision_id,
            },
        },
    })
}

#[cfg(test)]
#[path = "control_arguments_tests.rs"]
mod tests;
