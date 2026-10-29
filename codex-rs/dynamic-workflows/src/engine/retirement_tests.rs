use super::*;
use crate::AgentOutcome;
use crate::AgentRequest;
use crate::AgentStartError;
use crate::AgentStartMode;
use pretty_assertions::assert_eq;
use serde_json::json;

struct NoNative;
impl WorkflowHost for NoNative {
    async fn schedule_agent(
        &self,
        _request: AgentRequest,
        _mode: AgentStartMode,
    ) -> Result<String, AgentStartError> {
        panic!("retiring an old stack frame must not dispatch native work")
    }
    async fn wait_agent(
        &self,
        _request: AgentRequest,
        _child: String,
    ) -> Result<AgentOutcome, String> {
        panic!("retiring an old stack frame must not observe another native execution")
    }
    async fn stop_run(&self, _id: String) -> Result<(), String> {
        panic!("a stale guard must not stop the replacement execution")
    }
}

#[tokio::test]
async fn an_old_guard_never_interrupts_or_removes_a_replacement_execution() {
    for completed in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let root = AbsolutePathBuf::from_absolute_path_checked(directory.path()).unwrap();
        let store = Arc::new(Store::new(root.clone()).unwrap());
        let mut replacement: WorkflowRun = serde_json::from_value(json!({
            "id":uuid::Uuid::new_v4().to_string(),"formatVersion":2,"executionId":uuid::Uuid::new_v4().to_string(),
            "ownerThreadId":"owner","source":"workflow.meta({name:'replacement',description:'Exact retirement',phases:[]}); return null;",
            "input":{},"status":"running","phase":null,"steps":{},"error":null,"createdAt":1,"updatedAt":1
        })).unwrap();
        store.insert(&mut replacement).unwrap();
        let persisted = crate::read_persisted(&root, &replacement.id).unwrap();
        let fresh = Arc::new(ActiveRun::default());
        let active = Arc::new(Mutex::new(HashMap::from([(
            replacement.id.clone(),
            Arc::clone(&fresh),
        )])));
        let previous = Arc::new(ActiveRun::default());
        drop(RunGuard {
            store: Arc::clone(&store),
            active: Arc::clone(&active),
            run_id: replacement.id.clone(),
            finished: Arc::default(),
            host: Arc::new(NoNative),
            completed,
            control: Arc::clone(&previous),
        });
        assert!(Arc::ptr_eq(
            active.lock().unwrap().get(&replacement.id).unwrap(),
            &fresh
        ));
        assert_eq!(
            crate::read_persisted(&root, &replacement.id).unwrap(),
            persisted,
            "the stale guard cannot mutate any durable replacement state"
        );
        let created_ms = u128::try_from(persisted.created_at).unwrap() * 1000;
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let mut live = store.get(&replacement.id).unwrap();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let elapsed = u128::from(live.metrics.wall_duration_ms.unwrap());
        assert!(
            (before.saturating_sub(created_ms)..=after.saturating_sub(created_ms))
                .contains(&elapsed),
            "a live wall projection must use this read's actual clock bounds"
        );
        // Only the independently bounded live wall projection differs from the exact disk snapshot.
        live.metrics.wall_duration_ms = persisted.metrics.wall_duration_ms;
        assert_eq!(live, persisted);
        assert!(
            previous
                .runner_done
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        assert!(!fresh.runner_done.load(std::sync::atomic::Ordering::SeqCst));
    }
}
