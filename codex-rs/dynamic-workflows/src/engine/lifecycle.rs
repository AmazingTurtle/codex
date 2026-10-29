//! Delivery receipts are independent of immutable lifecycle payloads.

use super::WorkflowEngine;
use crate::WorkflowExecutionSnapshot;
use crate::WorkflowHost;
use crate::WorkflowLifecycleEvent;

impl<H: WorkflowHost> WorkflowEngine<H> {
    pub fn read_execution_snapshot(
        &self,
        run_id: &str,
        execution_id: &str,
    ) -> Result<WorkflowExecutionSnapshot, String> {
        self.store.read_execution_snapshot(run_id, execution_id)
    }
    pub fn subscribe_lifecycle(&self) -> tokio::sync::broadcast::Receiver<WorkflowLifecycleEvent> {
        self.store.lifecycle_events.subscribe()
    }

    pub fn pending_lifecycle_events(
        &self,
        run_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkflowLifecycleEvent>, String> {
        let run = self.get(run_id)?;
        let display = self.store.pending_display_events(&run)?;
        Ok(run
            .lifecycle
            .events
            .into_iter()
            .filter(|event| !run.lifecycle.delivered.contains(&event.id))
            .chain(display)
            .skip(offset)
            .take(limit.min(crate::MAX_PAGE_SIZE))
            .collect())
    }

    /// Call only after the exact event ID is durably present in the owner's rollout or mailbox.
    pub fn acknowledge_lifecycle_event(&self, run_id: &str, event_id: &str) -> Result<(), String> {
        if self.store.acknowledge_display_event(run_id, event_id)? {
            return Ok(());
        }
        self.store.acknowledge_lifecycle_event(run_id, event_id)
    }

    pub fn get_execution_snapshot(
        &self,
        run_id: &str,
        execution_id: &str,
    ) -> Result<WorkflowExecutionSnapshot, String> {
        self.get(run_id)?
            .lifecycle
            .snapshots
            .get(execution_id)
            .cloned()
            .ok_or_else(|| "workflow execution snapshot not found".into())
    }

    /// Explicitly display one historical execution; this never enables automatic backfill.
    pub fn show_result_once(
        &self,
        run_id: &str,
        execution_id: &str,
    ) -> Result<WorkflowLifecycleEvent, String> {
        self.store.show_result_once(run_id, execution_id)
    }
}
