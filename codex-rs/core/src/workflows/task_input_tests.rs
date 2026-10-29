use super::*;
use pretty_assertions::assert_eq;
use std::collections::BTreeSet;

fn source(payload: String) -> ResponseItem {
    let mut item = communication(
        payload,
        AgentPath::root(),
        AgentPath::root()
            .join("workflow_r1_review_s1_a1")
            .expect("native task path"),
        MessageDeliveryMode::TriggerTurn,
    )
    .to_model_input_item();
    item.set_id(Some(ResponseItemId::with_suffix("amsg", Uuid::nil())));
    if let ResponseItem::AgentMessage {
        internal_chat_message_metadata_passthrough: Some(metadata),
        ..
    } = &mut item
    {
        metadata.turn_id = Some("original-native-turn".into());
        metadata.content_item_metadata = Some(vec![ContentItemMetadata::tool(Some(
            ContentItemNamespace::Functions,
        ))]);
    }
    item
}

fn input_text(item: &ResponseItem) -> &str {
    let ResponseItem::AgentMessage { content, .. } = item else {
        panic!("expected a native AgentMessage")
    };
    let [AgentMessageInputContent::InputText { text }] = content.as_slice() else {
        panic!("expected plaintext task content")
    };
    text
}

#[test]
fn oversized_task_roundtrip_is_lossless_ordered_bounded_and_stable() {
    let prefix =
        "Review the original task without omitting evidence. é中🙂\0\n\"quoted\"\n".repeat(32);
    let payload = format!("{prefix}{}", "r".repeat(9914 - prefix.len()));
    let original = source(payload.clone());
    let before = serde_json::to_vec(&original).expect("durable original task");
    let parts = project(vec![original.clone()]).expect("project a valid oversized task");
    assert!(parts.len() > 1 && parts.len() <= MAX_TASK_PARTS);
    let mut reconstructed = String::new();
    let mut ids = BTreeSet::new();
    for (index, item) in parts.iter().enumerate() {
        let body = input_text(item);
        let header = format!(
            "Workflow task part {}/{}.\nTask name: /root/workflow_r1_review_s1_a1\nSender: /root\nRead every part in order as one task.\n\n",
            index + 1,
            parts.len()
        );
        reconstructed.push_str(
            body.strip_prefix(&header)
                .expect("ordered typed task wrapper"),
        );
        assert!(serde_json::to_vec(item).expect("rendered wire item").len() <= MAX_TASK_ITEM_BYTES);
        let ResponseItem::AgentMessage {
            id: Some(id),
            author,
            recipient,
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } = item
        else {
            panic!("part must preserve native provenance")
        };
        assert!(id.starts_with("amsg_") && id.len() <= 64);
        assert_eq!(
            Uuid::parse_str(id.strip_prefix("amsg_").unwrap())
                .unwrap()
                .get_version(),
            Some(uuid::Version::Sha1)
        );
        ids.insert(id.clone());
        assert_eq!(
            (
                author.as_str(),
                recipient.as_str(),
                metadata.turn_id.as_deref()
            ),
            (
                "/root",
                "/root/workflow_r1_review_s1_a1",
                Some("original-native-turn")
            )
        );
        assert_eq!(
            metadata.content_item_kinds,
            Some(vec![ContentItemKind(
                WorkflowTaskPart::PART_KIND.to_string()
            )])
        );
    }
    assert_eq!(reconstructed, input_text(&original));
    assert!(input_text(&original).ends_with(&payload));
    assert_eq!(ids.len(), parts.len());
    let restored: ResponseItem =
        serde_json::from_slice(&before).expect("resume the durable original");
    assert_eq!(
        project(vec![restored]).expect("stable resume projection"),
        parts
    );
    assert_eq!(
        project(parts.clone()).expect("already projected items remain unchanged"),
        parts
    );
    assert_eq!(
        serde_json::to_vec(&original).unwrap(),
        before,
        "request projection must not rewrite history"
    );
}

#[test]
fn ordinary_encrypted_and_short_native_messages_are_unchanged() {
    let author = AgentPath::root();
    let recipient = author.join("ordinary").expect("ordinary task path");
    let ordinary = InterAgentCommunication::new(
        author.clone(),
        recipient.clone(),
        Vec::new(),
        "o".repeat(20_000),
        /*trigger_turn*/ true,
    )
    .to_model_input_item();
    let encrypted = InterAgentCommunication::new_encrypted(
        author,
        recipient,
        Vec::new(),
        "ciphertext".repeat(3000),
        /*trigger_turn*/ true,
    )
    .to_model_input_item();
    let short = source("A short native workflow task.".into());
    let input = vec![ordinary, encrypted, short];
    assert_eq!(
        project(input.clone()).expect("only oversized marked plaintext is projected"),
        input
    );
}

#[test]
fn excessive_attribution_fails_without_dropping_any_task_text() {
    let mut item = source("task".repeat(3000));
    if let ResponseItem::AgentMessage { author, .. } = &mut item {
        *author = "a".repeat(MAX_TASK_ITEM_BYTES);
    }
    let original = item.clone();
    assert_eq!(
        project(vec![item]).expect_err("attribution cannot fit a bounded item"),
        "workflow task attribution exceeds the complete input-item byte limit"
    );
    assert_eq!(
        input_text(&original),
        input_text(&source("task".repeat(3000)))
    );
}
