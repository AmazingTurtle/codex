use super::*;
use crate::tools::context::ToolCallSource;

#[test]
fn profile_transport_keeps_direct_tool_messages_readable() {
    let source = ToolCallSource::Direct;
    assert!(
        matches!(agent_message_from_tool("task".to_string(), &source, AgentMessageTransport::Plaintext), AgentMessage::Plaintext(text) if text == "task")
    );
    assert!(
        matches!(agent_message_from_tool("opaque task".to_string(), &source, AgentMessageTransport::CodexEncrypted), AgentMessage::Encrypted(text) if text == "opaque task")
    );
}
