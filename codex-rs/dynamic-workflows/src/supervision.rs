//! Native observations outlive the JavaScript callback that originally awaited them.

use std::future::Future;
use std::sync::Mutex;

use tokio::sync::oneshot;
use tokio_util::task::TaskTracker;

/// Closing admission is synchronous; draining never cancels already accepted work.
pub(crate) struct AgentSupervision {
    accepting: Mutex<bool>,
    tasks: TaskTracker,
}

impl Default for AgentSupervision {
    fn default() -> Self {
        Self {
            accepting: Mutex::new(true),
            tasks: TaskTracker::new(),
        }
    }
}

impl AgentSupervision {
    pub(crate) fn spawn<T: Send + 'static>(
        &self,
        future: impl Future<Output = Result<T, String>> + Send + 'static,
    ) -> Result<oneshot::Receiver<Result<T, String>>, String> {
        let accepting = self.accepting.lock().map_err(|error| error.to_string())?;
        if !*accepting {
            return Err("workflow revision is fenced against new work".into());
        }
        let (sender, receiver) = oneshot::channel();
        self.tasks.spawn(async move {
            // A discarded callback receiver does not discard an owned result observation.
            let result = future.await;
            let _ = sender.send(result);
        });
        drop(accepting);
        Ok(receiver)
    }

    pub(crate) fn fence(&self) -> Result<(), String> {
        *self.accepting.lock().map_err(|error| error.to_string())? = false;
        self.tasks.close();
        Ok(())
    }

    pub(crate) async fn drain(&self) {
        self.tasks.wait().await;
    }
}

#[cfg(test)]
#[path = "supervision_tests.rs"]
mod tests;
