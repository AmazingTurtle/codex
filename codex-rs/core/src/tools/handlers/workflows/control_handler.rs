//! Model controls bind to the same caller-aware coordinator as human API actions.
use super::control_arguments::ControlCommand;
use super::control_arguments::RevisionCommand;
use super::control_arguments::parse_control;
use super::control_arguments::parse_revision;
use super::control_output;
use super::*;

#[derive(Clone, Copy)]
enum Tool {
    Control,
    Revision,
}

pub(super) fn register(registry: &mut ToolRegistry) -> Result<(), String> {
    let [control, revision] = super::control_definitions::specs()?;
    registry.add(ControlHandler {
        tool: Tool::Control,
        spec: control,
    });
    registry.add(ControlHandler {
        tool: Tool::Revision,
        spec: revision,
    });
    Ok(())
}

struct ControlHandler {
    tool: Tool,
    spec: ToolSpec,
}

impl ToolExecutor<ToolInvocation> for ControlHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(self.spec.name())
    }
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            let arguments = function_arguments(invocation.payload).map_err(bound_error)?;
            let value: Result<Value, String> = async {
                let service = crate::workflows::service(&invocation.session).await?;
                match self.tool {
                    Tool::Control => match parse_control(&arguments)? {
                        ControlCommand::Read { run_id, offset, target } => {
                            let state = service.control_state(&run_id)?;
                            let commands = service.list_controls(&run_id, offset, 6)?;
                            bounded_page(json!({"state":control_output::state(&state),"target_context":target.map(|target|preview(&serde_json::to_string(&target).unwrap_or_default(),256))}),
                                "commands", offset, commands.iter().take(5).map(control_output::command).collect(), commands.len() > 5)
                        }
                        ControlCommand::Submit { run_id, request } => {
                            let response = service.control_from(Arc::clone(&invocation.session), &run_id, request).await?;
                            Ok(control_output::command_response(&response.command, &response.state))
                        }
                    },
                    Tool::Revision => match parse_revision(&arguments).map_err(|error| error.to_string())? {
                        RevisionCommand::List { run_id, offset } => {
                            let state = service.control_state(&run_id)?;
                            let revisions = service.list_revisions(&run_id, offset, 6)?;
                            bounded_page(json!({"state":control_output::state(&state)}), "revisions", offset,
                                revisions.iter().take(5).map(control_output::revision).collect(), revisions.len() > 5)
                        }
                        RevisionCommand::Stage { run_id, request } => {
                            let response = service.stage_revision_from(Arc::clone(&invocation.session), &run_id, request).await?;
                            Ok(control_output::revision_response(&response.revision, &response.state))
                        }
                        RevisionCommand::Apply { run_id, request } => {
                            let response = service.apply_revision_from(Arc::clone(&invocation.session), &run_id, request).await?;
                            Ok(control_output::revision_response(&response.revision, &response.state))
                        }
                        RevisionCommand::Resume { run_id, request } => {
                            let response = service.resume_revision_from(Arc::clone(&invocation.session), &run_id, request).await?;
                            Ok(control_output::revision_response(&response.revision, &response.state))
                        }
                    },
                }
            }.await;
            let value =
                value.map_err(|error| bound_error(FunctionCallError::RespondToModel(error)))?;
            let output = serde_json::to_string(&value)
                .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
            if output.len() > MAX_OUTPUT_BYTES {
                return Err(FunctionCallError::RespondToModel(
                    "workflow control response exceeds its output budget; read a narrower page"
                        .into(),
                ));
            }
            Ok(boxed_tool_output(FunctionToolOutput::from_text(
                output,
                Some(true),
            )))
        })
    }
}

impl CoreToolRuntime for ControlHandler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}

use std::sync::Arc;
