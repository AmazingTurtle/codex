//! Explicit historical result display never rewrites or enables a historical run.

use super::Store;
use super::files;
use crate::WorkflowCompletionMode;
use crate::WorkflowLifecycleEvent;
use crate::WorkflowLifecycleKind;
use crate::WorkflowResult;
use crate::WorkflowRun;
use crate::WorkflowStatus;
use serde::Deserialize;
use serde::Serialize;

#[derive(Deserialize, Serialize)]
struct DisplayReceipt {
    event: WorkflowLifecycleEvent,
    delivered: bool,
}

fn event_id(run: &str, execution: &str) -> String {
    let identity = format!("{run}:{execution}:explicit-result");
    format!("wfe_{}", &blake3::hash(identity.as_bytes()).to_hex()[..48])
}

impl Store {
    pub(crate) fn show_result_once(
        &self,
        run_id: &str,
        execution_id: &str,
    ) -> Result<WorkflowLifecycleEvent, String> {
        let run = self.get(run_id)?;
        let _state = self.state.lock().map_err(|error| error.to_string())?;
        let id = event_id(run_id, execution_id);
        let path = self.root.join("display").join(format!("{id}.json"));
        if path.exists() {
            return files::read_json::<DisplayReceipt>(&path).map(|receipt| receipt.event);
        }
        let (metadata, status, result, error) =
            if let Some(snapshot) = run.lifecycle.snapshots.get(execution_id) {
                (
                    &snapshot.metadata,
                    snapshot.status,
                    snapshot.result.clone(),
                    snapshot.error.clone(),
                )
            } else if run.execution_id == execution_id {
                (
                    &run.metadata,
                    run.status,
                    WorkflowResult::from(&run.result),
                    run.error.clone(),
                )
            } else {
                return Err("workflow execution snapshot not found".into());
            };
        if matches!(status, WorkflowStatus::Pending | WorkflowStatus::Running) {
            return Err("workflow execution has no terminal result to display".into());
        }
        let name = metadata
            .as_ref()
            .map(|metadata| metadata.name.clone())
            .unwrap_or_else(|| run.id.clone());
        let event = WorkflowLifecycleEvent {
            id,
            owner_thread_id: run.owner_thread_id,
            run_id: run_id.to_string(),
            execution_id: execution_id.to_string(),
            workflow_name: name.clone(),
            sequence: crate::lifecycle::MAX_LIFECYCLE_EVENTS as u64
                + run
                    .lifecycle
                    .snapshots
                    .keys()
                    .position(|id| id == execution_id)
                    .unwrap_or_default() as u64,
            created_at: crate::types::timestamp(),
            completion_mode: WorkflowCompletionMode::Display,
            kind: WorkflowLifecycleKind::ResultShown {
                name,
                status,
                result,
                error,
            },
        };
        files::atomic_json(
            &path,
            &DisplayReceipt {
                event: event.clone(),
                delivered: false,
            },
        )?;
        let _ = self.lifecycle_events.send(event.clone());
        Ok(event)
    }

    pub(crate) fn pending_display_events(
        &self,
        run: &WorkflowRun,
    ) -> Result<Vec<WorkflowLifecycleEvent>, String> {
        let executions = std::iter::once(&run.execution_id)
            .chain(run.lifecycle.snapshots.keys())
            .collect::<std::collections::BTreeSet<_>>();
        let mut events = Vec::new();
        for execution in executions {
            let path = self
                .root
                .join("display")
                .join(format!("{}.json", event_id(&run.id, execution)));
            match std::fs::metadata(&path) {
                Ok(_) => {
                    let receipt: DisplayReceipt = files::read_json(&path)?;
                    if !receipt.delivered {
                        events.push(receipt.event);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(events)
    }

    pub(crate) fn acknowledge_display_event(&self, run_id: &str, id: &str) -> Result<bool, String> {
        if id.len() != 52
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err("invalid workflow lifecycle event id".into());
        }
        let _state = self.state.lock().map_err(|error| error.to_string())?;
        let path = self.root.join("display").join(format!("{id}.json"));
        match std::fs::metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.to_string()),
            Ok(_) => {}
        }
        let mut receipt: DisplayReceipt = files::read_json(&path)?;
        if receipt.event.run_id != run_id || receipt.event.id != id {
            return Err("workflow display event belongs to another run".into());
        }
        if !receipt.delivered {
            receipt.delivered = true;
            files::atomic_json(&path, &receipt)?;
        }
        Ok(true)
    }
}
