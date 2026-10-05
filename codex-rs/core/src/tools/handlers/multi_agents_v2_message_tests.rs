use super::*;
use crate::tools::context::ToolCallSource;

#[test]
fn profile_messages_remain_readable_when_sent_from_code_mode() {
    let source = ToolCallSource::CodeMode {
        cell_id: "cell".to_string(),
        runtime_tool_call_id: "nested-call".to_string(),
    };
    assert!(
        matches!(agent_message_from_tool("task".to_string(), &source, AgentMessageTransport::Plaintext), AgentMessage::Plaintext(text) if text == "task")
    );
    assert!(
        matches!(agent_message_from_tool("opaque task".to_string(), &source, AgentMessageTransport::CodexEncrypted), AgentMessage::Encrypted(text) if text == "opaque task")
    );
}
