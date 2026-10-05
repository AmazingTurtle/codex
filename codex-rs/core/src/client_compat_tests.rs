use super::*;
use pretty_assertions::assert_eq;

#[test]
fn request_projection_preserves_message_text_and_retained_history() {
    let original = ResponseItem::AgentMessage {
        id: None,
        author: "/root".to_string(),
        recipient: "/root/worker".to_string(),
        content: vec![
            AgentMessageInputContent::InputText {
                text: "Message Type: NEW_TASK\nSender: /root\nPayload:\n".to_string(),
            },
            AgentMessageInputContent::InputText {
                text: "Hello, world!".to_string(),
            },
        ],
        internal_chat_message_metadata_passthrough: None,
    };
    let retained = vec![original.clone()];
    let mut outgoing = retained.clone();
    standard_agent_messages(&mut outgoing).expect("project readable task");
    assert_eq!(
        outgoing,
        vec![ResponseItem::Message {
            encrypted_content: None,
            status: None,
            id: None,
            role: "user".to_string(),
            content: vec![
                ContentItem::InputText {
                    text: "Message Type: NEW_TASK\nSender: /root\nPayload:\n".to_string()
                },
                ContentItem::InputText {
                    text: "Hello, world!".to_string()
                }
            ],
            phase: None,
            internal_chat_message_metadata_passthrough: None
        }]
    );
    assert_eq!(retained, vec![original]);
}

#[test]
fn encrypted_task_is_rejected_without_discarding_its_payload() {
    let original = ResponseItem::AgentMessage {
        id: None,
        author: "/root".to_string(),
        recipient: "/root/worker".to_string(),
        content: vec![AgentMessageInputContent::EncryptedContent {
            encrypted_content: "opaque task".to_string(),
        }],
        internal_chat_message_metadata_passthrough: None,
    };
    let mut outgoing = vec![original.clone()];
    let error = standard_agent_messages(&mut outgoing).expect_err("reject unreadable task");
    assert!(
        error
            .to_string()
            .contains("cannot read encrypted agent tasks")
    );
    assert_eq!(outgoing, vec![original]);
}
