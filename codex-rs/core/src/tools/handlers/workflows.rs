//! Bounded model-facing views over the conversation's durable workflow service.

use crate::function_tool::FunctionCallError;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::multi_agents_common::function_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use crate::tools::registry::ToolRegistry;
mod control_arguments;
mod control_definitions;
mod control_handler;
mod control_output;
mod output;
mod read;

use codex_features::DynamicWorkflowsMode;
use codex_features::Feature;
use codex_models_manager::dynamic_workflows::DEFAULT_CONTROL_DESCRIPTION;
use codex_models_manager::dynamic_workflows::DEFAULT_LAUNCH_DESCRIPTION;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ToolMessage;
use codex_protocol::protocol::MultiAgentVersion;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use codex_tools::with_catalog_tool_message;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::truncate_text;
use output::MAX_OUTPUT_BYTES;
use output::bounded_page;
use output::content_page;
use output::preview;
use output::summary;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;

const DECLARATIONS: &str = "Pass one-off source inline; no temporary script file is needed. Canonical JavaScript starts with workflow.meta({name: 'name', description: 'purpose'}) using a literal object. Read $dynamic-workflow and its authoring reference. The callable workflow(name, args) runs a saved workflow. Methods: agent(prompt, options?), parallel(thunks), pipeline(items, ...stages), phase(title), log(message); workflow.budget has total, spent(), remaining(). Read workflow.args for input and return the final value. Agent options: key, label, phase, schema (optional), model, reasoningEffort, agentType, isolation: 'worktree'. Scripts coordinate agents; workers retain native tools and permissions and may return prose or Markdown. Legacy workflow.input/agent(stepId, ...)/publish scripts are read-only and cannot run.";
const MAX_TOOL_DEFINITION_BYTES: usize = 8000;

pub(crate) fn register(
    session: &Session,
    turn: &TurnContext,
    model: &ModelInfo,
    registry: &mut ToolRegistry,
) {
    if !session.services.code_mode_service.is_available()
        || turn.config.ephemeral
        || turn.config.multi_agent_v2.disable_native_tools
        || turn.multi_agent_version != MultiAgentVersion::V2
        || model.multi_agent_version == Some(MultiAgentVersion::Disabled)
        || (turn.session_source.get_agent_path().is_some()
            && model.multi_agent_version != Some(MultiAgentVersion::V2))
        || crate::agent::profile::is_leaf_worker(&turn.config, &turn.session_source)
    {
        return;
    }
    let mode = turn.config.dynamic_workflows_mode;
    let launch = turn.config.features.enabled(Feature::DynamicWorkflows)
        && match mode {
            DynamicWorkflowsMode::Auto => true,
            DynamicWorkflowsMode::Explicit => session.services.workflows.activated(&turn.sub_id),
            DynamicWorkflowsMode::Off => false,
        };
    let messages = model
        .model_messages
        .as_ref()
        .and_then(|messages| messages.tools.as_ref())
        .and_then(|tools| tools.dynamic_workflows.as_ref());
    for operation in Operation::ALL {
        if operation == Operation::Start && !launch {
            continue;
        }
        let catalog = messages.and_then(|messages| {
            if operation == Operation::Start {
                messages.launch.as_ref()
            } else {
                messages.control.as_ref()
            }
        });
        registry.add(Handler::new(operation, catalog));
    }
    if let Err(error) = control_handler::register(registry) {
        tracing::error!(%error, "workflow control tools could not be registered");
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Start,
    Read,
    List,
    Stop,
    Retry,
    Save,
    SavedList,
    SavedRead,
    SavedDelete,
}

impl Operation {
    const ALL: [Self; 9] = [
        Self::Start,
        Self::Read,
        Self::List,
        Self::Stop,
        Self::Retry,
        Self::Save,
        Self::SavedList,
        Self::SavedRead,
        Self::SavedDelete,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::Start => "workflow_start",
            Self::Read => "workflow_read",
            Self::List => "workflow_list",
            Self::Stop => "workflow_stop",
            Self::Retry => "workflow_retry",
            Self::Save => "workflow_save",
            Self::SavedList => "saved_workflow_list",
            Self::SavedRead => "saved_workflow_read",
            Self::SavedDelete => "saved_workflow_delete",
        }
    }
}

struct Handler {
    operation: Operation,
    spec: ToolSpec,
}

impl Handler {
    #[expect(
        clippy::expect_used,
        reason = "built-in workflow schemas are authored JSON objects"
    )]
    fn new(operation: Operation, catalog: Option<&ToolMessage>) -> Self {
        let schema = match operation {
            Operation::Start => {
                json!({"type":"object","properties":{"source":{"type":"string","description":"Inline canonical workflow.meta JavaScript source text, not a file path; mutually exclusive with saved_workflow_id."},"saved_workflow_id":{"type":"string"},"args":{"description":"Structured workflow input; omitted means an empty object."}},"additionalProperties":false})
            }
            Operation::Read => {
                json!({"type":"object","properties":{"run_id":{"type":"string"},"section":{"type":"string","enum":["metadata","result","source","args"]},"execution_id":{"type":"string","description":"Exact known historical execution for source or args; omitted selects current execution."},"offset":{"type":"integer","minimum":0}},"required":["run_id"],"additionalProperties":false})
            }
            Operation::Stop | Operation::Retry => {
                json!({"type":"object","properties":{"run_id":{"type":"string"}},"required":["run_id"],"additionalProperties":false})
            }
            Operation::Save => {
                json!({"type":"object","properties":{"run_id":{"type":"string"},"name":{"type":"string"},"scope":{"type":"string","enum":["project","user"]}},"required":["run_id","name","scope"],"additionalProperties":false})
            }
            Operation::List | Operation::SavedList => {
                json!({"type":"object","properties":{"offset":{"type":"integer","minimum":0}},"additionalProperties":false})
            }
            Operation::SavedRead => {
                json!({"type":"object","properties":{"saved_workflow_id":{"type":"string"},"offset":{"type":"integer","minimum":0}},"required":["saved_workflow_id"],"additionalProperties":false})
            }
            Operation::SavedDelete => {
                json!({"type":"object","properties":{"saved_workflow_id":{"type":"string"}},"required":["saved_workflow_id"],"additionalProperties":false})
            }
        };
        // Catalog prose and schema are bounded before they enter the model's tool context.
        let catalog = catalog.map(|message| ToolMessage {
            description: message
                .description
                .as_ref()
                .filter(|text| text.len() <= 1024)
                .cloned(),
            parameters: message
                .parameters
                .as_ref()
                .filter(|text| text.len() <= 2048)
                .cloned(),
        });
        let bundled = ResponsesApiTool {
            name: operation.name().into(),
            description: if operation == Operation::Start {
                DEFAULT_LAUNCH_DESCRIPTION
            } else {
                DEFAULT_CONTROL_DESCRIPTION
            }
            .into(),
            strict: false,
            defer_loading: None,
            parameters: serde_json::from_value(schema).expect("valid built-in workflow schema"),
            output_schema: None,
        };
        let complete = |mut tool: ResponsesApiTool| {
            if operation == Operation::Start {
                tool.description.push('\n');
                tool.description.push_str(DECLARATIONS);
            }
            ToolSpec::Function(tool)
        };
        let customized = complete(with_catalog_tool_message(bundled.clone(), catalog.as_ref()));
        // Measure the actual wire definition: JSON escaping and normalized numbers
        // can expand well beyond the catalog's raw description/schema byte limits.
        let spec = if serde_json::to_vec(&customized)
            .is_ok_and(|bytes| bytes.len() <= MAX_TOOL_DEFINITION_BYTES)
        {
            customized
        } else {
            complete(bundled)
        };
        Self { operation, spec }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    source: Option<String>,
    saved_workflow_id: Option<String>,
    #[serde(default, alias = "input", deserialize_with = "present_args")]
    args: Option<Value>,
    run_id: Option<String>,
    offset: Option<usize>,
    name: Option<String>,
    scope: Option<crate::workflows::WorkflowScope>,
    section: Option<ReadSection>,
    execution_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReadSection {
    Metadata,
    Result,
    Source,
    Args,
}

fn present_args<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

#[cfg(test)]
#[path = "workflows/arguments_tests.rs"]
mod arguments_tests;

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(self.operation.name())
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
            let args: Arguments = super::parse_arguments(&arguments).map_err(bound_error)?;
            let operation = self.operation;
            let result: Result<Value, String> = async {
                let service = crate::workflows::service(&invocation.session).await?;
                let offset = args.offset.unwrap_or(0);
                let required = |value: Option<String>, field: &str| value.ok_or_else(|| format!("{field} is required"));
                match operation {
                    Operation::Start => {
                        let turn = &invocation.step_context.turn;
                        if turn.config.dynamic_workflows_mode == DynamicWorkflowsMode::Explicit
                            && !invocation.session.services.workflows.activated(&turn.sub_id)
                        { return Err("invoke the dynamic-workflow skill in the current human turn before launching a workflow".into()); }
                        let source = match (args.source, args.saved_workflow_id) {
                            (Some(source), None) => source,
                            (None, Some(id)) => service.load_saved_from(&invocation.session, &id).await?.source,
                            _ => return Err("provide exactly one of source or saved_workflow_id".into()),
                        };
                        Ok(summary(&service.start_from(invocation.session.clone(), source, args.args.unwrap_or_else(|| json!({})), crate::workflows::NotificationSource::Model).await?))
                    }
                    Operation::Read => {
                        let run = service.get(&required(args.run_id, "run_id")?)?;
                        if matches!(args.section, Some(ReadSection::Source | ReadSection::Args)) {
                            return read::content(&service, &run, args.section.ok_or("section is required")?, args.execution_id.as_deref(), offset);
                        }
                        if args.execution_id.is_some() { return Err("execution_id is supported for source and args pages only".into()); }
                        if matches!(args.section, Some(ReadSection::Result)) {
                            let result = run.result.as_ref().ok_or("workflow has no published result")?.to_string();
                            return Ok(json!({"run_id":run.id,"section":"result","encoding":"json","page":content_page(&result,offset,768)?}));
                        }
                        read::metadata(&service, &run, offset).await
                    }
                    Operation::List => {
                        let runs = service.list_summaries(offset,6)?;
                        bounded_page(json!({}), "runs", offset, runs.iter().take(5).map(output::run_summary).collect(), runs.len() > 5)
                    }
                    Operation::Stop => Ok(summary(&service.stop(&required(args.run_id,"run_id")?).await?)),
                    Operation::Retry => Ok(summary(&service.retry_from_model(&required(args.run_id,"run_id")?).await?)),
                    Operation::Save => {
                        let run = service.get(&required(args.run_id,"run_id")?)?;
                        let name = required(args.name,"name")?;
                        let scope = args.scope.ok_or("scope is required")?;
                        if scope == crate::workflows::WorkflowScope::Project { check_project_write(&invocation.step_context, &service.library().save_target(&name, scope)?, service.project_root())?; }
                        let saved = service.library().save(&name, &run.source, scope)?;
                        Ok(output::saved_summary(&saved))
                    }
                    Operation::SavedList => {
                        let workflows = service.library().list(offset,6)?;
                        bounded_page(json!({}), "workflows", offset, workflows.iter().take(5).map(output::saved_summary).collect(), workflows.len() > 5)
                    }
                    Operation::SavedRead => {
                        let saved = service.read_saved_from(&invocation.session, &required(args.saved_workflow_id,"saved_workflow_id")?).await?;
                        let mut response = output::saved_summary(&saved);
                        response["encoding"] = json!("javascript");
                        response["page"] = content_page(&saved.source,offset,384)?;
                        Ok(response)
                    }
                    Operation::SavedDelete => {
                        let id = required(args.saved_workflow_id,"saved_workflow_id")?;
                        let saved = service.library().read(&id)?;
                        if saved.scope == crate::workflows::WorkflowScope::Project { check_project_write(&invocation.step_context, &service.library().delete_target(&id)?, service.project_root())?; }
                        service.library().delete(&id)?;
                        Ok(json!({"deleted":true}))
                    }
                }
            }.await;
            let value =
                result.map_err(|error| bound_error(FunctionCallError::RespondToModel(error)))?;
            let output = serde_json::to_string(&value)
                .map_err(|err| bound_error(FunctionCallError::RespondToModel(err.to_string())))?;
            if output.len() > MAX_OUTPUT_BYTES {
                return Err(FunctionCallError::RespondToModel(
                    "workflow view exceeds its output budget; request a narrower content page"
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

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}

fn bound_error(error: FunctionCallError) -> FunctionCallError {
    FunctionCallError::RespondToModel(truncate_text(
        &error.to_string(),
        TruncationPolicy::Tokens(800),
    ))
}

fn check_project_write(
    step: &crate::session::step_context::StepContext,
    path: &codex_utils_absolute_path::AbsolutePathBuf,
    root: &codex_utils_absolute_path::AbsolutePathBuf,
) -> Result<(), String> {
    let environment = step
        .environments
        .single_local_environment()
        .ok_or("project workflow writes require one selected local environment")?;
    let sandbox = environment.sandbox_context(None);
    let context = sandbox.policy_context();
    let policy = environment
        .permission_profile()
        .file_system_sandbox_policy();
    let grants = step
        .turn
        .granted_permissions(&environment.selection().environment_id);
    let policy = codex_sandboxing::policy_transforms::effective_file_system_sandbox_policy(
        &policy,
        grants.as_ref(),
    );
    let matching = policy
        .prepare_local_matching(&context)
        .map_err(|err| err.to_string())?;
    // The managed writer is host-local, so reject symlink indirection rather than relying
    // on the executor sandbox that protects ordinary file tools after their policy check.
    for (index, target) in path
        .as_path()
        .ancestors()
        .take_while(|target| target.starts_with(root.as_path()))
        .enumerate()
    {
        let missing = match std::fs::symlink_metadata(target) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(
                    "model project workflow writes cannot follow symlinked library paths".into(),
                );
            }
            Ok(_) => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => return Err(error.to_string()),
        };
        let absolute = codex_utils_absolute_path::AbsolutePathBuf::try_from(target.to_path_buf())
            .map_err(|err| err.to_string())?;
        let uri = codex_utils_path_uri::PathUri::from_abs_path(&absolute);
        if (index <= 1 || missing)
            && !matching
                .can_write_path(&uri)
                .map_err(|err| err.to_string())?
        {
            return Err("the current filesystem permissions do not permit writing this project's workflow library".into());
        }
    }
    Ok(())
}
