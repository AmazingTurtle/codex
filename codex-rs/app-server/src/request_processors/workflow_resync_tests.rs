//! An existing listener-start gate makes actual bounded workflow buses lag.
//!
//! Register under thread_lifecycle only after the current validation source freeze.

use super::ListenerStart;
use super::ListenerTaskContext;
use super::ensure_conversation_listener;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingEnvelope;
use crate::outgoing_message::OutgoingMessage;
use crate::outgoing_message::OutgoingMessageSender;
use crate::skills_watcher::SkillsWatcher;
use crate::thread_state::ConnectionCapabilities;
use crate::thread_state::ThreadStateManager;
use crate::thread_status::ThreadWatchManager;
use anyhow::Result;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::WorkflowStatus as ApiStatus;
use codex_core::workflows::DynamicWorkflowService;
use codex_core::workflows::WorkflowRun;
use codex_core::workflows::WorkflowStatus;
use codex_features::Feature;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::MultiAgentVersion;
use core_test_support::responses;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

async fn settled(service: &DynamicWorkflowService, id: &str) -> Result<WorkflowRun> {
    let mut updates = service.subscribe();
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        loop {
            let run = service.get(id).map_err(anyhow::Error::msg)?;
            if run.status == WorkflowStatus::Succeeded
                && run.lifecycle.terminal_ready
                && run.lifecycle.delivered.len() == run.lifecycle.events.len()
            {
                return Ok(run);
            }
            anyhow::ensure!(
                !matches!(run.status, WorkflowStatus::Failed | WorkflowStatus::Stopped),
                "native VM run failed: {:?}",
                run.error
            );
            match updates.recv().await {
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await?
}

fn source(value: usize) -> String {
    format!(
        "workflow.meta({{name:'resync',description:'Retain this independently completed report',phases:[]}}); return {{value:{value}}};"
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lagged_real_listener_refreshes_older_terminal_runs_without_foreign_owner_or_inference()
-> Result<()> {
    core_test_support::skip_if_no_network!(Ok(()));
    let model = responses::start_mock_server().await;
    let test = test_codex()
        .with_model("gpt-5.6-sol")
        .with_model_info_override("gpt-5.6-sol", |model| {
            model.tool_mode = Some(ToolMode::Direct);
            model.multi_agent_version = Some(MultiAgentVersion::V2);
        })
        .with_config(|config| {
            for feature in [
                Feature::Collab,
                Feature::MultiAgentV2,
                Feature::CodeModeHost,
                Feature::DynamicWorkflows,
            ] {
                config
                    .features
                    .enable(feature)
                    .expect("enable the actual workflow VM");
            }
            config.multi_agent_v2.non_code_mode_only = false;
        })
        .build_with_auto_env(&model)
        .await?;
    let root = test.session_configured.thread_id;
    let other = test
        .thread_manager
        .start_thread(test.start_thread_options().await)
        .await?;
    let foreign = other
        .thread
        .start_workflow(source(999), json!({}))
        .await
        .map_err(anyhow::Error::msg)?;
    let foreign_service = other.thread.workflows().await.map_err(anyhow::Error::msg)?;
    settled(foreign_service.as_ref(), &foreign.id).await?;
    let service = test.codex.workflows().await.map_err(anyhow::Error::msg)?;
    let mut snapshot_proof = service.subscribe();
    let mut progress_proof = service.subscribe_progress();
    let (outgoing_tx, mut client) = mpsc::channel(2);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        outgoing_tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let states = ThreadStateManager::new();
    let connection = ConnectionId(71);
    states
        .connection_initialized(connection, ConnectionCapabilities::default())
        .await;
    let watches = ThreadWatchManager::new_with_outgoing(Arc::clone(&outgoing));
    watches.upsert_thread_silently(&root.to_string()).await;
    let skills = SkillsWatcher::new(
        test.thread_manager.skills_service(),
        &test.config.codex_home,
        Arc::clone(&outgoing),
    );
    let (release, gate) = oneshot::channel();
    ensure_conversation_listener(
        ListenerTaskContext {
            thread_manager: Arc::clone(&test.thread_manager),
            thread_store: Arc::clone(&test.thread_store),
            thread_state_manager: states.clone(),
            outgoing,
            pending_thread_unloads: Arc::new(Mutex::new(HashMap::new())),
            thread_watch_manager: watches,
            codex_home: test.config.codex_home.to_path_buf(),
            skills_watcher: Arc::clone(&skills),
            turn_cost_worker: None,
        },
        root,
        connection,
        /*raw_events_enabled*/ false,
        ListenerStart::AfterLifecycle(gate),
    )
    .await
    .map_err(|error| anyhow::anyhow!("attach the actual gated listener: {error:?}"))?;
    let mut expected = BTreeMap::new();
    // Zero-agent runs may emit only one compact progress update. Fill the actual
    // 32-entry progress bus, rather than assuming every state commit uses it.
    for value in 0..40 {
        let run = test
            .codex
            .start_workflow(source(value), json!({}))
            .await
            .map_err(anyhow::Error::msg)?;
        let run = settled(&service, &run.id).await?;
        expected.insert(run.id, json!({"value":value}));
        if expected.len() >= 3 && snapshot_proof.len() > 8 && progress_proof.len() > 32 {
            break;
        }
    }
    assert!(
        matches!(snapshot_proof.recv().await,Err(broadcast::error::RecvError::Lagged(skipped)) if skipped>0),
        "the real snapshot bus must actually evict notifications while the listener gate is held"
    );
    assert!(
        matches!(progress_proof.recv().await,Err(broadcast::error::RecvError::Lagged(skipped)) if skipped>0),
        "the compact progress bus must also actually lag, without changing its capacity"
    );
    assert!(
        client.try_recv().is_err(),
        "the production lifecycle gate must hold the subscriber before forwarding any update"
    );
    let earliest = expected
        .iter()
        .find(|(_, result)| result["value"] == 0)
        .expect("the older completed report")
        .0
        .clone();
    release
        .send(())
        .expect("release the existing production listener-start barrier");
    let (snapshots, progress) = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        let mut snapshots = BTreeSet::new();
        let mut progress = BTreeSet::new();
        while snapshots.len() < expected.len() || progress.len() < expected.len() {
            let envelope = client
                .recv()
                .await
                .expect("the real typed outgoing channel remains open");
            let (recipient, message) = match envelope {
                OutgoingEnvelope::ToConnection {
                    connection_id,
                    message,
                    ..
                } => (Some(connection_id), message),
                OutgoingEnvelope::Broadcast { message } => (None, message),
            };
            if let OutgoingMessage::AppServerNotification(envelope) = message {
                match envelope.notification {
                    ServerNotification::WorkflowUpdated(update) => {
                        assert_eq!(recipient, Some(connection));
                        assert_eq!(update.thread_id, root.to_string());
                        assert_ne!(update.workflow.id, foreign.id);
                        if update.workflow.status == ApiStatus::Succeeded {
                            assert_eq!(
                                update.workflow.result.as_ref(),
                                expected.get(&update.workflow.id)
                            );
                            snapshots.insert(update.workflow.id);
                        }
                    }
                    ServerNotification::WorkflowProgress(update) => {
                        assert_eq!(recipient, Some(connection));
                        assert_eq!(update.thread_id, root.to_string());
                        assert_ne!(update.workflow.id, foreign.id);
                        if update.workflow.status == ApiStatus::Succeeded {
                            progress.insert(update.workflow.id);
                        }
                    }
                    _ => {}
                }
            }
        }
        (snapshots, progress)
    })
    .await?;
    assert!(
        snapshots.contains(&earliest) && progress.contains(&earliest),
        "resync must restore a terminal report that is older than the bounded event window"
    );
    assert_eq!(snapshots, expected.keys().cloned().collect());
    assert_eq!(progress, expected.keys().cloned().collect());
    assert_eq!(
        test.thread_manager.list_thread_ids().await.len(),
        2,
        "resync must neither load nor create another runtime"
    );
    assert!(
        responses::received_responses_requests(&model)
            .await
            .is_empty()
    );
    states.remove_thread_state(root).await;
    skills.shutdown();
    test.codex.shutdown_and_wait().await?;
    other.thread.shutdown_and_wait().await?;
    Ok(())
}
