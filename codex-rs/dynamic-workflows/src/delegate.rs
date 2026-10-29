//! Durable private-bootstrap operations. Public scripts never receive this delegate.
mod agent;
mod agent_plan;
mod effects;
mod nested;
mod observations;
use crate::WorkflowHost;
use crate::WorkflowStatus;
use crate::store::Store;
use crate::types::bounded_value;
use codex_code_mode_protocol::CellId;
use codex_code_mode_protocol::CodeModeNestedToolCall;
use codex_code_mode_protocol::CodeModeSessionDelegate;
use codex_code_mode_protocol::NotificationFuture;
use codex_code_mode_protocol::ToolInvocationFuture;
use codex_protocol::ToolName;
use serde_json::Value;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;

pub(crate) struct Delegate<H> {
    pub(crate) store: Arc<Store>,
    pub(crate) host: Arc<H>,
    pub(crate) run_id: String,
    pub(crate) seen: Arc<Mutex<HashSet<String>>>,
    pub(crate) seen_nested: Arc<Mutex<HashSet<String>>>,
    pub(crate) pending: Arc<AtomicUsize>,
    pub(crate) retry_recovered_failure: bool,
    pub(crate) seen_effects: Arc<Mutex<HashSet<String>>>,
    pub(crate) checkpoint_index: Arc<Mutex<u64>>,
    pub(crate) execution_id: String,
    pub(crate) revision_id: String,
    pub(crate) supervision: Arc<crate::supervision::AgentSupervision>,
    pub(crate) cancellation: CancellationToken,
}

struct PendingCall<'a>(&'a AtomicUsize);
impl Drop for PendingCall<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl<H: WorkflowHost> Delegate<H> {
    fn owned(&self) -> Self {
        Self {
            store: self.store.clone(),
            host: self.host.clone(),
            run_id: self.run_id.clone(),
            seen: self.seen.clone(),
            seen_nested: self.seen_nested.clone(),
            pending: self.pending.clone(),
            retry_recovered_failure: self.retry_recovered_failure,
            seen_effects: self.seen_effects.clone(),
            checkpoint_index: self.checkpoint_index.clone(),
            execution_id: self.execution_id.clone(),
            revision_id: self.revision_id.clone(),
            supervision: self.supervision.clone(),
            cancellation: self.cancellation.clone(),
        }
    }
    async fn invoke(&self, input: Value) -> Result<Value, String> {
        bounded_value(&input)?;
        match input.get("operation").and_then(Value::as_str) {
            Some("checkpoint") => self.checkpoint(&input),
            Some("agent") => self.agent(input).await,
            Some("loadWorkflow") => self.nested(input).await,
            Some("budgetObservations") => self.observe_budget(input),
            Some("phase") => {
                let name = input
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("phase requires a title")?;
                if let Some(id) = input.get("effectId").and_then(Value::as_str) {
                    self.effect(
                        &input,
                        id,
                        crate::steering::WorkflowEffectKind::Phase {
                            title: name.to_string(),
                        },
                    )?;
                }
                self.store.update(&self.run_id, |run| {
                    crate::orchestration::validate_effect_execution(
                        run,
                        &self.execution_id,
                        &self.revision_id,
                    )?;
                    if run.status != WorkflowStatus::Running {
                        return Err("workflow is stopped".into());
                    }
                    let progress = run
                        .phase_progress
                        .iter_mut()
                        .find(|phase| phase.title == name)
                        .ok_or("workflow phase was not declared in metadata")?;
                    progress.status = crate::PhaseStatus::Active;
                    run.phase = Some(name.to_string());
                    crate::orchestration::refresh_phases(run);
                    Ok(json!({"budget":crate::orchestration::budget(run)}))
                })
            }
            Some("log") => self.log(&input),
            Some("publish") => {
                let value = input
                    .get("value")
                    .ok_or("workflow return must be JSON")?
                    .clone();
                bounded_value(&value)?;
                self.store.update(&self.run_id, |run| {
                    if run.status != WorkflowStatus::Running {
                        return Err("workflow is stopped".into());
                    }
                    crate::orchestration::validate_effect_execution(
                        run,
                        &self.execution_id,
                        &self.revision_id,
                    )?;
                    if run.steering.version == 1 && !run.steering.suffix_open {
                        return Err("workflow prefix has not authorized publication".into());
                    }
                    run.result = Some(value);
                    Ok(json!({"budget":crate::orchestration::budget(run)}))
                })
            }
            _ => Err("workflow operation is unavailable".into()),
        }
    }

    fn reply(&self, mut value: Value) -> Result<Value, String> {
        value["budget"] = crate::orchestration::budget(&self.store.get(&self.run_id)?);
        Ok(value)
    }
}

impl<H: WorkflowHost> CodeModeSessionDelegate for Delegate<H> {
    fn invoke_tool<'a>(
        &'a self,
        invocation: CodeModeNestedToolCall,
        cancellation: CancellationToken,
    ) -> ToolInvocationFuture<'a> {
        Box::pin(async move {
            if invocation.tool_name != ToolName::plain("workflow_host") {
                return Err("only workflow operations are available".into());
            }
            self.pending.fetch_add(1, Ordering::SeqCst);
            let _pending = PendingCall(&self.pending);
            if invocation
                .input
                .as_ref()
                .and_then(|input| input.get("operation"))
                .and_then(Value::as_str)
                == Some("agent")
            {
                let delegate = self.owned();
                let input = invocation.input.ok_or("workflow input missing")?;
                let receiver = self.supervision.spawn(async move {
                    let result = tokio::select! {biased; _=delegate.cancellation.cancelled()=>Err("workflow stopped while observing its native attempt".into()),result=delegate.agent(input)=>result};
                    if let Err(error) = &result {
                        delegate.store.update(&delegate.run_id, |run| {
                            crate::orchestration::validate_effect_execution(run,&delegate.execution_id,&delegate.revision_id)?;
                            run.error=Some(error.chars().take(2048).collect());
                            Ok(())
                        })?;
                    }
                    result
                })?;
                return tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => Err("workflow operation interrupted".into()),
                    result = receiver => result.map_err(|error| error.to_string())?,
                };
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => Err("workflow operation interrupted".into()),
                result = self.invoke(invocation.input.ok_or("workflow input missing")?) => {
                    if let Err(error) = &result { self.store.update(&self.run_id, |run| { crate::orchestration::validate_effect_execution(run,&self.execution_id,&self.revision_id)?;run.error = Some(error.chars().take(/*n*/ 2048).collect()); Ok(()) })?; }
                    result
                },
            }
        })
    }
    fn notify<'a>(
        &'a self,
        _call_id: String,
        _cell_id: CellId,
        _text: String,
        _cancellation: CancellationToken,
    ) -> NotificationFuture<'a> {
        Box::pin(async { Err("workflow notifications are unavailable".into()) })
    }
    fn cell_closed(&self, _cell_id: &CellId) {}
}
