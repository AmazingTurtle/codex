//! Atomic journal storage and the accepted signals owned by a conversation.

use super::journal_input;
use crate::session::TurnInput;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct QueuedSignal {
    pub(super) id: String,
    pub(crate) thread_id: ThreadId,
    #[serde(with = "journal_input")]
    pub(crate) input: Vec<TurnInput>,
    #[serde(default)]
    pub(crate) start_options: codex_protocol::turn_input::TurnStartOptions,
    #[serde(default)]
    pub(super) wake_generation: Option<u64>,
    #[serde(skip)]
    pub(super) ephemeral: bool,
    #[serde(skip)]
    pub(super) awaiting_finalization: bool,
}

impl QueuedSignal {
    pub(super) fn same_delivery(&self, other: &Self) -> bool {
        if self.thread_id != other.thread_id {
            return false;
        }
        match (self.input.as_slice(), other.input.as_slice()) {
            ([left], [right]) => match (delivery_id(left), delivery_id(right)) {
                (Some(left), Some(right)) => left == right,
                _ => self.input == other.input,
            },
            _ => self.input == other.input,
        }
    }
    pub(crate) fn new(thread_id: ThreadId, mut input: Vec<TurnInput>) -> Self {
        for item in &mut input {
            if let TurnInput::FunctionCallOutput(item) | TurnInput::ResponseItem(item) = item {
                crate::session::session::Session::assign_missing_response_item_id(&mut item.item);
            }
            if let TurnInput::InterAgentCommunication(mail) = item
                && mail.id.is_none()
            {
                let mut communication = (**mail).clone();
                communication.id = Some(codex_protocol::ResponseItemId::new("mail"));
                *mail = crate::PendingAgentMessage::new(communication, mail.delivery.clone());
            }
        }
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            thread_id,
            input,
            start_options: Default::default(),
            wake_generation: None,
            ephemeral: false,
            awaiting_finalization: false,
        }
    }

    pub(crate) fn on_start(
        mut self,
        start_options: codex_protocol::turn_input::TurnStartOptions,
    ) -> Self {
        self.start_options = start_options;
        self
    }

    pub(crate) fn with_wake_generation(mut self, generation: u64) -> Self {
        self.wake_generation = Some(generation);
        self
    }

    pub(crate) fn awaiting_finalization(mut self) -> Self {
        self.awaiting_finalization = true;
        self
    }
}

pub(super) fn delivery_id(input: &TurnInput) -> Option<&codex_protocol::ResponseItemId> {
    match input {
        TurnInput::InterAgentCommunication(mail) => mail.id.as_ref(),
        TurnInput::FunctionCallOutput(item) | TurnInput::ResponseItem(item) => item.item.id(),
        TurnInput::UserInput { .. } => None,
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Journal {
    pub(super) root_thread_id: ThreadId,
    pub(super) paused: bool,
    #[serde(serialize_with = "serialize_durable_signals")]
    pub(super) signals: Vec<QueuedSignal>,
}

fn serialize_durable_signals<S: serde::Serializer>(
    signals: &[QueuedSignal],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    signals
        .iter()
        .filter(|signal| !signal.ephemeral && !signal.awaiting_finalization)
        .collect::<Vec<_>>()
        .serialize(serializer)
}

pub(super) struct Storage {
    pub(super) path: Option<PathBuf>,
    pub(super) durable_path: Option<PathBuf>,
    pub(super) journal: Journal,
    pub(super) claimed: HashSet<String>,
    pub(super) ephemeral_threads: HashSet<ThreadId>,
}

pub(super) fn receipt_key(input: &TurnInput) -> Option<String> {
    match input {
        TurnInput::FunctionCallOutput(item) | TurnInput::ResponseItem(item) => {
            item.item.id().map(ToString::to_string)
        }
        TurnInput::InterAgentCommunication(mail) => mail.id.as_ref().map(ToString::to_string),
        TurnInput::UserInput { metadata, .. } => metadata
            .acceptance_order
            .map(|order| format!("user:{order}")),
    }
}

pub(super) fn remove_receipts(
    journal: &mut Journal,
    thread_id: ThreadId,
    receipts: &HashSet<String>,
) -> bool {
    let mut changed = false;
    for signal in &mut journal.signals {
        if signal.thread_id != thread_id {
            continue;
        }
        let previous_len = signal.input.len();
        signal
            .input
            .retain(|input| !receipt_key(input).is_some_and(|id| receipts.contains(&id)));
        changed |= signal.input.len() != previous_len;
    }
    journal.signals.retain(|signal| !signal.input.is_empty());
    changed
}

pub(super) async fn persist(path: &Option<PathBuf>, journal: &Journal) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let bytes = serde_json::to_vec(journal)
        .map_err(|error| CodexErr::Fatal(format!("serialize execution control: {error}")))?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| CodexErr::Fatal(format!("create execution control: {error}")))?;
    }
    let temporary = path.with_extension("json.tmp");
    let mut file = tokio::fs::File::create(&temporary)
        .await
        .map_err(|error| CodexErr::Fatal(format!("write execution control: {error}")))?;
    file.write_all(&bytes)
        .await
        .map_err(|error| CodexErr::Fatal(format!("write execution control: {error}")))?;
    file.sync_all()
        .await
        .map_err(|error| CodexErr::Fatal(format!("flush execution control: {error}")))?;
    drop(file);
    tokio::fs::rename(temporary, path)
        .await
        .map_err(|error| CodexErr::Fatal(format!("save execution control: {error}")))?;
    Ok(())
}
