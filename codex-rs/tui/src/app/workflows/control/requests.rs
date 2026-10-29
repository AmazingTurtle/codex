//! Typed RPCs retain the displayed caller instead of borrowing the dashboard owner's authority.

use super::*;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol as api;

fn request_id() -> api::RequestId {
    api::RequestId::String(Uuid::new_v4().to_string())
}

pub(super) async fn actions(
    handle: &AppServerRequestHandle,
    scope: ActionScope,
) -> Result<ActionSnapshot, String> {
    let active_target = match &scope.target {
        ActionTarget::Step {
            logical_id,
            expected_attempt_id: Some(attempt),
            ..
        } => Some(api::WorkflowControlTarget::Step {
            logical_id: logical_id.clone(),
            expected_attempt_id: Some(attempt.clone()),
            expected_turn_id: None,
        }),
        _ => None,
    };
    let controls: api::WorkflowControlListResponse = handle
        .request_typed(ClientRequest::WorkflowControlList {
            request_id: request_id(),
            params: api::WorkflowControlListParams {
                thread_id: scope.caller.to_string(),
                workflow_id: scope.workflow_id.clone(),
                active_target,
                cursor: None,
                limit: Some(1),
            },
        })
        .await
        .map_err(|error| error.to_string())?;
    // Read the pinned execution after its guards. A concurrent replacement then
    // fails context's execution check instead of pairing old source with new guards.
    let run: api::WorkflowReadResponse = handle
        .request_typed(ClientRequest::WorkflowRead {
            request_id: request_id(),
            params: api::WorkflowReadParams {
                thread_id: scope.caller.to_string(),
                workflow_id: scope.workflow_id.clone(),
            },
        })
        .await
        .map_err(|error| error.to_string())?;
    actions::context(
        scope,
        run.workflow,
        controls.state,
        &controls.active_targets,
    )
}

pub(super) async fn mutate(
    handle: &AppServerRequestHandle,
    intent: &WorkflowUiIntent,
) -> Result<MutationReceipt, String> {
    let scope = intent_scope(intent);
    let command_id = Uuid::new_v4().to_string();
    let (revision, state) = match intent {
        WorkflowUiIntent::Control { operation, .. } => {
            let response: api::WorkflowControlResponse = handle
                .request_typed(ClientRequest::WorkflowControl {
                    request_id: request_id(),
                    params: api::WorkflowControlParams {
                        thread_id: scope.caller.to_string(),
                        workflow_id: scope.workflow_id.clone(),
                        command_id,
                        expected_control_revision: scope.expected_control_revision,
                        target: scope.target.clone().into(),
                        operation: operation.clone().into(),
                    },
                })
                .await
                .map_err(|error| error.to_string())?;
            if matches!(
                response.command.status,
                api::WorkflowControlCommandStatus::Rejected
                    | api::WorkflowControlCommandStatus::Blocked
            ) {
                return Err(response.command.error.unwrap_or_else(|| {
                    "The workflow command was not applied. Inspect its receipt before retrying."
                        .into()
                }));
            }
            (None, Some(response.state))
        }
        WorkflowUiIntent::StageRevision { source, input, .. } => {
            let response: api::WorkflowRevisionStageResponse = handle
                .request_typed(ClientRequest::WorkflowRevisionStage {
                    request_id: request_id(),
                    params: api::WorkflowRevisionStageParams {
                        thread_id: scope.caller.to_string(),
                        workflow_id: scope.workflow_id.clone(),
                        command_id,
                        expected_control_revision: scope.expected_control_revision,
                        source: source.clone(),
                        input: api::WorkflowResult::Value {
                            value: input.clone(),
                        },
                    },
                })
                .await
                .map_err(|error| error.to_string())?;
            (Some(response.revision.into()), Some(response.state))
        }
        WorkflowUiIntent::ApplyRevision {
            revision_id, mode, ..
        } => {
            let response: api::WorkflowRevisionApplyResponse = handle
                .request_typed(ClientRequest::WorkflowRevisionApply {
                    request_id: request_id(),
                    params: api::WorkflowRevisionApplyParams {
                        thread_id: scope.caller.to_string(),
                        workflow_id: scope.workflow_id.clone(),
                        command_id,
                        expected_control_revision: scope.expected_control_revision,
                        revision_id: revision_id.clone(),
                        mode: Some((*mode).into()),
                    },
                })
                .await
                .map_err(|error| error.to_string())?;
            (Some(response.revision.into()), Some(response.state))
        }
        WorkflowUiIntent::Resume { revision_id, .. } => {
            let response: api::WorkflowRevisionResumeResponse = handle
                .request_typed(ClientRequest::WorkflowRevisionResume {
                    request_id: request_id(),
                    params: api::WorkflowRevisionResumeParams {
                        thread_id: scope.caller.to_string(),
                        workflow_id: scope.workflow_id.clone(),
                        command_id,
                        expected_control_revision: scope.expected_control_revision,
                        revision_id: revision_id.clone(),
                    },
                })
                .await
                .map_err(|error| error.to_string())?;
            (Some(response.revision.into()), Some(response.state))
        }
        WorkflowUiIntent::ShowResult { .. } => {
            handle
                .request_typed::<api::WorkflowResultDisplayResponse>(
                    ClientRequest::WorkflowResultDisplay {
                        request_id: request_id(),
                        params: api::WorkflowResultDisplayParams {
                            thread_id: scope.caller.to_string(),
                            workflow_id: scope.workflow_id.clone(),
                            execution_id: Some(scope.execution_id.clone()),
                        },
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            (None, None)
        }
        WorkflowUiIntent::OpenForm { .. }
        | WorkflowUiIntent::RevisionArguments { .. }
        | WorkflowUiIntent::RevisionHistory { .. }
        | WorkflowUiIntent::InspectRevision { .. }
        | WorkflowUiIntent::ViewRevisionSource { .. }
        | WorkflowUiIntent::SetFooterVisibility { .. }
        | WorkflowUiIntent::ViewExecution { .. } => {
            return Err("This workflow action does not submit a mutation".into());
        }
    };
    Ok(MutationReceipt { revision, state })
}

pub(super) async fn revisions(
    handle: &AppServerRequestHandle,
    scope: &ActionScope,
    cursor: Option<String>,
) -> Result<RevisionPage, String> {
    let response: api::WorkflowRevisionListResponse = handle
        .request_typed(ClientRequest::WorkflowRevisionList {
            request_id: request_id(),
            params: api::WorkflowRevisionListParams {
                thread_id: scope.caller.to_string(),
                workflow_id: scope.workflow_id.clone(),
                cursor,
                limit: Some(10),
            },
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(RevisionPage {
        data: response.data.into_iter().map(Into::into).collect(),
        next_cursor: response.next_cursor,
    })
}

pub(super) async fn execution(
    handle: &AppServerRequestHandle,
    scope: &ActionScope,
    revision_id: &str,
    execution_id: &str,
) -> Result<api::WorkflowExecutionSnapshot, String> {
    let response: api::WorkflowExecutionReadResponse = handle
        .request_typed(ClientRequest::WorkflowExecutionRead {
            request_id: request_id(),
            params: api::WorkflowExecutionReadParams {
                thread_id: scope.caller.to_string(),
                workflow_id: scope.workflow_id.clone(),
                execution_id: execution_id.to_owned(),
            },
        })
        .await
        .map_err(|error| error.to_string())?;
    if response.execution.run_id != scope.workflow_id
        || response.execution.execution_id != execution_id
        || response.execution.revision_id.as_deref() != Some(revision_id)
    {
        return Err("The returned execution does not belong to the selected revision".into());
    }
    Ok(response.execution)
}
