//! Workflow proof changes only the exact native message's visual presentation.
use super::*;
use crate::history_cell::NativeMessageIdentity;
use crate::history_cell::WorkflowMessageCell;
use codex_app_server_protocol::WorkflowAgentMessageProvenance;

impl ChatWidget {
    pub(super) fn remember_workflow_final(&mut self, proof: WorkflowAgentMessageProvenance) {
        if self.thread_id.map(|id| id.to_string()).as_deref()
            != Some(proof.child_thread_id.as_str())
        {
            return;
        }
        let identity = NativeMessageIdentity {
            thread_id: proof.child_thread_id.clone(),
            turn_id: proof.turn_id.clone(),
            item_id: proof.item_id.clone(),
        };
        if self.transcript.workflow_finals.len() >= 128
            && !self.transcript.workflow_finals.contains_key(&identity)
        {
            self.transcript.workflow_finals.clear();
        }
        self.transcript
            .workflow_finals
            .insert(identity, proof.clone());
        self.app_event_tx.send(AppEvent::ApplyWorkflowFinal(proof));
    }

    pub(crate) fn identify_agent_message(
        &self,
        native: history_cell::AgentMarkdownCell,
        identity: Option<NativeMessageIdentity>,
    ) -> Box<dyn HistoryCell> {
        let Some(identity) = identity else {
            return Box::new(native);
        };
        let cell = WorkflowMessageCell::new(identity.clone(), native);
        if let Some(proof) = self.transcript.workflow_finals.get(&identity) {
            cell.apply(proof);
        }
        Box::new(cell)
    }
}

#[cfg(test)]
#[path = "workflow_final_tests.rs"]
mod tests;
