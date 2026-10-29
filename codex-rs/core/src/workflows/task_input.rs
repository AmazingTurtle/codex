//! Versioned request projection preserves the durable native task and its receipt.
//!
//! Only explicitly marked workflow plaintext is partitioned. The original item,
//! task options, admission, conversation history, and user-facing prompt remain intact.

use crate::agent::types::MessageDeliveryMode;
use crate::context::ContextualUserFragment;
use crate::context::InterAgentMessage;
use crate::context::InterAgentMessageType;
use crate::context::WorkflowTaskPart;
use codex_protocol::AgentPath;
use codex_protocol::ResponseItemId;
use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ContentItemMetadata;
use codex_protocol::models::ContentItemNamespace;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::InterAgentCommunication;
use uuid::Uuid;

/// Bound the complete serialized item, including attribution and partition wrapper.
pub(crate) const MAX_TASK_ITEM_BYTES: usize = 8192;
/// Fixed payload sizing keeps v1 boundaries independent of transient provider metadata.
const MAX_ESCAPED_PAYLOAD_BYTES: usize = 6144;
const MAX_TASK_PARTS: usize = 8;

pub(crate) fn communication(
    payload: String,
    author: AgentPath,
    recipient: AgentPath,
    mode: MessageDeliveryMode,
) -> InterAgentCommunication {
    let message_type = match mode {
        MessageDeliveryMode::QueueOnly => InterAgentMessageType::Message,
        MessageDeliveryMode::TriggerTurn => InterAgentMessageType::NewTask,
    };
    let content = InterAgentMessage::new(message_type, recipient.clone(), author.clone(), payload)
        .render()
        .into_text()
        .unwrap_or_default();
    let mut communication = InterAgentCommunication::new(
        author,
        recipient,
        Vec::new(),
        content,
        mode == MessageDeliveryMode::TriggerTurn,
    );
    communication
        .internal_chat_message_metadata_passthrough
        .get_or_insert_default()
        .content_item_kinds = Some(vec![ContentItemKind(
        WorkflowTaskPart::SOURCE_KIND.to_string(),
    )]);
    communication
}

/// Validate after native child configuration is resolved, before child admission.
pub(crate) fn validate(
    payload: &str,
    author: &AgentPath,
    recipient: &AgentPath,
    namespace: Option<&str>,
) -> Result<(), String> {
    let mut source = communication(
        payload.to_string(),
        author.clone(),
        recipient.clone(),
        MessageDeliveryMode::TriggerTurn,
    )
    .to_model_input_item();
    source.set_id(Some(ResponseItemId::with_suffix("amsg", Uuid::nil())));
    if let ResponseItem::AgentMessage {
        internal_chat_message_metadata_passthrough,
        ..
    } = &mut source
    {
        let metadata = internal_chat_message_metadata_passthrough.get_or_insert_default();
        metadata.turn_id = Some(Uuid::nil().to_string());
        metadata.content_item_metadata = Some(vec![ContentItemMetadata::tool(Some(
            namespace
                .map(|name| ContentItemNamespace::from(name.to_string()))
                .unwrap_or(ContentItemNamespace::Functions),
        ))]);
    }
    project(vec![source]).map(drop)
}

/// Runs on the request copy before transport cache/delta selection, never on stored history.
pub(crate) fn project(input: Vec<ResponseItem>) -> Result<Vec<ResponseItem>, String> {
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let marked = matches!(&item, ResponseItem::AgentMessage { internal_chat_message_metadata_passthrough: Some(metadata), .. }
            if metadata.content_item_kinds.as_ref().is_some_and(|kinds| kinds.as_slice() == [ContentItemKind(WorkflowTaskPart::SOURCE_KIND.to_string())]));
        if !marked
            || serde_json::to_vec(&item)
                .map_err(|error| error.to_string())?
                .len()
                <= MAX_TASK_ITEM_BYTES
        {
            output.push(item);
            continue;
        }
        output.extend(partition(&item)?);
    }
    Ok(output)
}

fn partition(source: &ResponseItem) -> Result<Vec<ResponseItem>, String> {
    let ResponseItem::AgentMessage {
        id: Some(source_id),
        content,
        author,
        recipient,
        ..
    } = source
    else {
        return Err("workflow task projection requires its durable native message identity".into());
    };
    let [AgentMessageInputContent::InputText { text }] = content.as_slice() else {
        return Err("workflow task projection requires one plaintext native task".into());
    };
    if text.is_empty() {
        return Err("workflow task projection requires nonempty native task content".into());
    }
    let mut remaining = text.as_str();
    let mut chunks = Vec::new();
    while !remaining.is_empty() {
        if chunks.len() == MAX_TASK_PARTS {
            return Err("workflow task exceeds eight bounded input parts".into());
        }
        let mut boundaries = vec![0];
        boundaries.extend(
            remaining
                .char_indices()
                .skip(1)
                .map(|(index, _)| index)
                .take_while(|index| *index <= MAX_ESCAPED_PAYLOAD_BYTES),
        );
        if remaining.len() <= MAX_ESCAPED_PAYLOAD_BYTES {
            boundaries.push(remaining.len());
        }
        let mut low = 0;
        let mut high = boundaries.len();
        while low + 1 < high {
            let middle = low + (high - low) / 2;
            let encoded = serde_json::to_vec(&remaining[..boundaries[middle]])
                .map_err(|error| error.to_string())?;
            if encoded.len().saturating_sub(2) <= MAX_ESCAPED_PAYLOAD_BYTES {
                low = middle;
            } else {
                high = middle;
            }
        }
        let end = boundaries[low];
        if end == 0 {
            return Err(
                "workflow task contains a character that cannot fit a bounded input part".into(),
            );
        }
        chunks.push(&remaining[..end]);
        remaining = &remaining[end..];
    }
    let total = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, payload)| {
            let part = WorkflowTaskPart {
                payload,
                index: index + 1,
                total,
                author,
                recipient,
            }
            .render()
            .into_text()
            .unwrap_or_default();
            let identity = Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!(
                    "{}:{source_id}:{}",
                    WorkflowTaskPart::SOURCE_KIND,
                    index + 1
                )
                .as_bytes(),
            );
            let mut item = source.clone();
            item.set_id(Some(ResponseItemId::with_suffix("amsg", identity)));
            if let ResponseItem::AgentMessage {
                content,
                internal_chat_message_metadata_passthrough,
                ..
            } = &mut item
            {
                *content = vec![AgentMessageInputContent::InputText { text: part }];
                internal_chat_message_metadata_passthrough
                    .get_or_insert_default()
                    .content_item_kinds = Some(vec![ContentItemKind(
                    WorkflowTaskPart::PART_KIND.to_string(),
                )]);
            }
            if serde_json::to_vec(&item)
                .map_err(|error| error.to_string())?
                .len()
                > MAX_TASK_ITEM_BYTES
            {
                return Err(
                    "workflow task attribution exceeds the complete input-item byte limit".into(),
                );
            }
            Ok(item)
        })
        .collect()
}

#[cfg(test)]
#[path = "task_input_tests.rs"]
mod tests;
