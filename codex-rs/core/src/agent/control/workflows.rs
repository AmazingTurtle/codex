//! Local thread attachment for workflow observation; never consumes the thread event stream.

use super::runtime::LocalAgentRuntime;
use crate::CodexThread;
use codex_protocol::ThreadId;
use codex_protocol::error::Result;
use std::sync::Arc;

impl LocalAgentRuntime {
    pub(crate) async fn workflow_thread(&self, id: ThreadId) -> Result<Arc<CodexThread>> {
        self.upgrade()?.get_thread(id).await
    }

    pub(crate) async fn pin_workflow_observation(
        &self,
        thread: &Arc<CodexThread>,
    ) -> Result<Option<tokio::sync::OwnedRwLockReadGuard<()>>> {
        let state = self.upgrade()?;
        self.pin_v2_residency(&state, thread).await
    }
}
