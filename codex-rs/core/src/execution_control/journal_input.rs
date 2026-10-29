//! Journal encoding retains harness annotations that the external turn-input
//! serialization boundary deliberately excludes.

use crate::session::TurnInput;
use crate::session::UserInputMetadata;
use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_protocol::models::ResponseItem;
use codex_protocol::user_input::UserInput;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize)]
enum JournalInput {
    User {
        content: Vec<UserInput>,
        client_id: Option<String>,
        metadata: UserInputMetadata,
    },
    FunctionOutput {
        item: ResponseItem,
        metadata: Option<CodexHarnessMetadata>,
    },
    Response {
        item: ResponseItem,
        metadata: Option<CodexHarnessMetadata>,
    },
    Mail(crate::PendingAgentMessage),
}

pub(super) fn serialize<S: serde::Serializer>(
    input: &[TurnInput],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    input
        .iter()
        .map(|input| match input {
            TurnInput::UserInput {
                content,
                client_id,
                metadata,
            } => JournalInput::User {
                content: content.clone(),
                client_id: client_id.clone(),
                metadata: *metadata,
            },
            TurnInput::FunctionCallOutput(item) => JournalInput::FunctionOutput {
                item: item.item.clone(),
                metadata: item.metadata.clone(),
            },
            TurnInput::ResponseItem(item) => JournalInput::Response {
                item: item.item.clone(),
                metadata: item.metadata.clone(),
            },
            TurnInput::InterAgentCommunication(mail) => JournalInput::Mail(mail.clone()),
        })
        .collect::<Vec<_>>()
        .serialize(serializer)
}

pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<TurnInput>, D::Error> {
    Vec::<JournalInput>::deserialize(deserializer).map(|input| {
        input
            .into_iter()
            .map(|input| match input {
                JournalInput::User {
                    content,
                    client_id,
                    metadata,
                } => TurnInput::UserInput {
                    content,
                    client_id,
                    metadata,
                },
                JournalInput::FunctionOutput { item, metadata } => {
                    TurnInput::FunctionCallOutput(ResponseItemEnvelope { item, metadata })
                }
                JournalInput::Response { item, metadata } => {
                    TurnInput::ResponseItem(ResponseItemEnvelope { item, metadata })
                }
                JournalInput::Mail(mail) => TurnInput::InterAgentCommunication(mail),
            })
            .collect()
    })
}
