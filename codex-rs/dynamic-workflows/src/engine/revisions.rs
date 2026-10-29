//! Revision application fences admission promptly and retains a separate asynchronous drain.

mod resume;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::WorkflowEngine;
use crate::AdmissionStatus;
use crate::RevisionApplyMode;
use crate::RevisionApplyRequest;
use crate::RevisionStageRequest;
use crate::WorkflowHost;
use crate::WorkflowRevision;
use crate::WorkflowRevisionResponse;
use crate::WorkflowRevisionStatus;
use crate::WorkflowStatus;
use crate::execution::ActiveRun;

impl<H: WorkflowHost> WorkflowEngine<H> {
    pub async fn wait_revision_settlement(
        &self,
        id: &str,
        revision_id: &str,
    ) -> Result<WorkflowRevisionResponse, String> {
        let mut events = self.subscribe();
        loop {
            let run = self.get(id)?;
            let response = run.steering.revision_response(revision_id)?;
            if matches!(
                response.revision.status,
                WorkflowRevisionStatus::Ready
                    | WorkflowRevisionStatus::Blocked
                    | WorkflowRevisionStatus::Active
            ) {
                return Ok(response);
            }
            match events.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    }
    pub fn list_revisions(
        &self,
        id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkflowRevision>, String> {
        let mut revisions: Vec<_> = self.get(id)?.steering.revisions.into_values().collect();
        revisions.sort_by(|left, right| {
            left.number
                .cmp(&right.number)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(revisions
            .into_iter()
            .skip(offset)
            .take(limit.min(crate::MAX_PAGE_SIZE))
            .collect())
    }

    pub fn stage_revision(
        &self,
        id: &str,
        request: RevisionStageRequest,
    ) -> Result<WorkflowRevisionResponse, String> {
        if let Some(response) = self.repeated_revision(
            id,
            &request.command_id,
            &serde_json::to_value(&request).map_err(|error| error.to_string())?,
        )? {
            return Ok(response);
        }
        self.store.update(id, |run| {
            let mut steering = std::mem::take(&mut run.steering);
            let response = steering.stage(run, &request)?;
            run.steering = steering;
            Ok(response)
        })
    }

    /// Returns after durable fencing. Accepted agents finish in the retained drain task.
    pub async fn apply_revision(
        &self,
        id: &str,
        request: RevisionApplyRequest,
    ) -> Result<WorkflowRevisionResponse, String> {
        if let Some(response) = self.repeated_revision(
            id,
            &request.command_id,
            &serde_json::to_value(&request).map_err(|error| error.to_string())?,
        )? {
            return Ok(response);
        }
        let (response, control, fresh) = {
            let active = self.active.lock().map_err(|error| error.to_string())?;
            let control = active.get(id).cloned();
            let prior = self.get(id)?;
            crate::orchestration::require_executable(&prior)?;
            let repeated = prior.steering.mutations.contains_key(&request.command_id);
            if !repeated && let Some(control) = &control {
                control.revision_draining.store(true, Ordering::SeqCst);
            }
            let result = self
                .store
                .update(id, |run| run.steering.begin_apply(&request));
            if result.is_err()
                && !repeated
                && let Some(control) = &control
            {
                control.revision_draining.store(false, Ordering::SeqCst);
            }
            (result?, control, !repeated)
        };
        if fresh {
            let engine = self.clone();
            let run_id = id.to_string();
            let revision_id = request.revision_id.clone();
            tokio::spawn(async move {
                if let Err(error) = engine
                    .drain_revision(&run_id, &revision_id, request.mode, control.clone())
                    .await
                {
                    if let Some(control) = &control {
                        let _ = control
                            .failed_finish(&engine.store, &run_id, error.clone())
                            .await;
                    }
                    let _ = engine.store.update(&run_id, |run| {
                        run.steering.state.status = WorkflowRevisionStatus::Blocked;
                        if let Some(revision) = run.steering.revisions.get_mut(&revision_id) {
                            revision.status = WorkflowRevisionStatus::Blocked;
                            revision.error = Some(error.chars().take(2048).collect());
                        }
                        if matches!(
                            run.status,
                            WorkflowStatus::Running | WorkflowStatus::Pending
                        ) {
                            run.status = WorkflowStatus::Interrupted;
                            run.error = Some(error.chars().take(2048).collect());
                            run.lifecycle.terminal_ready = true;
                            run.lifecycle.interrupted_unsettled = true;
                        }
                        Ok(())
                    });
                    engine.finished.notify_waiters();
                }
            });
        }
        Ok(response)
    }

    pub(super) fn repeated_revision(
        &self,
        id: &str,
        command_id: &str,
        payload: &serde_json::Value,
    ) -> Result<Option<WorkflowRevisionResponse>, String> {
        crate::steering::validate_command_id(command_id)?;
        let run = self.get(id)?;
        match run.steering.repeated_mutation(command_id, payload)? {
            Some(record) => run
                .steering
                .revision_response(
                    record
                        .revision_id
                        .as_deref()
                        .ok_or("workflow command belongs to a step control")?,
                )
                .map(Some),
            None => Ok(None),
        }
    }

    async fn drain_revision(
        &self,
        id: &str,
        revision_id: &str,
        mode: RevisionApplyMode,
        control: Option<Arc<ActiveRun>>,
    ) -> Result<(), String> {
        let old_revision = self.get(id)?.steering.state.active_revision_id;
        if let Some(control) = &control {
            control.supervision.fence()?;
            control.revision_cancel.cancel();
        }
        let receipt = self.host.fence_run(id.to_string(), old_revision).await?;
        self.store.update(id, |run| {
            if run.steering.state.pending_revision_id.as_deref() != Some(revision_id) {
                return Err("workflow pending revision changed".into());
            }
            run.steering.state.status = WorkflowRevisionStatus::Draining;
            run.steering
                .revisions
                .get_mut(revision_id)
                .ok_or("workflow revision missing")?
                .status = WorkflowRevisionStatus::Draining;
            for attempt in run.steps.values_mut().flat_map(|step| &mut step.attempts) {
                if attempt.admission == AdmissionStatus::Reserved
                    && attempt.child_thread_id.is_none()
                {
                    attempt.admission = AdmissionStatus::Rejected;
                    attempt.status = WorkflowStatus::Failed;
                    attempt.error =
                        Some("workflow revision fenced before native acceptance".into());
                }
            }
            Ok(())
        })?;
        if let Some(control) = &control {
            if mode == RevisionApplyMode::InterruptAndApply {
                self.interrupt_revision_attempts(id).await?;
            }
            control.supervision.drain().await;
            loop {
                let done = self.finished.notified();
                tokio::pin!(done);
                done.as_mut().enable();
                if control.runner_done.load(Ordering::SeqCst) {
                    break;
                }
                done.await;
            }
            let session = control
                .session
                .lock()
                .map_err(|error| error.to_string())?
                .clone();
            if let Some(session) = session {
                session.shutdown().await?;
                control
                    .session
                    .lock()
                    .map_err(|error| error.to_string())?
                    .take();
            }
        }
        self.host.finish_run(id.to_string()).await?;
        let run = self.get(id)?;
        if !receipt.uncertain_attempt_ids.is_empty()
            || run
                .steps
                .values()
                .flat_map(|step| &step.attempts)
                .any(|attempt| {
                    attempt.admission == AdmissionStatus::Uncertain
                        || (attempt.admission == AdmissionStatus::Accepted
                            && matches!(
                                attempt.status,
                                WorkflowStatus::Running | WorkflowStatus::Interrupted
                            ))
                })
        {
            return Err("workflow revision is blocked by an unresolved native attempt; recover the original plan before resuming".into());
        }
        self.store.update(id, |run| {
            if run.status == WorkflowStatus::Stopped {
                return Err("workflow revision was stopped before its drain completed".into());
            }
            run.steering.state.status = WorkflowRevisionStatus::Ready;
            run.steering
                .revisions
                .get_mut(revision_id)
                .ok_or("workflow revision missing")?
                .status = WorkflowRevisionStatus::Ready;
            if matches!(
                run.status,
                WorkflowStatus::Running | WorkflowStatus::Pending
            ) {
                run.status = WorkflowStatus::Interrupted;
                run.error =
                    Some("workflow plan revision is ready; explicit resume required".into());
                run.lifecycle.terminal_ready = true;
                run.lifecycle.interrupted_unsettled = false;
            }
            Ok(())
        })?;
        if control.is_some() {
            self.active
                .lock()
                .map_err(|error| error.to_string())?
                .remove(id);
            self.finished.notify_waiters();
        }
        Ok(())
    }

    async fn interrupt_revision_attempts(&self, id: &str) -> Result<(), String> {
        let run = self.get(id)?;
        for step in run.steps.values() {
            let Some(attempt) = step.attempts.last().filter(|attempt| {
                attempt.admission == AdmissionStatus::Accepted
                    && attempt.status == WorkflowStatus::Running
            }) else {
                continue;
            };
            let child = attempt
                .child_thread_id
                .clone()
                .ok_or("accepted attempt identity missing")?;
            let request = super::controls::request(&run, step, attempt)?;
            let turn = self
                .host
                .active_owned_turn(request.clone(), child.clone())
                .await?
                .ok_or("workflow interruption requires a proven current owned turn identity")?;
            let receipt = self.host.interrupt_attempt(request, child, turn).await?;
            if !receipt.settled {
                return Err(
                    "workflow native interruption did not settle; refresh its active owned turn"
                        .into(),
                );
            }
            self.store.update(id, |run| {
                let attempt = run
                    .steps
                    .values_mut()
                    .flat_map(|step| &mut step.attempts)
                    .find(|candidate| candidate.id == attempt.id)
                    .ok_or("workflow native attempt missing")?;
                attempt.status = WorkflowStatus::Failed;
                attempt.error =
                    Some("workflow worker interrupted for an explicit plan revision".into());
                Ok(())
            })?;
        }
        Ok(())
    }
}
