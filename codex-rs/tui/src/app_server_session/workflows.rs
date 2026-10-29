//! Bounded workflow requests owned by the TUI session transport.
use super::AppServerSession;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SavedWorkflowSummary;
use codex_app_server_protocol::ThreadExecutionControl;
use codex_app_server_protocol::ThreadExecutionControlReadParams;
use codex_app_server_protocol::ThreadExecutionControlReadResponse;
use codex_app_server_protocol::ThreadPauseParams;
use codex_app_server_protocol::ThreadPauseResponse;
use codex_app_server_protocol::ThreadUnpauseParams;
use codex_app_server_protocol::ThreadUnpauseResponse;
use codex_app_server_protocol::WorkflowListParams;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowListScope;
use codex_app_server_protocol::WorkflowReadParams;
use codex_app_server_protocol::WorkflowReadResponse;
use codex_app_server_protocol::WorkflowRecoverParams;
use codex_app_server_protocol::WorkflowRecoverResponse;
use codex_app_server_protocol::WorkflowRetryParams;
use codex_app_server_protocol::WorkflowRetryResponse;
use codex_app_server_protocol::WorkflowRun;
use codex_app_server_protocol::WorkflowSaveParams;
use codex_app_server_protocol::WorkflowSaveResponse;
use codex_app_server_protocol::WorkflowSavedListParams;
use codex_app_server_protocol::WorkflowSavedListResponse;
use codex_app_server_protocol::WorkflowScope;
use codex_app_server_protocol::WorkflowStartParams;
use codex_app_server_protocol::WorkflowStartResponse;
use codex_app_server_protocol::WorkflowStopParams;
use codex_app_server_protocol::WorkflowStopResponse;
use codex_app_server_protocol::WorkflowSummary;
use codex_protocol::ThreadId;

impl AppServerSession {
    pub(super) fn record_thread_input_capability(
        &mut self,
        thread: &codex_app_server_protocol::Thread,
    ) {
        let Ok(id) = ThreadId::from_string(&thread.id) else {
            return;
        };
        if thread.can_accept_direct_input == Some(true)
            && matches!(
                thread.source,
                codex_app_server_protocol::SessionSource::SubAgent(
                    codex_protocol::protocol::SubAgentSource::ThreadSpawn {
                        agent_path: Some(_),
                        ..
                    }
                )
            )
        {
            self.input_only_threads.insert(id);
        } else {
            self.input_only_threads.remove(&id);
        }
    }

    pub(crate) fn preserves_child_turn_settings(&self, thread_id: ThreadId) -> bool {
        self.input_only_threads.contains(&thread_id)
    }

    pub(crate) async fn child_human_turn_start(
        &mut self,
        thread_id: ThreadId,
        client_user_message_id: String,
        input: Vec<codex_app_server_protocol::UserInput>,
    ) -> color_eyre::Result<codex_app_server_protocol::TurnStartResponse> {
        let request_id = self.next_request_id();
        Ok(self
            .request_handle()
            .request_typed(ClientRequest::TurnStart {
                request_id,
                params: codex_app_server_protocol::TurnStartParams {
                    thread_id: thread_id.to_string(),
                    client_user_message_id: Some(client_user_message_id),
                    input,
                    ..Default::default()
                },
            })
            .await?)
    }
}

fn request_id() -> RequestId {
    RequestId::String(format!("workflow-{}", uuid::Uuid::new_v4()))
}

pub(crate) struct WorkflowListSnapshot {
    pub(crate) summaries: Vec<WorkflowSummary>,
    pub(crate) runs: Vec<WorkflowRun>,
    pub(crate) saved: Vec<SavedWorkflowSummary>,
    pub(crate) next_cursor: Option<String>,
    pub(crate) saved_next_cursor: Option<String>,
}

pub(crate) async fn list(
    handle: &AppServerRequestHandle,
    thread_id: ThreadId,
    scope: WorkflowListScope,
    cursor: Option<String>,
    saved_cursor: Option<String>,
    library_watch: &super::workflow_watch::LibraryWatch,
) -> Result<WorkflowListSnapshot, String> {
    let response: WorkflowListResponse = handle
        .request_typed(ClientRequest::WorkflowList {
            request_id: request_id(),
            params: WorkflowListParams {
                thread_id: thread_id.to_string(),
                scope: Some(scope),
                cursor,
                limit: Some(50),
            },
        })
        .await
        .map_err(|error| error.to_string())?;
    let mut workflows = Vec::with_capacity(response.data.len());
    for summary in &response.data {
        if summary.owner_thread_id != thread_id.to_string() {
            continue;
        }
        let response: WorkflowReadResponse = handle
            .request_typed(ClientRequest::WorkflowRead {
                request_id: request_id(),
                params: WorkflowReadParams {
                    thread_id: thread_id.to_string(),
                    workflow_id: summary.id.clone(),
                },
            })
            .await
            .map_err(|error| error.to_string())?;
        workflows.push(response.workflow);
    }
    let saved: WorkflowSavedListResponse = handle
        .request_typed(ClientRequest::WorkflowSavedList {
            request_id: request_id(),
            params: WorkflowSavedListParams {
                thread_id: thread_id.to_string(),
                cursor: saved_cursor,
                limit: Some(50),
            },
        })
        .await
        .map_err(|error| error.to_string())?;
    library_watch.update(saved.watch_roots).await;
    Ok(WorkflowListSnapshot {
        summaries: response.data,
        runs: workflows,
        saved: saved.data,
        next_cursor: response.next_cursor,
        saved_next_cursor: saved.next_cursor,
    })
}

pub(crate) async fn action(
    handle: &AppServerRequestHandle,
    thread_id: ThreadId,
    id: String,
    action: crate::workflows::WorkflowAction,
) -> Result<(), String> {
    use crate::workflows::WorkflowAction;
    match action {
        WorkflowAction::Stop => {
            handle
                .request_typed::<WorkflowStopResponse>(ClientRequest::WorkflowStop {
                    request_id: request_id(),
                    params: WorkflowStopParams {
                        thread_id: thread_id.to_string(),
                        workflow_id: id,
                    },
                })
                .await
                .map_err(|error| error.to_string())?;
        }
        WorkflowAction::Retry => {
            handle
                .request_typed::<WorkflowRetryResponse>(ClientRequest::WorkflowRetry {
                    request_id: request_id(),
                    params: WorkflowRetryParams {
                        thread_id: thread_id.to_string(),
                        workflow_id: id,
                    },
                })
                .await
                .map_err(|error| error.to_string())?;
        }
        WorkflowAction::Recover => {
            handle
                .request_typed::<WorkflowRecoverResponse>(ClientRequest::WorkflowRecover {
                    request_id: request_id(),
                    params: WorkflowRecoverParams {
                        thread_id: thread_id.to_string(),
                        workflow_id: id,
                    },
                })
                .await
                .map_err(|error| error.to_string())?;
        }
        WorkflowAction::Save => {
            handle
                .request_typed::<WorkflowSaveResponse>(ClientRequest::WorkflowSave {
                    request_id: request_id(),
                    params: WorkflowSaveParams {
                        thread_id: thread_id.to_string(),
                        name: id.clone(),
                        workflow_id: id,
                        scope: WorkflowScope::Project,
                    },
                })
                .await
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

pub(crate) async fn start_saved(
    handle: &AppServerRequestHandle,
    thread_id: ThreadId,
    saved_workflow_id: String,
    input: serde_json::Value,
) -> Result<(), String> {
    handle
        .request_typed::<WorkflowStartResponse>(ClientRequest::WorkflowStart {
            request_id: request_id(),
            params: WorkflowStartParams {
                thread_id: thread_id.to_string(),
                completion_mode: None,
                source: None,
                saved_workflow_id: Some(saved_workflow_id),
                input,
            },
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) async fn transcript(
    handle: &AppServerRequestHandle,
    target: &crate::workflows::TranscriptTarget,
    cursor: Option<String>,
) -> Result<codex_app_server_protocol::WorkflowTranscriptListResponse, String> {
    handle
        .request_typed(ClientRequest::WorkflowTranscriptList {
            request_id: request_id(),
            params: codex_app_server_protocol::WorkflowTranscriptListParams {
                thread_id: target.owner.to_string(),
                workflow_id: target.run_id.clone(),
                execution_id: (!target.execution_id.is_empty())
                    .then(|| target.execution_id.clone()),
                target: target.target.clone(),
                cursor,
                limit: Some(100),
            },
        })
        .await
        .map_err(|error| error.to_string())
}

pub(crate) async fn save(
    handle: &AppServerRequestHandle,
    thread_id: ThreadId,
    id: String,
    name: String,
    scope: WorkflowScope,
) -> Result<(), String> {
    handle
        .request_typed::<WorkflowSaveResponse>(ClientRequest::WorkflowSave {
            request_id: request_id(),
            params: WorkflowSaveParams {
                thread_id: thread_id.to_string(),
                name,
                workflow_id: id,
                scope,
            },
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) async fn execution_control(
    handle: &AppServerRequestHandle,
    thread_id: ThreadId,
) -> Result<ThreadExecutionControl, String> {
    let response: ThreadExecutionControlReadResponse = handle
        .request_typed(ClientRequest::ThreadExecutionControlRead {
            request_id: request_id(),
            params: ThreadExecutionControlReadParams {
                thread_id: thread_id.to_string(),
            },
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(response.execution_control)
}

pub(crate) async fn set_execution(
    handle: &AppServerRequestHandle,
    thread_id: ThreadId,
    pause: bool,
) -> Result<(), String> {
    if pause {
        handle
            .request_typed::<ThreadPauseResponse>(ClientRequest::ThreadPause {
                request_id: request_id(),
                params: ThreadPauseParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .map_err(|error| error.to_string())?;
    } else {
        handle
            .request_typed::<ThreadUnpauseResponse>(ClientRequest::ThreadUnpause {
                request_id: request_id(),
                params: ThreadUnpauseParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}
