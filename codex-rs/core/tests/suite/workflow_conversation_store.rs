//! One dependency failure at the real root lifecycle append/flush boundary.

use codex_core::config::Config;
use codex_history::RolloutItem;
use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_thread_store::*;
use std::any::Any;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

#[derive(Clone, Copy)]
pub(super) enum FaultBoundary {
    Append,
    Flush,
}

pub(super) struct PublicationFaultStore {
    inner: OnceLock<Arc<dyn ThreadStore>>,
    boundary: FaultBoundary,
    completed_append: AtomicBool,
    failed: AtomicBool,
}

impl PublicationFaultStore {
    pub(super) fn new(boundary: FaultBoundary) -> Self {
        Self {
            inner: OnceLock::new(),
            boundary,
            completed_append: AtomicBool::new(false),
            failed: AtomicBool::new(false),
        }
    }
    pub(super) fn configure(&self, config: &Config) {
        assert!(
            self.inner
                .set(codex_core::thread_store_from_config(
                    config, /*state_db*/ None
                ))
                .is_ok()
        );
    }
    pub(super) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
    fn inner(&self) -> &Arc<dyn ThreadStore> {
        self.inner
            .get()
            .expect("configured real local thread store")
    }
}

macro_rules! delegate {
    ($(fn $name:ident($arg:ident:$params:ty)->$result:ty;)*) => {
        $(fn $name(&self,$arg:$params)->ThreadStoreFuture<'_,$result> {self.inner().$name($arg)})*
    };
}

impl ThreadStore for PublicationFaultStore {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn default_history_mode(&self) -> ThreadHistoryMode {
        self.inner().default_history_mode()
    }
    delegate! {
        fn create_thread(params:CreateThreadParams)->();
        fn resume_thread(params:ResumeThreadParams)->Arc<Vec<RolloutItem>>;
        fn shutdown_thread(thread_id:ThreadId)->();
        fn discard_thread(thread_id:ThreadId)->();
        fn load_history(params:LoadThreadHistoryParams)->StoredThreadHistory;
        fn load_latest_model_context(params:LoadThreadHistoryParams)->StoredModelContext;
        fn find_persisted_input_ids(params:FindPersistedInputIdsParams)->BTreeSet<PersistedInputId>;
        fn read_thread(params:ReadThreadParams)->StoredThread;
        fn read_thread_by_rollout_path(params:ReadThreadByRolloutPathParams)->StoredThread;
        fn list_threads(params:ListThreadsParams)->ThreadPage;
        fn update_thread_metadata(params:UpdateThreadMetadataParams)->Option<StoredThread>;
        fn archive_thread(params:ArchiveThreadParams)->();
        fn unarchive_thread(params:ArchiveThreadParams)->StoredThread;
        fn delete_thread(params:DeleteThreadParams)->();
    }
    fn persist_thread(
        &self,
        thread_id: ThreadId,
        context: PersistContext,
    ) -> ThreadStoreFuture<'_, ()> {
        self.inner().persist_thread(thread_id, context)
    }
    fn append_items(&self, params: AppendThreadItemsParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            let completed=params.items.iter().any(|item| matches!(item,RolloutItem::EventMsg(EventMsg::WorkflowLifecycle(fact))
                if matches!(&fact.kind,codex_protocol::workflow::WorkflowLifecycleKind::Completed {..})));
            if completed
                && matches!(self.boundary, FaultBoundary::Append)
                && !self.failed.swap(true, Ordering::AcqRel)
            {
                return Err(ThreadStoreError::Internal {
                    message: "injected lifecycle completion append failure".into(),
                });
            }
            self.inner().append_items(params).await?;
            if completed {
                self.completed_append.store(true, Ordering::Release);
            }
            Ok(())
        })
    }
    fn flush_thread(&self, thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            if matches!(self.boundary, FaultBoundary::Flush)
                && self.completed_append.load(Ordering::Acquire)
                && !self.failed.swap(true, Ordering::AcqRel)
            {
                return Err(ThreadStoreError::Internal {
                    message: "injected lifecycle completion flush failure".into(),
                });
            }
            self.inner().flush_thread(thread_id).await
        })
    }
}
