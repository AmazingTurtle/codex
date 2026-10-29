//! Persist known rejection even if the JavaScript callback that awaited a start is dropped.

use codex_dynamic_workflows::WorkflowHostEvent;
use codex_dynamic_workflows::WorkflowObserver;
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;

#[path = "receipt_control.rs"]
mod control;
pub(super) use control::StartDisposition;

const PREPARED: u8 = 0;
const ADMITTED: u8 = 1;
const ACCEPTED: u8 = 2;
const REJECTED: u8 = 3;

pub(super) struct StartReceipt {
    attempt: String,
    revision_id: String,
    admission_cancellation: CancellationToken,
    observer: WorkflowObserver,
    state: AtomicU8,
}

pub(super) struct ReceiptLease(pub Arc<StartReceipt>);

impl StartReceipt {
    pub(super) fn new(
        attempt: String,
        revision_id: String,
        admission_cancellation: CancellationToken,
        observer: WorkflowObserver,
    ) -> Arc<Self> {
        Arc::new(Self {
            attempt,
            revision_id,
            admission_cancellation,
            observer,
            state: AtomicU8::new(PREPARED),
        })
    }

    pub(super) fn is_prepared(&self) -> bool {
        self.state.load(Ordering::Acquire) == PREPARED
    }

    pub(super) fn admitted(&self) -> Result<(), String> {
        if self.admission_cancellation.is_cancelled() {
            return Err("workflow native admission was cancelled before acceptance".into());
        }
        self.state.store(ADMITTED, Ordering::Release);
        (self.observer)(WorkflowHostEvent::StartUncertain {
            attempt_id: self.attempt.clone(),
            revision_id: self.revision_id.clone(),
        })
    }

    pub(super) fn retry_known_rejection(&self) -> Result<(), String> {
        (self.observer)(WorkflowHostEvent::StartRetryable {
            attempt_id: self.attempt.clone(),
        })?;
        let _ =
            self.state
                .compare_exchange(ADMITTED, PREPARED, Ordering::AcqRel, Ordering::Acquire);
        Ok(())
    }

    pub(super) fn accept(&self, child: String) -> Result<(), String> {
        self.state.store(ACCEPTED, Ordering::Release);
        (self.observer)(WorkflowHostEvent::StartAccepted {
            attempt_id: self.attempt.clone(),
            child_thread_id: child,
        })
    }

    pub(super) fn reject(&self, reason: &str) -> Result<(), String> {
        if self
            .state
            .compare_exchange(PREPARED, REJECTED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            (self.observer)(WorkflowHostEvent::StartRejected {
                attempt_id: self.attempt.clone(),
                reason: reason.to_string(),
            })?;
        }
        Ok(())
    }
}

impl Drop for ReceiptLease {
    fn drop(&mut self) {
        if let Err(error) = self
            .0
            .reject("workflow start ended before native admission")
        {
            tracing::warn!(%error, "failed to persist workflow start rejection");
        }
    }
}
