//! Exact native identity allows late workflow proof to change presentation, never source.
use super::*;
use codex_app_server_protocol::WorkflowAgentMessageFormat;
use codex_app_server_protocol::WorkflowAgentMessageProvenance;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct NativeMessageIdentity {
    pub(crate) thread_id: String,
    pub(crate) turn_id: String,
    pub(crate) item_id: String,
}

impl NativeMessageIdentity {
    pub(crate) fn matches(&self, proof: &WorkflowAgentMessageProvenance) -> bool {
        self.thread_id == proof.child_thread_id
            && self.turn_id == proof.turn_id
            && self.item_id == proof.item_id
    }
}

#[derive(Debug)]
pub(crate) struct WorkflowMessageCell {
    identity: NativeMessageIdentity,
    native: AgentMarkdownCell,
    value: Mutex<Option<(serde_json::Value, AgentMarkdownCell)>>,
    revision: AtomicU64,
}

impl WorkflowMessageCell {
    pub(crate) fn is_structured(&self) -> bool {
        self.value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }
    pub(crate) fn new(identity: NativeMessageIdentity, native: AgentMarkdownCell) -> Self {
        Self {
            identity,
            native,
            value: Mutex::new(None),
            revision: AtomicU64::new(0),
        }
    }

    pub(crate) fn apply(&self, proof: &WorkflowAgentMessageProvenance) -> bool {
        if !self.identity.matches(proof)
            || proof.run_id.is_empty()
            || proof.execution_id.is_empty()
            || proof.attempt_id.is_empty()
            || proof.format != WorkflowAgentMessageFormat::Json
        {
            return false;
        }
        let mut previous = self
            .value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if previous.is_some() {
            return false;
        }
        let Some(source) = self.native.copy_source() else {
            return false;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(source) else {
            return false;
        };
        if value.is_string() {
            return false;
        }
        // Give the native assistant marker its own paragraph; the generated document can itself
        // begin with a list, whose marker must remain separate from the assistant marker.
        let markdown = crate::structured_value::value_markdown(&value);
        let rendered = self
            .native
            .with_display_markdown(format!("Response\n\n{markdown}"));
        *previous = Some((value, rendered));
        self.revision.fetch_add(1, Ordering::Relaxed);
        true
    }
}

impl HistoryCell for WorkflowMessageCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        match self
            .value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            Some((_, native)) => native.display_lines(width),
            None => self.native.display_lines(width),
        }
    }
    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.native.raw_lines()
    }
    fn display_hyperlink_lines(
        &self,
        width: u16,
    ) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
        match self
            .value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            Some((_, native)) => native.display_hyperlink_lines(width),
            None => self.native.display_hyperlink_lines(width),
        }
    }
    fn retained_hyperlink_lines(
        &self,
        width: u16,
        detailed: bool,
    ) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
        match self
            .value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            Some((_, native)) => native.retained_hyperlink_lines(width, detailed),
            None => self.native.retained_hyperlink_lines(width, detailed),
        }
    }
    fn copy_source(&self) -> Option<&str> {
        self.native.copy_source()
    }
    fn transcript_hyperlink_lines(
        &self,
        width: u16,
    ) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
        self.retained_hyperlink_lines(width, /*detailed*/ true)
    }
    fn has_stable_transcript_height(&self) -> bool {
        self.native.has_stable_transcript_height()
    }
    fn transcript_animation_tick(&self) -> Option<u64> {
        Some(self.revision.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
#[path = "workflow_message_tests.rs"]
mod tests;
