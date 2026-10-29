//! Durable agent admission, replay and structured-output repair.
use super::Delegate;
use crate::AdmissionStatus;
use crate::AgentStartError;
use crate::AgentStartMode;
use crate::WorkflowHost;
use crate::WorkflowStatus;
use crate::types::bounded_value;
use serde_json::Value;
use serde_json::json;

impl<H: WorkflowHost> Delegate<H> {
    pub(super) async fn agent(&self, input: Value) -> Result<Value, String> {
        let logical_id = input
            .get("logicalId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 2048)
            .ok_or("agent requires an automatic logical identity")?
            .to_string();
        let step_id = blake3::hash(logical_id.as_bytes()).to_hex().to_string();
        if !self
            .seen
            .lock()
            .map_err(|error| error.to_string())?
            .insert(step_id.clone())
        {
            return Err("duplicate workflow logical identity".into());
        }
        let prompt = input
            .get("prompt")
            .and_then(Value::as_str)
            .filter(|prompt| !prompt.trim().is_empty())
            .ok_or("agent requires a prompt")?;
        let mut options = input
            .get("options")
            .and_then(Value::as_object)
            .cloned()
            .ok_or("agent options must be an object")?;
        for key in options.keys() {
            if ![
                "label",
                "phase",
                "schema",
                "model",
                "reasoningEffort",
                "agentType",
                "isolation",
                "key",
            ]
            .contains(&key.as_str())
            {
                return Err(format!("unsupported workflow agent option: {key}"));
            }
        }
        for key in [
            "label",
            "phase",
            "model",
            "reasoningEffort",
            "agentType",
            "isolation",
            "key",
        ] {
            if options.get(key).is_some_and(|value| !value.is_string()) {
                return Err(format!("workflow agent {key} must be a string"));
            }
        }
        if options
            .get("isolation")
            .is_some_and(|value| value != "worktree")
        {
            return Err("workflow isolation must be worktree".into());
        }
        let label = options
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                prompt
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or(prompt)
                    .chars()
                    .take(/*n*/ 80)
                    .collect()
            });
        if label.trim().is_empty() || label.len() > 320 {
            return Err("workflow agent label must contain 1..=320 bytes".into());
        }
        let phase = input
            .get("phase")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let schema = options.get("schema").cloned();
        if let Some(schema) = &schema {
            crate::validate_schema(schema)?;
        }
        options.insert("message".into(), Value::String(prompt.to_string()));
        let key = options
            .get("key")
            .and_then(Value::as_str)
            .map(str::to_string);
        let scope = input
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if let Some(key) = &key {
            if key.is_empty() || key.len() > 128 || scope.is_empty() {
                return Err("workflow key requires a full scope and 1..=128 UTF-8 bytes".into());
            }
            if logical_id != crate::steering::logical_key(&scope, key) {
                return Err("workflow scoped key does not match its logical identity".into());
            }
        }
        let options = Value::Object(options);
        bounded_value(&options)?;
        let cache_only = self.effect(
            &input,
            &logical_id,
            crate::steering::WorkflowEffectKind::Agent {
                step_id: step_id.clone(),
            },
        )?;
        'attempts: loop {
            let plan = self.store.update(&self.run_id, |run| {
                let plan = super::agent_plan::plan(
                    run,
                    super::agent_plan::AgentCall {
                        step_id: &step_id,
                        logical_id: &logical_id,
                        scope: &scope,
                        key: &key,
                        label: &label,
                        phase: &phase,
                        options: &options,
                        schema: schema.as_ref(),
                        cache_only,
                        execution_id: &self.execution_id,
                        revision_id: &self.revision_id,
                    },
                )?;
                crate::orchestration::refresh_phases(run);
                Ok(plan)
            })?;
            let (request, mode) = match plan {
                super::agent_plan::AgentPlan::Cached(result) => {
                    return self.reply(json!({"ok":true,"result":result}));
                }
                super::agent_plan::AgentPlan::Dispatch(request, mode) => (*request, mode),
            };
            let was_recovery = matches!(&mode, AgentStartMode::Recover { .. });
            let child = match self.host.schedule_agent(request.clone(), mode).await {
                Ok(child) => child,
                Err(error) => {
                    let mut controls = self.store.events.subscribe();
                    let rejected = matches!(error, AgentStartError::NotStarted(_));
                    self.store.update(&self.run_id, |run| {
                        let attempt = run
                            .steps
                            .get_mut(&step_id)
                            .and_then(|step| step.attempts.last_mut())
                            .ok_or("workflow attempt missing")?;
                        let known_rejected =
                            rejected || attempt.admission == AdmissionStatus::Rejected;
                        attempt.status = if known_rejected {
                            WorkflowStatus::Failed
                        } else {
                            WorkflowStatus::Interrupted
                        };
                        if !matches!(
                            attempt.admission,
                            AdmissionStatus::Accepted | AdmissionStatus::Rejected
                        ) {
                            attempt.admission = if rejected {
                                AdmissionStatus::Rejected
                            } else {
                                AdmissionStatus::Uncertain
                            };
                        }
                        if !known_rejected || attempt.error.is_none() {
                            attempt.error = Some(error.to_string());
                        }
                        Ok(())
                    })?;
                    loop {
                        let run = self.store.get(&self.run_id)?;
                        let command=run.steering.commands.iter().rev().find(|command| command.revision_id==self.revision_id
                            && matches!(&command.target,crate::WorkflowControlTarget::Step{logical_id:target,expected_attempt_id:Some(attempt),..} if target==&logical_id && attempt==&request.attempt_id)
                            && matches!(command.operation,crate::WorkflowControlOperation::EditQueued{..}|crate::WorkflowControlOperation::Skip{..}));
                        match command.map(|command| command.status) {
                            Some(crate::WorkflowControlStatus::Pending) => {
                                match controls.recv().await {
                                    Ok(_)
                                    | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                        continue;
                                    }
                                    Err(error) => return Err(error.to_string()),
                                }
                            }
                            Some(crate::WorkflowControlStatus::Applied)
                                if run.steps.get(&step_id).is_some_and(|step| {
                                    step.disposition == crate::WorkflowStepDisposition::Skipped
                                        || step.attempts.last().is_some_and(|attempt| {
                                            attempt.admission == AdmissionStatus::Rejected
                                        })
                                }) =>
                            {
                                continue 'attempts;
                            }
                            _ => break,
                        }
                    }
                    return Err(error.to_string());
                }
            };
            self.store.update(&self.run_id, |run| {
                crate::orchestration::record_acceptance(run, &request.attempt_id, child.clone())
            })?;
            let mut events = self.store.events.subscribe();
            let outcome = {
                let waiting = self.host.wait_agent(request.clone(), child);
                tokio::pin!(waiting);
                loop {
                    let snapshot = self.store.get(&self.run_id)?;
                    let step = snapshot
                        .steps
                        .get(&step_id)
                        .ok_or("workflow controlled step missing")?;
                    if step.disposition == crate::WorkflowStepDisposition::Skipped {
                        return self.reply(
                            json!({"ok":true,"result":step.result.clone().unwrap_or(Value::Null)}),
                        );
                    }
                    if let Some(attempt) = step.attempts.last().filter(|attempt| {
                        attempt.id == request.attempt_id && attempt.status == WorkflowStatus::Failed
                    }) {
                        break Ok(crate::AgentOutcome {
                            result: Value::Null,
                            usage: attempt.usage.clone().unwrap_or(Value::Null),
                            error: attempt.error.clone(),
                        });
                    }
                    tokio::select! {
                        result=&mut waiting=>break result,
                        event=events.recv()=>match event {Ok(_)|Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>{},Err(error)=>break Err(error.to_string())},
                    }
                }
            };
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.store.update(&self.run_id, |run| {
                        let attempt = run
                            .steps
                            .get_mut(&step_id)
                            .and_then(|step| step.attempts.last_mut())
                            .ok_or("workflow attempt missing")?;
                        attempt.status = WorkflowStatus::Interrupted;
                        attempt.error = Some(error.clone());
                        Ok(())
                    })?;
                    return Err(error);
                }
            };
            bounded_value(&outcome.result)?;
            bounded_value(&outcome.usage)?;
            let schema_error = if outcome.error.is_none() {
                schema.as_ref().and_then(|schema| {
                    crate::schema::validate_output(schema, &outcome.result).err()
                })
            } else {
                None
            };
            let failure = outcome.error.clone().or_else(|| schema_error.clone());
            self.store.update(&self.run_id, |run| {
                let step = run.steps.get_mut(&step_id).ok_or("workflow step missing")?;
                if step.disposition == crate::WorkflowStepDisposition::Skipped {
                    return Ok(());
                }
                let attempt = step.attempts.last_mut().ok_or("workflow attempt missing")?;
                attempt.status = if failure.is_some() {
                    WorkflowStatus::Failed
                } else {
                    WorkflowStatus::Succeeded
                };
                attempt.usage = Some(outcome.usage);
                attempt.error = failure.clone();
                attempt.schema_error = schema_error.clone();
                if schema_error.is_some() {
                    step.schema_attempts += 1;
                }
                if failure.is_none() {
                    step.result = Some(outcome.result.clone());
                }
                crate::orchestration::refresh_phases(run);
                Ok(())
            })?;
            if let Some(step) = self
                .store
                .get(&self.run_id)?
                .steps
                .get(&step_id)
                .filter(|step| step.disposition == crate::WorkflowStepDisposition::Skipped)
            {
                return self
                    .reply(json!({"ok":true,"result":step.result.clone().unwrap_or(Value::Null)}));
            }
            if let Some(error) = schema_error {
                if self
                    .store
                    .get(&self.run_id)?
                    .steps
                    .get(&step_id)
                    .is_some_and(|step| step.schema_attempts >= 5)
                {
                    return Err(format!(
                        "structured output failed after five attempts: {error}"
                    ));
                }
                continue;
            }
            if let Some(error) = failure {
                if was_recovery && self.retry_recovered_failure {
                    continue;
                }
                return self.reply(json!({"ok":false,"kind":"agentFailure","error":error}));
            }
            return self.reply(json!({"ok":true,"result":outcome.result}));
        }
    }
}
