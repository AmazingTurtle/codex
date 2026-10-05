//! Projects internal agent messages onto the standard Responses wire format.
//! Only the request copy changes; persisted history keeps its original items.

use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

pub(crate) fn standard_agent_messages(input: &mut [ResponseItem]) -> Result<()> {
    for item in input {
        let ResponseItem::AgentMessage { content, .. } = item else {
            continue;
        };
        if content
            .iter()
            .any(|part| matches!(part, AgentMessageInputContent::EncryptedContent { .. }))
        {
            return Err(CodexErr::InvalidRequest("This model provider cannot read encrypted agent tasks. Retry delegation after the plaintext collaboration tools have refreshed.".to_string()));
        }
        let content = std::mem::take(content)
            .into_iter()
            .map(|part| match part {
                AgentMessageInputContent::InputText { text } => ContentItem::InputText { text },
                AgentMessageInputContent::EncryptedContent { .. } => {
                    unreachable!("encrypted tasks were rejected above")
                }
            })
            .collect();
        *item = ResponseItem::Message {
            encrypted_content: None,
            status: None,
            id: None,
            role: "user".to_string(),
            content,
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        };
    }
    Ok(())
}

#[cfg(test)]
#[path = "client_compat_tests.rs"]
mod tests;
