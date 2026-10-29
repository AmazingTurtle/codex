//! Final workflow observation must retain its writer while capacity eviction competes.

use anyhow::Result;
use codex_core::ThreadEvictionOutcome;
use codex_core::config::Config;
use codex_core::workflows::WorkflowStatus;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_features::Feature;
use codex_history::RolloutItem;
use codex_protocol::ThreadId;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_thread_store::AppendThreadItemsParams;
use codex_thread_store::ArchiveThreadParams;
use codex_thread_store::CreateThreadParams;
use codex_thread_store::DeleteThreadParams;
use codex_thread_store::FindPersistedInputIdsParams;
use codex_thread_store::ListThreadsParams;
use codex_thread_store::LoadThreadHistoryParams;
use codex_thread_store::PersistContext;
use codex_thread_store::PersistedInputId;
use codex_thread_store::ReadThreadByRolloutPathParams;
use codex_thread_store::ReadThreadParams;
use codex_thread_store::ResumeThreadParams;
use codex_thread_store::StoredModelContext;
use codex_thread_store::StoredThread;
use codex_thread_store::StoredThreadHistory;
use codex_thread_store::ThreadPage;
use codex_thread_store::ThreadStore;
use codex_thread_store::ThreadStoreFuture;
use codex_thread_store::UpdateThreadMetadataParams;
use core_test_support::ThreadIdle;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::any::Any;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::oneshot;

struct FinalReadGate {
    reached: oneshot::Sender<ThreadId>,
    release: oneshot::Receiver<()>,
    failure: Option<String>,
}

struct GatedOutcomeStore {
    inner: OnceLock<Arc<dyn ThreadStore>>,
    gate: Mutex<Option<FinalReadGate>>,
}

macro_rules! delegate_store_methods {
    ($(fn $name:ident($param:ident: $params:ty) -> $result:ty;)*) => {
        $(fn $name(&self, $param: $params) -> ThreadStoreFuture<'_, $result> {
            self.inner.get().expect("configured local store").$name($param)
        })*
    };
}

impl ThreadStore for GatedOutcomeStore {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn default_history_mode(&self) -> ThreadHistoryMode {
        self.inner
            .get()
            .expect("configured local store")
            .default_history_mode()
    }

    delegate_store_methods! {
        fn create_thread(params: CreateThreadParams) -> ();
        fn resume_thread(params: ResumeThreadParams) -> Arc<Vec<RolloutItem>>;
        fn append_items(params: AppendThreadItemsParams) -> ();
        fn flush_thread(thread_id: ThreadId) -> ();
        fn shutdown_thread(thread_id: ThreadId) -> ();
        fn discard_thread(thread_id: ThreadId) -> ();
        fn load_history(params: LoadThreadHistoryParams) -> StoredThreadHistory;
        fn find_persisted_input_ids(params: FindPersistedInputIdsParams) -> BTreeSet<PersistedInputId>;
        fn read_thread(params: ReadThreadParams) -> StoredThread;
        fn read_thread_by_rollout_path(params: ReadThreadByRolloutPathParams) -> StoredThread;
        fn list_threads(params: ListThreadsParams) -> ThreadPage;
        fn update_thread_metadata(params: UpdateThreadMetadataParams) -> Option<StoredThread>;
        fn archive_thread(params: ArchiveThreadParams) -> ();
        fn unarchive_thread(params: ArchiveThreadParams) -> StoredThread;
        fn delete_thread(params: DeleteThreadParams) -> ();
    }

    fn persist_thread(
        &self,
        thread_id: ThreadId,
        context: PersistContext,
    ) -> ThreadStoreFuture<'_, ()> {
        self.inner
            .get()
            .expect("configured local store")
            .persist_thread(thread_id, context)
    }

    fn load_latest_model_context(
        &self,
        params: LoadThreadHistoryParams,
    ) -> ThreadStoreFuture<'_, StoredModelContext> {
        Box::pin(async move {
            let thread_id = params.thread_id;
            let history = self
                .inner
                .get()
                .expect("configured local store")
                .load_latest_model_context(params)
                .await?;
            if history
                .items
                .iter()
                .any(|item| matches!(item, RolloutItem::EventMsg(EventMsg::TurnComplete(_))))
            {
                let gate = self.gate.lock().expect("final read gate").take();
                if let Some(gate) = gate {
                    gate.reached.send(thread_id).expect("observe final read");
                    gate.release.await.expect("release final read");
                    if let Some(message) = gate.failure {
                        return Err(codex_thread_store::ThreadStoreError::Internal { message });
                    }
                }
            }
            Ok(history)
        })
    }
}

fn builder(configured_store: Arc<GatedOutcomeStore>) -> TestCodexBuilder {
    let mut extensions = ExtensionRegistryBuilder::<Config>::new();
    extensions.thread_lifecycle_contributor(Arc::new(ThreadIdle));
    test_codex()
        .with_model("gpt-5.6-sol")
        .with_thread_store(configured_store.clone())
        .with_extensions(Arc::new(extensions.build()))
        .with_model_info_override("gpt-5.6-sol", |model| {
            model.tool_mode = Some(ToolMode::Direct)
        })
        .with_config(move |config| {
            for feature in [
                Feature::Collab,
                Feature::CodeModeHost,
                Feature::DynamicWorkflows,
            ] {
                config
                    .features
                    .enable(feature)
                    .expect("enable workflow feature");
            }
            config.multi_agent_v2.non_code_mode_only = false;
            config.multi_agent_v2.max_concurrent_threads_per_session = 2;
            assert!(
                configured_store
                    .inner
                    .set(codex_core::thread_store_from_config(
                        config, /*state_db*/ None
                    ))
                    .is_ok()
            );
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_workflow_child_is_pinned_only_through_final_observation() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let (reached, observing) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let store = Arc::new(GatedOutcomeStore {
        inner: OnceLock::new(),
        gate: Mutex::new(Some(FinalReadGate {
            reached,
            release: released,
            failure: None,
        })),
    });
    let test = builder(store).build_with_auto_env(&server).await?;
    let worker = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_assistant_message("worker-result", r#"{"observed":true}"#),
            responses::ev_completed("worker-result"),
        ]),
    )
    .await;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut events = service.subscribe();
    let run = service
        .start(
            super::workflow_v2::script(
                "return await workflow.agent('Return a JSON result.', {label:'one'});",
            ),
            json!({}),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let child_id = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), observing).await??;
    let child = test.thread_manager.get_thread(child_id).await?;
    ThreadIdle::wait(&child).await;
    assert_eq!(
        test.thread_manager
            .try_evict_v2_thread(Arc::clone(&child))
            .await?,
        ThreadEvictionOutcome::Busy
    );
    assert_eq!(
        service.get(&run.id).map_err(anyhow::Error::msg)?.status,
        WorkflowStatus::Running
    );
    release
        .send(())
        .expect("release authoritative result observation");
    let completed = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        loop {
            let current = service.get(&run.id).map_err(anyhow::Error::msg)?;
            if matches!(
                current.status,
                WorkflowStatus::Succeeded | WorkflowStatus::Failed | WorkflowStatus::Interrupted
            ) {
                return Ok::<_, anyhow::Error>(current);
            }
            match events.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await??;
    assert_eq!(
        (completed.status, completed.result),
        (WorkflowStatus::Succeeded, Some(json!({"observed":true}))),
        "final observation must succeed after the gate opens: {:?}",
        completed.error
    );
    assert!(!completed.metrics.usage_complete);
    assert!(
        completed
            .metrics
            .usage_incomplete_reason
            .as_deref()
            .is_some_and(|reason| !reason.is_empty()),
        "a store without a historical relation index must retain explicit partial-accounting truth"
    );
    let step = completed
        .steps
        .values()
        .find(|step| step.label == "one")
        .expect("the real worker retains its authored label");
    assert_eq!(step.attempts.len(), 1);
    assert_eq!(
        step.attempts[0].child_thread_id.as_deref(),
        Some(child_id.to_string().as_str())
    );
    assert_eq!(
        test.thread_manager.try_evict_v2_thread(child).await?,
        ThreadEvictionOutcome::Evicted
    );
    assert!(test.thread_manager.get_thread(child_id).await.is_err());
    assert_eq!(worker.requests().len(), 1);
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_schema_repair_retains_feedback_and_its_remaining_attempts_across_restart()
-> Result<()> {
    use anyhow::Context;
    use codex_core::workflows::AdmissionStatus;
    use codex_protocol::protocol::ExecutionControlState;

    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let (reached, observing) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let store = Arc::new(GatedOutcomeStore {
        inner: OnceLock::new(),
        gate: Mutex::new(Some(FinalReadGate {
            reached,
            release: released,
            failure: None,
        })),
    });
    let mut test = builder(store)
        .with_config(|config| config.dynamic_workflows.max_agents = 10)
        .build_with_auto_env(&server)
        .await?;
    test.codex.ensure_rollout_materialized().await;
    test.codex.flush_rollout().await?;
    let stored_root = test
        .thread_store
        .read_thread(ReadThreadParams {
            thread_id: test.session_configured.thread_id,
            include_archived: true,
            include_history: false,
        })
        .await?;
    let rollout_path = stored_root
        .rollout_path
        .context("the materialized root's actual local store path")?;
    assert!(rollout_path.is_file());
    // The wrapper exposes its real backing store through the public metadata read;
    // the initial configured event cannot discover a path by downcasting the wrapper.
    test.session_configured.rollout_path = Some(rollout_path);
    // Verify the exact counts below, so mock destruction cannot mask an earlier
    // storage or lifecycle error with an unrelated ten-response expectation.
    let mut models = Vec::new();
    for index in 0..10 {
        models.push(
            responses::mount_sse_once(
                &server,
                responses::sse(vec![
                    responses::ev_assistant_message(
                        &format!("invalid-{index}"),
                        r#"{"ok":"invalid"}"#,
                    ),
                    responses::ev_completed_with_tokens(
                        &format!("invalid-{index}"),
                        /*total_tokens*/ 1,
                    ),
                ]),
            )
            .await,
        );
    }
    let model_requests = || {
        models
            .iter()
            .flat_map(responses::ResponseMock::requests)
            .collect::<Vec<_>>()
    };
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let run = service
        .start(
            super::workflow_v2::script(
                r#"
return await workflow.agent("Produce the requested Boolean object.", {label:"repair", schema:{
  type:"object", properties:{ok:{type:"boolean"}}, required:["ok"], additionalProperties:false
}});
"#,
            ),
            json!({}),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let first_child = tokio::time::timeout(Duration::from_secs(30), observing)
        .await
        .context(
            "the first invalid child never reached authoritative final-result observation",
        )??;
    assert_eq!(
        test.codex.pause_execution().await?.state,
        ExecutionControlState::Paused
    );
    release
        .send(())
        .expect("admit the real invalid result while corrective native acceptance remains paused");
    let pending = super::workflow_v2::observe(&service, &run.id, |run| {
        run.steps.values().any(|step| {
            step.label == "repair"
                && step.schema_attempts == 1
                && step.attempts.len() == 2
                && step.attempts[0].schema_error.is_some()
                && step.attempts[1].admission == AdmissionStatus::Reserved
                && step.attempts[1].child_thread_id.is_none()
                && step.attempts[1].validation_feedback.is_some()
        })
    })
    .await?;
    let step = pending
        .steps
        .values()
        .next()
        .context("pending corrective step")?;
    assert_eq!(pending.accepted_starts, 1);
    assert_eq!(
        step.attempts[0].child_thread_id.as_deref(),
        Some(first_child.to_string().as_str())
    );
    let feedback = step.attempts[1]
        .validation_feedback
        .clone()
        .context("durable repair feedback before acceptance")?;
    assert!(feedback.len() <= 2048 && feedback.contains("structured output validation failed"));
    assert_eq!(model_requests().len(), 1);
    let shutdown = test
        .thread_manager
        .request_agent_tree_shutdown(test.session_configured.thread_id)
        .await?;
    tokio::time::timeout(Duration::from_secs(30), shutdown.wait()).await??;
    drop(service);
    let fresh_store = Arc::new(GatedOutcomeStore {
        inner: OnceLock::new(),
        gate: Mutex::new(None),
    });
    let resumed = builder(fresh_store)
        .with_config(|config| config.dynamic_workflows.max_agents = 10)
        .restart_with_auto_env(&server, &test)
        .await?;
    let service = resumed
        .codex
        .workflows()
        .await
        .map_err(anyhow::Error::msg)?;
    let restored = service.get(&run.id).map_err(anyhow::Error::msg)?;
    assert_eq!(
        (restored.status, restored.accepted_starts),
        (WorkflowStatus::Interrupted, 1)
    );
    let restored_step = restored
        .steps
        .values()
        .next()
        .context("restored corrective step")?;
    assert_eq!(restored_step.schema_attempts, 1);
    assert_eq!(
        restored_step.attempts[1].validation_feedback.as_deref(),
        Some(feedback.as_str())
    );
    assert_eq!(
        resumed.codex.execution_state().await?.state,
        ExecutionControlState::Paused
    );
    service.recover(&run.id).await.map_err(anyhow::Error::msg)?;
    let ready = super::workflow_v2::observe(&service, &run.id, |run| {
        run.status == WorkflowStatus::Running
    })
    .await?;
    assert_eq!(
        ready.steps, restored.steps,
        "paused recovery must wait before replaying or reserving another corrective child"
    );
    assert_eq!(ready.accepted_starts, 1);
    assert_eq!(
        model_requests().len(),
        1,
        "recovery while paused cannot accept a corrective child"
    );
    resumed.codex.unpause_execution().await?;
    let capped = super::workflow_v2::observe(&service, &run.id, |run| {
        run.status == WorkflowStatus::Failed
    })
    .await?;
    let attempts = capped
        .steps
        .values()
        .map(|step| {
            (
                step.label.as_str(),
                step.schema_attempts,
                step.attempts
                    .iter()
                    .take(/*n*/ 16)
                    .map(|attempt| {
                        (
                            attempt.status,
                            attempt.admission,
                            attempt.child_thread_id.as_deref(),
                            attempt
                                .error
                                .as_deref()
                                .map(|error| error.chars().take(/*n*/ 512).collect::<String>()),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        capped
            .error
            .as_deref()
            .is_some_and(|error| error.contains("five attempts")),
        "first schema cycle ended before its cap: error={:?}; accepted_starts={}; attempts={attempts:?}",
        capped.error,
        capped.accepted_starts
    );
    assert_eq!((capped.accepted_starts, &capped.result), (5, &None));
    let step = capped
        .steps
        .values()
        .next()
        .context("bounded first schema cycle")?;
    assert_eq!(
        step.schema_attempts, 5,
        "recovery must preserve the first failed validation in the five-attempt cap"
    );
    let accepted = step
        .attempts
        .iter()
        .filter(|attempt| attempt.admission == AdmissionStatus::Accepted)
        .collect::<Vec<_>>();
    assert_eq!(accepted.len(), 5);
    assert!(
        accepted
            .iter()
            .skip(1)
            .all(|attempt| attempt.validation_feedback.is_some())
    );
    let requests = model_requests();
    assert_eq!(requests.len(), 5);
    assert!(
        requests[1].body_contains_text(&feedback),
        "the first corrective child after restart must receive the persisted feedback"
    );
    service.retry(&run.id).await.map_err(anyhow::Error::msg)?;
    let retried = super::workflow_v2::observe(&service, &run.id, |run| {
        run.status == WorkflowStatus::Failed
    })
    .await?;
    assert_eq!((retried.accepted_starts, &retried.result), (10, &None));
    assert!(
        retried
            .error
            .as_deref()
            .is_some_and(|error| error.contains("five attempts")),
        "{:?}",
        retried.error
    );
    let step = retried
        .steps
        .values()
        .next()
        .context("explicit new schema cycle")?;
    assert_eq!(step.schema_attempts, 5);
    let children = step
        .attempts
        .iter()
        .filter_map(|attempt| attempt.child_thread_id.as_ref())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        children.len(),
        10,
        "explicit retry grants a new five-attempt cycle, with every actual child charged"
    );
    assert_eq!(model_requests().len(), 10);
    service.retry(&run.id).await.map_err(anyhow::Error::msg)?;
    let exhausted = super::workflow_v2::observe(&service, &run.id, |run| {
        run.status == WorkflowStatus::Failed
    })
    .await?;
    assert_eq!((exhausted.accepted_starts, &exhausted.result), (10, &None));
    assert!(
        exhausted
            .error
            .as_deref()
            .is_some_and(|error| error.contains("agent-start limit exhausted (10)")),
        "{:?}",
        exhausted.error
    );
    assert_eq!(
        model_requests().len(),
        10,
        "resetting schema repair must never reset the configured global agent budget"
    );
    resumed.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_final_observation_rejects_parallel_instead_of_returning_null_and_recovers_the_same_child()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let (reached, observing) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let store = Arc::new(GatedOutcomeStore {
        inner: OnceLock::new(),
        gate: Mutex::new(Some(FinalReadGate {
            reached,
            release: released,
            failure: Some("final observation unavailable".to_owned()),
        })),
    });
    let test = builder(store).build_with_auto_env(&server).await?;
    let worker = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_assistant_message("actual-result", r#"{"observed":true}"#),
            responses::ev_completed("actual-result"),
        ]),
    )
    .await;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut events = service.subscribe();
    let run = service.start(super::workflow_v2::script(
        "return await workflow.parallel([() => workflow.agent('Return an authoritative result.',{label:'uncertain'})]);"), json!({}))
        .await.map_err(anyhow::Error::msg)?;
    let child = tokio::time::timeout(Duration::from_secs(30), observing).await??;
    worker.single_request();
    release
        .send(())
        .expect("fail only the final storage observation of a real completed child");
    let failed = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let run = service.get(&run.id).map_err(anyhow::Error::msg)?;
            if run.status == WorkflowStatus::Failed {
                return Ok::<_, anyhow::Error>(run);
            }
            // Resnapshot above after coalesced lifecycle/ACK notifications.
            match events.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await??;
    assert_eq!(
        (failed.result, failed.accepted_starts, failed.steps.len()),
        (None, 1, 1),
        "an unobserved outcome must reject the whole combinator rather than produce [null]"
    );
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains("final observation unavailable"))
    );
    let attempt = &failed
        .steps
        .values()
        .next()
        .expect("one accepted step")
        .attempts[0];
    assert_eq!(
        (attempt.status, attempt.child_thread_id.as_deref()),
        (
            WorkflowStatus::Interrupted,
            Some(child.to_string().as_str())
        )
    );
    service.recover(&run.id).await.map_err(anyhow::Error::msg)?;
    let recovered = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let run = service.get(&run.id).map_err(anyhow::Error::msg)?;
            if run.status == WorkflowStatus::Succeeded {
                return Ok::<_, anyhow::Error>(run);
            }
            match events.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await??;
    assert_eq!(
        (recovered.result, recovered.accepted_starts),
        (Some(json!([{"observed":true}])), 1)
    );
    let attempts = &recovered
        .steps
        .values()
        .next()
        .expect("one reconciled step")
        .attempts;
    assert_eq!(attempts.len(), 1);
    assert_eq!(
        attempts[0].child_thread_id.as_deref(),
        Some(child.to_string().as_str())
    );
    assert_eq!(
        worker.requests().len(),
        1,
        "reconciliation must not spawn or sample another child"
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
