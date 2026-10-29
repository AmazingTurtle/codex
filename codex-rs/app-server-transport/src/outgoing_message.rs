use std::fmt;

use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerNotificationEnvelope;
use codex_app_server_protocol::ServerRequest;
use serde::Serialize;
use tokio::sync::oneshot;

#[path = "history_presentation.rs"]
mod history_presentation;

/// Stable identifier for a transport connection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConnectionId(pub u64);

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Outgoing message from the server to the client.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum OutgoingMessage {
    Request(ServerRequest),
    /// AppServerNotification is specific to the case where this is run as an
    /// "app server" as opposed to an MCP server.
    AppServerNotification(ServerNotificationEnvelope),
    Response(OutgoingResponse),
    Error(OutgoingError),
    /// A connection-specific view of native history without experimental workflow
    /// presentation fields. The original typed payload remains available internally.
    StableHistory(
        #[serde(serialize_with = "history_presentation::serialize")] Box<OutgoingMessage>,
    ),
}

impl OutgoingMessage {
    pub fn without_workflow_presentation(mut self) -> Self {
        if matches!(self, Self::StableHistory(_)) {
            return self;
        }
        if history_presentation::clear(&mut self) {
            Self::StableHistory(Box::new(self))
        } else {
            self
        }
    }

    /// Unwraps a serialization view for consumers that handle typed notifications.
    pub fn into_original(self) -> Self {
        match self {
            Self::StableHistory(message) => message.into_original(),
            message => message,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OutgoingResponse {
    pub id: RequestId,
    pub result: Box<ClientResponsePayload>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutgoingError {
    pub error: JSONRPCErrorError,
    pub id: RequestId,
}

#[derive(Debug)]
pub struct QueuedOutgoingMessage {
    pub message: OutgoingMessage,
    pub write_complete_tx: Option<oneshot::Sender<()>>,
}

impl QueuedOutgoingMessage {
    pub fn new(message: OutgoingMessage) -> Self {
        Self {
            message,
            write_complete_tx: None,
        }
    }
}
