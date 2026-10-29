//! App-server adapter for the same per-conversation workflow service as model tools.

use super::*;
use codex_app_server_protocol as api;
use codex_core::workflows::DynamicWorkflowService;
#[path = "workflow_conversion.rs"]
pub(super) mod conversion;
#[path = "workflow_live.rs"]
mod live;
#[path = "workflow_steering.rs"]
mod steering;
#[path = "workflow_steering_conversion.rs"]
mod steering_conversion;
#[path = "workflow_transcript.rs"]
mod transcript;

pub(crate) struct WorkflowRequestProcessor {
    thread_manager: Arc<ThreadManager>,
    thread_store: Arc<dyn ThreadStore>,
    config: Arc<Config>,
    listener_task_context: super::thread_lifecycle::ListenerTaskContext,
    final_items: std::sync::Mutex<std::collections::HashMap<(String, String, String), String>>,
}

impl WorkflowRequestProcessor {
    pub(crate) async fn result_display(
        &self,
        params: api::WorkflowResultDisplayParams,
    ) -> Result<api::WorkflowResultDisplayResponse, JSONRPCErrorError> {
        let id = ThreadId::from_string(&params.thread_id)
            .map_err(|error| invalid_request(format!("invalid thread id: {error}")))?;
        let caller = self
            .thread_manager
            .get_thread(id)
            .await
            .map_err(|_| invalid_request(format!("thread not found: {id}")))?;
        let event = caller
            .display_workflow_result(&params.workflow_id, params.execution_id.as_deref())
            .await
            .map_err(invalid_request)?;
        Ok(api::WorkflowResultDisplayResponse {
            event: event.into(),
        })
    }
    pub(crate) fn new(
        thread_manager: Arc<ThreadManager>,
        thread_store: Arc<dyn ThreadStore>,
        config: Arc<Config>,
        thread_processor: &super::ThreadRequestProcessor,
    ) -> Self {
        Self {
            thread_manager,
            thread_store,
            config,
            listener_task_context: thread_processor.listener_task_context(),
            final_items: Default::default(),
        }
    }

    pub(crate) async fn start(
        &self,
        params: api::WorkflowStartParams,
    ) -> Result<api::WorkflowStartResponse, JSONRPCErrorError> {
        let caller_id = ThreadId::from_string(&params.thread_id)
            .map_err(|error| invalid_request(format!("invalid thread id: {error}")))?;
        let caller = self
            .thread_manager
            .get_thread(caller_id)
            .await
            .map_err(|_| invalid_request(format!("thread not found: {caller_id}")))?;
        super::thread_input::ensure_user_input_allowed(&caller).await?;
        let completion_mode = match params.completion_mode.unwrap_or_default() {
            api::WorkflowCompletionMode::Display => {
                codex_core::workflows::WorkflowCompletionMode::Display
            }
            api::WorkflowCompletionMode::Continue => {
                codex_core::workflows::WorkflowCompletionMode::Continue
            }
        };
        let run = match (params.source, params.saved_workflow_id) {
            (Some(source), None) => {
                caller
                    .start_workflow_with_completion_mode(source, params.input, completion_mode)
                    .await
            }
            (None, Some(id)) => {
                caller
                    .start_saved_workflow_with_completion_mode(id, params.input, completion_mode)
                    .await
            }
            (Some(_), Some(_)) | (None, None) => {
                return Err(invalid_request(
                    "supply exactly one of source and savedWorkflowId",
                ));
            }
        }
        .map_err(invalid_request)?;
        Ok(api::WorkflowStartResponse {
            workflow: conversion::workflow(run),
        })
    }

    pub(crate) async fn list(
        &self,
        params: api::WorkflowListParams,
    ) -> Result<api::WorkflowListResponse, JSONRPCErrorError> {
        let service = self.service(&params.thread_id).await?;
        let (offset, limit) = page(params.cursor, params.limit)?;
        let list = |offset, limit| match params.scope.unwrap_or(api::WorkflowListScope::Current) {
            api::WorkflowListScope::Current => service.list_summaries(offset, limit),
            api::WorkflowListScope::Project => service.list_project_summaries(offset, limit),
            api::WorkflowListScope::All => service.list_all_summaries(offset, limit),
        };
        let runs = list(offset, limit).map_err(invalid_request)?;
        let next_offset = offset.saturating_add(limit);
        let next_cursor = if runs.len() == limit
            && !list(next_offset, /*limit*/ 1)
                .map_err(invalid_request)?
                .is_empty()
        {
            Some(next_offset.to_string())
        } else {
            None
        };
        Ok(api::WorkflowListResponse {
            data: runs.into_iter().map(conversion::summary).collect(),
            next_cursor,
        })
    }

    pub(crate) async fn read(
        &self,
        params: api::WorkflowReadParams,
    ) -> Result<api::WorkflowReadResponse, JSONRPCErrorError> {
        let run = self
            .service(&params.thread_id)
            .await?
            .get(&params.workflow_id)
            .map_err(invalid_request)?;
        Ok(api::WorkflowReadResponse {
            workflow: conversion::workflow(run),
        })
    }

    pub(crate) async fn stop(
        &self,
        params: api::WorkflowStopParams,
    ) -> Result<api::WorkflowStopResponse, JSONRPCErrorError> {
        let run = self
            .service(&params.thread_id)
            .await?
            .stop(&params.workflow_id)
            .await
            .map_err(invalid_request)?;
        Ok(api::WorkflowStopResponse {
            workflow: conversion::workflow(run),
        })
    }

    pub(crate) async fn retry(
        &self,
        params: api::WorkflowRetryParams,
    ) -> Result<api::WorkflowRetryResponse, JSONRPCErrorError> {
        let run = self
            .service(&params.thread_id)
            .await?
            .retry(&params.workflow_id)
            .await
            .map_err(invalid_request)?;
        Ok(api::WorkflowRetryResponse {
            workflow: conversion::workflow(run),
        })
    }

    pub(crate) async fn recover(
        &self,
        params: api::WorkflowRecoverParams,
    ) -> Result<api::WorkflowRecoverResponse, JSONRPCErrorError> {
        let run = self
            .service(&params.thread_id)
            .await?
            .recover(&params.workflow_id)
            .await
            .map_err(invalid_request)?;
        Ok(api::WorkflowRecoverResponse {
            workflow: conversion::workflow(run),
        })
    }

    pub(crate) async fn save(
        &self,
        params: api::WorkflowSaveParams,
    ) -> Result<api::WorkflowSaveResponse, JSONRPCErrorError> {
        let service = self.service(&params.thread_id).await?;
        let run = service.get(&params.workflow_id).map_err(invalid_request)?;
        let saved = service
            .library()
            .save(
                &params.name,
                &run.source,
                conversion::engine_scope(params.scope),
            )
            .map_err(invalid_request)?;
        Ok(api::WorkflowSaveResponse {
            saved_workflow: conversion::saved(saved),
        })
    }

    pub(crate) async fn saved_list(
        &self,
        params: api::WorkflowSavedListParams,
    ) -> Result<api::WorkflowSavedListResponse, JSONRPCErrorError> {
        let service = self.service(&params.thread_id).await?;
        let (offset, limit) = page(params.cursor, params.limit)?;
        let saved = service
            .library()
            .list(offset, limit)
            .map_err(invalid_request)?;
        let next_offset = offset.saturating_add(limit);
        let next_cursor = if saved.len() == limit
            && !service
                .library()
                .list(next_offset, /*limit*/ 1)
                .map_err(invalid_request)?
                .is_empty()
        {
            Some(next_offset.to_string())
        } else {
            None
        };
        Ok(api::WorkflowSavedListResponse {
            data: saved.into_iter().map(conversion::saved_summary).collect(),
            next_cursor,
            watch_roots: service
                .library()
                .watch_roots()
                .map_err(invalid_request)?
                .iter()
                .map(codex_utils_path_uri::LegacyAppPathString::from_abs_path)
                .collect(),
        })
    }

    pub(crate) async fn saved_read(
        &self,
        params: api::WorkflowSavedReadParams,
    ) -> Result<api::WorkflowSavedReadResponse, JSONRPCErrorError> {
        let service = self.service(&params.thread_id).await?;
        let saved = service
            .library()
            .read(&params.saved_workflow_id)
            .map_err(invalid_request)?;
        Ok(api::WorkflowSavedReadResponse {
            saved_workflow: conversion::saved(saved),
        })
    }

    pub(crate) async fn saved_delete(
        &self,
        params: api::WorkflowSavedDeleteParams,
    ) -> Result<api::WorkflowSavedDeleteResponse, JSONRPCErrorError> {
        let service = self.service(&params.thread_id).await?;
        service
            .library()
            .delete(&params.saved_workflow_id)
            .map_err(invalid_request)?;
        Ok(api::WorkflowSavedDeleteResponse {})
    }

    async fn service(&self, id: &str) -> Result<Arc<DynamicWorkflowService>, JSONRPCErrorError> {
        let thread_id = ThreadId::from_string(id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        let thread = self
            .thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|_| invalid_request(format!("thread not found: {thread_id}")))?;
        thread.workflows().await.map_err(invalid_request)
    }
}

fn page(cursor: Option<String>, limit: Option<u32>) -> Result<(usize, usize), JSONRPCErrorError> {
    let offset = cursor
        .map(|cursor| {
            cursor
                .parse::<usize>()
                .map_err(|_| invalid_request("invalid workflow cursor"))
        })
        .transpose()?
        .unwrap_or_default();
    let limit = limit.unwrap_or(20);
    if !(1..=50).contains(&limit) {
        return Err(invalid_request("workflow limit must be between 1 and 50"));
    }
    Ok((offset, limit as usize))
}
