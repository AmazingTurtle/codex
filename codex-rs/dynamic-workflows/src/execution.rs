//! Cleanup attempts are shared; failure retains the run's recovery fence.
use crate::WorkflowHost;
use crate::WorkflowStatus;
use crate::store::Store;
use codex_code_mode_protocol::CodeModeSession;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
pub(crate) enum TeardownMode {
    Join,
    RetryFailed,
}

pub(crate) struct ActiveRun {
    pub(crate) cancellation: CancellationToken,
    pub(crate) session: Mutex<Option<Arc<dyn CodeModeSession>>>,
    pub(crate) failed: AtomicBool,
    pub(crate) runner_done: AtomicBool,
    pub(crate) revision_draining: AtomicBool,
    pub(crate) revision_cancel: CancellationToken,
    pub(crate) supervision: Arc<crate::supervision::AgentSupervision>,
    settled: tokio::sync::Mutex<Option<Result<(), String>>>,
}

impl Default for ActiveRun {
    fn default() -> Self {
        Self {
            cancellation: CancellationToken::new(),
            session: Mutex::new(None),
            failed: AtomicBool::new(false),
            runner_done: AtomicBool::new(false),
            revision_draining: AtomicBool::new(false),
            revision_cancel: CancellationToken::new(),
            supervision: Arc::default(),
            settled: tokio::sync::Mutex::new(None),
        }
    }
}

impl ActiveRun {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "serialize teardown and memoize its result before concurrent stop or recovery can proceed"
    )]
    pub(crate) async fn stop<H: WorkflowHost>(
        &self,
        host: &H,
        store: &Store,
        id: &str,
        mode: TeardownMode,
    ) -> Result<(), String> {
        let mut settled = self.settled.lock().await;
        let retry = matches!(mode, TeardownMode::RetryFailed)
            && settled.as_ref().is_some_and(Result::is_err);
        if !retry && let Some(result) = settled.as_ref() {
            return result.clone();
        }
        let result: Result<(), String> = async {
            self.cancellation.cancel();
            self.supervision.fence()?;
            host.stop_run(id.to_string()).await?;
            store.update(id, |run| {
                for attempt in run.steps.values_mut().flat_map(|step| &mut step.attempts) {
                    if attempt.admission == crate::AdmissionStatus::Reserved
                        && attempt.child_thread_id.is_none()
                    {
                        attempt.admission = crate::AdmissionStatus::Rejected;
                        attempt.status = WorkflowStatus::Failed;
                        attempt.error = Some("workflow stopped before native admission".into());
                    }
                }
                Ok(())
            })?;
            let session = self
                .session
                .lock()
                .map_err(|error| error.to_string())?
                .clone();
            if let Some(session) = session {
                session.shutdown().await?;
                self.session
                    .lock()
                    .map_err(|error| error.to_string())?
                    .take();
            }
            self.supervision.drain().await;
            host.finish_run(id.to_string()).await?;
            if retry {
                store.update(id, |run| {
                    run.error = None;
                    Ok(())
                })?;
            }
            Ok(())
        }
        .await;
        self.failed.store(result.is_err(), Ordering::SeqCst);
        if let Err(error) = &result {
            Self::persist_failure(store, id, error);
        }
        *settled = Some(result.clone());
        result
    }

    pub(crate) async fn failed_finish(
        &self,
        store: &Store,
        id: &str,
        error: String,
    ) -> Result<(), String> {
        let mut settled = self.settled.lock().await;
        if settled.as_ref().is_some_and(Result::is_ok) {
            return Ok(());
        }
        self.failed.store(true, Ordering::SeqCst);
        Self::persist_failure(store, id, &error);
        *settled = Some(Err(error.clone()));
        Err(error)
    }

    fn persist_failure(store: &Store, id: &str, error: &str) {
        let _ = store.update(id, |run| {
            if run.status == WorkflowStatus::Running {
                run.status = WorkflowStatus::Interrupted;
            }
            run.error = Some(format!("workflow cleanup did not settle: {error}"));
            Ok(())
        });
    }
}
