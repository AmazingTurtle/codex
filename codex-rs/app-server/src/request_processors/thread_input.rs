use super::invalid_request;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_core::CodexThread;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;

pub(super) const DIRECT_INPUT_TO_MULTI_AGENT_V2_SUBAGENT_ERROR: &str =
    "direct app-server input is not allowed for multi-agent v2 sub-agents";

/// Mirrors the ownership policy in request validation and thread capability responses.
pub(super) fn can_accept_direct_input(
    multi_agent_version: Option<MultiAgentVersion>,
    session_source: &SessionSource,
) -> bool {
    multi_agent_version != Some(MultiAgentVersion::V2)
        || !matches!(
            session_source,
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn { .. })
        )
}

pub(super) async fn ensure_direct_input_allowed(
    thread: &CodexThread,
) -> Result<(), JSONRPCErrorError> {
    let config_snapshot = thread.config_snapshot().await;
    if !can_accept_direct_input(
        thread.multi_agent_version(),
        &config_snapshot.session_source,
    ) {
        return Err(invalid_request(
            DIRECT_INPUT_TO_MULTI_AGENT_V2_SUBAGENT_ERROR,
        ));
    }

    Ok(())
}

/// Human text may steer recorded workflow workers without granting their
/// callers permission to replace the worker's configuration or tool context.
pub(super) async fn can_accept_user_input(thread: &CodexThread) -> bool {
    let snapshot = thread.config_snapshot().await;
    can_accept_direct_input(thread.multi_agent_version(), &snapshot.session_source)
        || thread.is_workflow_owned_child().await
}

pub(super) async fn ensure_user_input_allowed(
    thread: &CodexThread,
) -> Result<(), JSONRPCErrorError> {
    if can_accept_user_input(thread).await {
        return Ok(());
    }
    Err(invalid_request(
        DIRECT_INPUT_TO_MULTI_AGENT_V2_SUBAGENT_ERROR,
    ))
}

pub(super) fn ensure_workflow_start_preserves_config(
    params: &codex_app_server_protocol::TurnStartParams,
) -> Result<(), JSONRPCErrorError> {
    let permitted = codex_app_server_protocol::TurnStartParams {
        thread_id: params.thread_id.clone(),
        input: params.input.clone(),
        client_user_message_id: params.client_user_message_id.clone(),
        responsesapi_client_metadata: params.responsesapi_client_metadata.clone(),
        responsesapi_client_metadata_patch: params.responsesapi_client_metadata_patch.clone(),
        ..Default::default()
    };
    if params != &permitted {
        return Err(invalid_request(
            "workflow worker input must preserve its configuration, lineage, and tool context",
        ));
    }
    Ok(())
}
