//! A single service observer drains durable presentation facts and recovers lag.

use super::host::AgentHost;
use super::lifecycle_delivery::LifecycleDelivery;
use crate::session::session::Session;
use codex_dynamic_workflows::WorkflowEngine;
use std::sync::Arc;

pub(super) fn start(
    owner: &Arc<Session>,
    engine: WorkflowEngine<AgentHost>,
    delivery: Arc<LifecycleDelivery>,
) -> Result<(), String> {
    let mut events = engine.subscribe_lifecycle();
    let mut state = owner
        .services
        .local_agent_runtime
        .execution_control
        .subscribe_state();
    let shutdown = owner.services.local_agent_runtime.shutdown.clone();
    let membership = owner
        .services
        .local_agent_runtime
        .track_tree_member()
        .map_err(|error| error.to_string())?;
    let owner = Arc::downgrade(owner);
    tokio::spawn(async move {
        let _membership = membership;
        let mut retry = None;
        let mut delay = std::time::Duration::from_millis(100);
        loop {
            let Some(owner) = owner.upgrade() else {
                return;
            };
            let run_id = tokio::select! {
                _ = shutdown.cancelled() => return,
                update = events.recv() => match update {
                    Ok(event) => Some(event.run_id),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                changed = state.changed() => {
                    if changed.is_err() { return; }
                    None
                },
                _ = async { match retry { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => None,
            };
            let full_replay = run_id.is_none();
            let result = match run_id {
                Some(run_id) => delivery.drain(&owner, &engine, &run_id).await,
                None => replay(&owner, &engine, &delivery).await,
            };
            if let Err(error) = result {
                tracing::warn!(%error, "workflow lifecycle delivery remains pending");
                retry = Some(tokio::time::Instant::now() + delay);
                delay = delay
                    .saturating_mul(2)
                    .min(std::time::Duration::from_secs(5));
            } else if full_replay {
                retry = None;
                delay = std::time::Duration::from_millis(100);
            }
        }
    });
    Ok(())
}

pub(super) async fn replay(
    owner: &Session,
    engine: &WorkflowEngine<AgentHost>,
    delivery: &LifecycleDelivery,
) -> Result<(), String> {
    let mut offset = 0;
    loop {
        let runs = engine.list(
            &owner.thread_id.to_string(),
            offset,
            codex_dynamic_workflows::MAX_PAGE_SIZE,
        )?;
        for run in &runs {
            delivery.drain(owner, engine, &run.id).await?;
        }
        if runs.len() < codex_dynamic_workflows::MAX_PAGE_SIZE {
            return Ok(());
        }
        offset = offset.saturating_add(runs.len());
    }
}
