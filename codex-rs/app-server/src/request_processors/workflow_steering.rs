//! Control mutations retain the caller; historical inspection never activates a service.
use super::steering_conversion as convert;
use super::*;
use codex_core::workflows as engine;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;

#[derive(Deserialize, Serialize)]
struct HistoryCursor {
    workflow_id: String,
    kind: HistoryKind,
    offset: usize,
}

#[derive(Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
enum HistoryKind {
    Controls,
    Revisions,
}

impl WorkflowRequestProcessor {
    async fn mutation_caller(
        &self,
        thread: &str,
    ) -> Result<Arc<codex_core::CodexThread>, JSONRPCErrorError> {
        let id =
            ThreadId::from_string(thread).map_err(|error| invalid_request(error.to_string()))?;
        let caller = self
            .thread_manager
            .get_thread(id)
            .await
            .map_err(|_| invalid_request(format!("thread not found: {id}")))?;
        super::super::thread_input::ensure_user_input_allowed(&caller).await?;
        Ok(caller)
    }

    pub(super) async fn cold_run(
        &self,
        thread: &str,
        workflow: &str,
    ) -> Result<(AbsolutePathBuf, engine::WorkflowRun), JSONRPCErrorError> {
        let id =
            ThreadId::from_string(thread).map_err(|error| invalid_request(error.to_string()))?;
        let root = self.persisted_root(id).await?;
        let path = self
            .config
            .codex_home
            .join("workflow-runs")
            .join(root.to_string());
        let run = engine::read_persisted(&path, workflow).map_err(invalid_request)?;
        if run.owner_thread_id != root.to_string() {
            return Err(invalid_request(
                "workflow is not owned by this conversation",
            ));
        }
        Ok((path, run))
    }

    pub(crate) async fn control(
        &self,
        params: api::WorkflowControlParams,
    ) -> Result<api::WorkflowControlResponse, JSONRPCErrorError> {
        let response = self
            .mutation_caller(&params.thread_id)
            .await?
            .workflow_control(
                &params.workflow_id,
                engine::WorkflowControlRequest {
                    command_id: params.command_id,
                    expected_control_revision: params.expected_control_revision,
                    target: convert::target(params.target),
                    operation: convert::operation(params.operation),
                },
            )
            .await
            .map_err(invalid_request)?;
        Ok(api::WorkflowControlResponse {
            command: convert::command(response.command),
            state: convert::state(response.state),
        })
    }

    pub(crate) async fn revision_stage(
        &self,
        params: api::WorkflowRevisionStageParams,
    ) -> Result<api::WorkflowRevisionStageResponse, JSONRPCErrorError> {
        let response = self
            .mutation_caller(&params.thread_id)
            .await?
            .stage_workflow_revision(
                &params.workflow_id,
                engine::RevisionStageRequest {
                    command_id: params.command_id,
                    expected_control_revision: params.expected_control_revision,
                    source: params.source,
                    input: convert::input(params.input),
                },
            )
            .await
            .map_err(invalid_request)?;
        Ok(api::WorkflowRevisionStageResponse {
            revision: convert::revision(response.revision),
            state: convert::state(response.state),
        })
    }

    pub(crate) async fn revision_apply(
        &self,
        params: api::WorkflowRevisionApplyParams,
    ) -> Result<api::WorkflowRevisionApplyResponse, JSONRPCErrorError> {
        let response = self
            .mutation_caller(&params.thread_id)
            .await?
            .apply_workflow_revision(
                &params.workflow_id,
                engine::RevisionApplyRequest {
                    command_id: params.command_id,
                    expected_control_revision: params.expected_control_revision,
                    revision_id: params.revision_id,
                    mode: convert::mode(params.mode.unwrap_or_default()),
                },
            )
            .await
            .map_err(invalid_request)?;
        Ok(api::WorkflowRevisionApplyResponse {
            revision: convert::revision(response.revision),
            state: convert::state(response.state),
        })
    }

    pub(crate) async fn revision_resume(
        &self,
        params: api::WorkflowRevisionResumeParams,
    ) -> Result<api::WorkflowRevisionResumeResponse, JSONRPCErrorError> {
        let response = self
            .mutation_caller(&params.thread_id)
            .await?
            .resume_workflow_revision(
                &params.workflow_id,
                engine::RevisionResumeRequest {
                    command_id: params.command_id,
                    expected_control_revision: params.expected_control_revision,
                    revision_id: params.revision_id,
                },
            )
            .await
            .map_err(invalid_request)?;
        Ok(api::WorkflowRevisionResumeResponse {
            revision: convert::revision(response.revision),
            state: convert::state(response.state),
        })
    }

    pub(crate) async fn control_list(
        &self,
        params: api::WorkflowControlListParams,
    ) -> Result<api::WorkflowControlListResponse, JSONRPCErrorError> {
        let (_, run) = self
            .cold_run(&params.thread_id, &params.workflow_id)
            .await?;
        let (offset, limit) =
            history_page(&run.id, HistoryKind::Controls, params.cursor, params.limit)?;
        let selected = match params.active_target {
            Some(api::WorkflowControlTarget::Step {
                logical_id,
                expected_attempt_id: Some(attempt_id),
                ..
            }) => Some((logical_id, attempt_id)),
            Some(
                api::WorkflowControlTarget::Run
                | api::WorkflowControlTarget::Phase { .. }
                | api::WorkflowControlTarget::FutureStep { .. }
                | api::WorkflowControlTarget::Step {
                    expected_attempt_id: None,
                    ..
                },
            ) => {
                return Err(invalid_request(
                    "activeTarget requires an exact step logicalId and expectedAttemptId",
                ));
            }
            None => None,
        };
        if selected.as_ref().is_some_and(|(logical, attempt)| {
            logical.is_empty() || logical.len() > 2048 || attempt.is_empty() || attempt.len() > 128
        }) {
            return Err(invalid_request(
                "activeTarget contains an invalid step or attempt identity",
            ));
        }
        let mut active_targets = Vec::new();
        let owner = ThreadId::from_string(&run.owner_thread_id)
            .map_err(|error| invalid_request(error.to_string()))?;
        if (selected.is_some() || offset == 0)
            && let Ok(thread) = self.thread_manager.get_thread(owner).await
        {
            let mut steps = run.steps.values().collect::<Vec<_>>();
            steps.sort_by_key(|step| (step.worker_sequence, &step.id));
            for step in steps {
                let Some(attempt) = step.attempts.last() else {
                    continue;
                };
                if selected
                    .as_ref()
                    .is_some_and(|(logical, id)| logical != &step.logical_id || id != &attempt.id)
                {
                    continue;
                }
                if attempt.admission != engine::AdmissionStatus::Accepted
                    || !matches!(
                        attempt.status,
                        engine::WorkflowStatus::Running | engine::WorkflowStatus::Interrupted
                    )
                {
                    continue;
                }
                if let Some(turn_id) = thread
                    .active_workflow_owned_turn(&run.id, &step.logical_id, &attempt.id)
                    .await
                    .map_err(invalid_request)?
                {
                    active_targets.push(api::WorkflowControlActiveTarget {
                        logical_id: step.logical_id.clone(),
                        attempt_id: attempt.id.clone(),
                        turn_id,
                    });
                    if selected.is_some() || active_targets.len() == limit {
                        break;
                    }
                }
            }
        }
        let mut commands = run.steering.commands;
        commands.sort_by(|a, b| (a.control_revision, &a.id).cmp(&(b.control_revision, &b.id)));
        let next_cursor = history_next(
            &run.id,
            HistoryKind::Controls,
            offset,
            limit,
            commands.len(),
        )?;
        Ok(api::WorkflowControlListResponse {
            data: commands
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(convert::command)
                .collect(),
            next_cursor,
            state: convert::state(run.steering.state),
            active_targets,
        })
    }

    pub(crate) async fn revision_list(
        &self,
        params: api::WorkflowRevisionListParams,
    ) -> Result<api::WorkflowRevisionListResponse, JSONRPCErrorError> {
        let (_, run) = self
            .cold_run(&params.thread_id, &params.workflow_id)
            .await?;
        let (offset, limit) =
            history_page(&run.id, HistoryKind::Revisions, params.cursor, params.limit)?;
        let mut revisions = run.steering.revisions.into_values().collect::<Vec<_>>();
        revisions.sort_by(|a, b| (a.number, &a.id).cmp(&(b.number, &b.id)));
        let next_cursor = history_next(
            &run.id,
            HistoryKind::Revisions,
            offset,
            limit,
            revisions.len(),
        )?;
        Ok(api::WorkflowRevisionListResponse {
            data: revisions
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(convert::revision)
                .collect(),
            next_cursor,
            state: convert::state(run.steering.state),
        })
    }

    pub(crate) async fn execution_read(
        &self,
        params: api::WorkflowExecutionReadParams,
    ) -> Result<api::WorkflowExecutionReadResponse, JSONRPCErrorError> {
        let (path, _) = self
            .cold_run(&params.thread_id, &params.workflow_id)
            .await?;
        let snapshot =
            engine::read_execution_snapshot(&path, &params.workflow_id, &params.execution_id)
                .map_err(invalid_request)?;
        Ok(api::WorkflowExecutionReadResponse {
            execution: convert::execution(snapshot),
        })
    }
}

fn history_page(
    workflow: &str,
    kind: HistoryKind,
    cursor: Option<String>,
    limit: Option<u32>,
) -> Result<(usize, usize), JSONRPCErrorError> {
    let limit = limit.unwrap_or(20);
    if !(1..=50).contains(&limit) {
        return Err(invalid_request(
            "workflow history limit must be between 1 and 50",
        ));
    }
    let offset = match cursor {
        Some(cursor) => {
            if cursor.len() > 4096 {
                return Err(invalid_request("invalid workflow history cursor"));
            }
            let cursor: HistoryCursor = serde_json::from_str(&cursor)
                .map_err(|_| invalid_request("invalid workflow history cursor"))?;
            if cursor.workflow_id != workflow || cursor.kind != kind {
                return Err(invalid_request(
                    "workflow history cursor belongs to another run or list",
                ));
            }
            cursor.offset
        }
        None => 0,
    };
    Ok((offset, limit as usize))
}

fn history_next(
    workflow: &str,
    kind: HistoryKind,
    offset: usize,
    limit: usize,
    total: usize,
) -> Result<Option<String>, JSONRPCErrorError> {
    let offset = offset.saturating_add(limit);
    (offset < total)
        .then(|| {
            serde_json::to_string(&HistoryCursor {
                workflow_id: workflow.to_owned(),
                kind,
                offset,
            })
        })
        .transpose()
        .map_err(|error| internal_error(error.to_string()))
}
