//! Saved-library invalidation uses the existing connection-owned filesystem watches.
use super::AppServerSession;
use super::fs::AppServerFileSystem;
use codex_app_server_client::AppServerPath;
use codex_utils_path_uri::LegacyAppPathString;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;

pub(crate) const WATCH_PREFIX: &str = "workflow-library-";

pub(crate) struct LibraryWatch {
    fs: AppServerFileSystem,
    state: Arc<LibraryWatchState>,
    generation: u64,
}

#[derive(Default)]
pub(super) struct LibraryWatchState {
    entries: Mutex<Vec<(String, String)>>,
    generation: AtomicU64,
}

impl AppServerSession {
    pub(crate) fn workflow_library_watch(&self) -> LibraryWatch {
        LibraryWatch {
            fs: self.file_system(),
            state: Arc::clone(&self.workflow_library_watches),
            generation: self
                .workflow_library_watches
                .generation
                .fetch_add(1, Ordering::AcqRel)
                + 1,
        }
    }
}

impl LibraryWatch {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "watch replacement serializes unwatch and watch RPCs with its tracked entries"
    )]
    pub(crate) async fn update(&self, roots: Vec<LegacyAppPathString>) {
        let roots = roots
            .into_iter()
            .map(|root| root.to_string())
            .collect::<Vec<_>>();
        let mut entries = self.state.entries.lock().await;
        if self.state.generation.load(Ordering::Acquire) != self.generation {
            return;
        }
        for (id, _) in entries.iter().filter(|(_, path)| !roots.contains(path)) {
            if let Err(error) = self.fs.fs_unwatch(id.clone()).await {
                tracing::warn!(%error, "could not release saved workflow library watch");
            }
        }
        entries.retain(|(_, path)| roots.contains(path));
        for path in roots {
            if entries.iter().any(|(_, watched)| watched == &path) {
                continue;
            }
            let id = format!("{WATCH_PREFIX}{}", uuid::Uuid::new_v4());
            match self
                .fs
                .fs_watch_path(&id, &AppServerPath::from_app_server(path.clone()))
                .await
            {
                Ok(()) => entries.push((id, path)),
                Err(error) => {
                    tracing::warn!(%error, "saved workflow changes require manual refresh because watching is unavailable")
                }
            }
        }
    }
}
