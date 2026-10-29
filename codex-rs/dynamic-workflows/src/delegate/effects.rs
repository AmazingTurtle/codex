//! Only the private compiler boundary can authorize dispatch beyond a replay prefix.

use serde_json::Value;
use serde_json::json;

use super::Delegate;
use crate::WorkflowHost;
use crate::WorkflowRevisionStatus;
use crate::WorkflowStatus;
use crate::steering::WorkflowEffectKind;
use crate::steering::WorkflowEffectReceipt;

impl<H: WorkflowHost> Delegate<H> {
    pub(super) fn log(&self, input: &Value) -> Result<Value, String> {
        let message = input
            .get("message")
            .and_then(Value::as_str)
            .ok_or("log requires a string")?;
        if message.len() > 1024 {
            return Err("workflow log message exceeds 1024 bytes".into());
        }
        let phase = input
            .get("phase")
            .and_then(Value::as_str)
            .map(str::to_string);
        let id = input.get("effectId").and_then(Value::as_str);
        let response = self.store.update(&self.run_id, |run| {
            crate::orchestration::validate_effect_execution(
                run,
                &self.execution_id,
                &self.revision_id,
            )?;
            let mut sequence = run.logs.len() as u64;
            let mut append = true;
            if run.steering.version == 1 {
                let id = id
                    .filter(|id| !id.is_empty() && id.len() <= 2048)
                    .ok_or("workflow log has no bounded effect identity")?;
                if let Some(effect) = run.steering.effects.get(id) {
                    sequence = match &effect.kind {
                        WorkflowEffectKind::Log { sequence, .. } => *sequence,
                        _ => return Err("workflow log effect identity changed".into()),
                    };
                }
                let scope = input
                    .get("scope")
                    .and_then(Value::as_str)
                    .filter(|scope| !scope.is_empty() && scope.len() <= 2048)
                    .ok_or("workflow log has no bounded scope")?;
                let statement_index = input
                    .get("statementIndex")
                    .and_then(Value::as_u64)
                    .ok_or("workflow log has no compiler provenance")?
                    .try_into()
                    .map_err(|_| "invalid workflow log statement index")?;
                let receipt = WorkflowEffectReceipt {
                    id: id.to_string(),
                    scope: scope.to_string(),
                    statement_index,
                    kind: WorkflowEffectKind::Log {
                        sequence,
                        message: message.to_string(),
                        phase: phase.clone(),
                    },
                };
                if let Some(existing) = run.steering.effects.get(id) {
                    if existing != &receipt {
                        return Err("inconsistent replay of a workflow log".into());
                    }
                    if let Some(log) = run.logs.get(sequence as usize) {
                        if log.sequence != sequence || log.message != message || log.phase != phase
                        {
                            return Err("recorded workflow log differs from its receipt".into());
                        }
                        append = false;
                    } else if !run.steering.suffix_open || sequence != run.logs.len() as u64 {
                        return Err(
                            "recorded workflow log is missing; prefix proof cannot proceed".into(),
                        );
                    }
                } else {
                    if !run.steering.suffix_open {
                        return Err("revision prefix attempted an unrecorded log".into());
                    }
                    if run.steering.effects.len() >= 8192 {
                        return Err("workflow effect receipt limit exhausted (8192)".into());
                    }
                    run.steering.effects.insert(id.to_string(), receipt);
                }
            }
            if append {
                if run.logs.len() >= crate::MAX_LOG_ENTRIES
                    || run.logs.iter().map(|log| log.message.len()).sum::<usize>() + message.len()
                        > 16 * 1024
                {
                    return Err("workflow log limit exhausted".into());
                }
                run.logs.push(crate::WorkflowLog {
                    sequence,
                    message: message.to_string(),
                    phase: phase.clone(),
                    created_at: crate::types::timestamp(),
                });
            }
            Ok(json!({"budget":crate::orchestration::budget(run)}))
        })?;
        if let Some(id) = id
            && !self
                .seen_effects
                .lock()
                .map_err(|error| error.to_string())?
                .insert(id.to_string())
        {
            return Err("duplicate workflow effect identity".into());
        }
        Ok(response)
    }
    pub(super) fn effect(
        &self,
        input: &Value,
        id: &str,
        kind: WorkflowEffectKind,
    ) -> Result<bool, String> {
        let run = self.store.get(&self.run_id)?;
        crate::orchestration::validate_effect_execution(
            &run,
            &self.execution_id,
            &self.revision_id,
        )?;
        if run.steering.version != 1 {
            return Ok(false);
        }
        if run.execution_id != self.execution_id
            || run.steering.state.active_revision_id != self.revision_id
            || run.steering.state.status != WorkflowRevisionStatus::Active
        {
            return Err("workflow revision is fenced against effects".into());
        }
        let scope = input
            .get("scope")
            .and_then(Value::as_str)
            .filter(|scope| !scope.is_empty() && scope.len() <= 2048)
            .ok_or("workflow effect has no bounded scope")?;
        let statement_index: u32 = input
            .get("statementIndex")
            .and_then(Value::as_u64)
            .ok_or("workflow effect has no compiler provenance")?
            .try_into()
            .map_err(|_| "workflow statement index is invalid")?;
        let receipt = WorkflowEffectReceipt {
            id: id.to_string(),
            scope: scope.to_string(),
            statement_index,
            kind,
        };
        let cache_only = !run.steering.suffix_open;
        self.store.update(&self.run_id, |run| {
            crate::orchestration::validate_effect_execution(
                run,
                &self.execution_id,
                &self.revision_id,
            )?;
            if run.execution_id != self.execution_id
                || run.steering.state.status != WorkflowRevisionStatus::Active
            {
                return Err("workflow revision is fenced against effects".into());
            }
            if let Some(existing) = run.steering.effects.get(id) {
                if existing != &receipt {
                    return Err("inconsistent replay of a workflow effect".into());
                }
            } else {
                if cache_only {
                    return Err("revision prefix attempted an unrecorded effect".into());
                }
                if run.steering.effects.len() >= 8192 {
                    return Err("workflow effect receipt limit exhausted (8192)".into());
                }
                run.steering.effects.insert(id.to_string(), receipt);
            }
            Ok(())
        })?;
        if !self
            .seen_effects
            .lock()
            .map_err(|error| error.to_string())?
            .insert(id.to_string())
        {
            return Err("duplicate workflow effect identity".into());
        }
        Ok(cache_only)
    }

    pub(super) fn checkpoint(&self, input: &Value) -> Result<Value, String> {
        let index = input
            .get("statementIndex")
            .and_then(Value::as_u64)
            .ok_or("workflow checkpoint index missing")?;
        if input.get("scope").and_then(Value::as_str) != Some("root") {
            return Err("workflow checkpoint must belong to the private root program".into());
        }
        let complete = input
            .get("complete")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let pending = input
            .get("pendingWork")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let seen = self
            .seen_effects
            .lock()
            .map_err(|error| error.to_string())?;
        self.store.update(&self.run_id, |run| {
            if run.status != WorkflowStatus::Running
                || run.execution_id != self.execution_id
                || run.steering.state.active_revision_id != self.revision_id
                || run.steering.state.status != WorkflowRevisionStatus::Active
            {
                return Err("workflow revision is fenced at its compiler boundary".into());
            }
            let frontier = u64::from(run.steering.replay_frontier);
            if !run.steering.suffix_open {
                let mut expected = self
                    .checkpoint_index
                    .lock()
                    .map_err(|error| error.to_string())?;
                if index != *expected || index > frontier {
                    return Err("workflow private checkpoint sequence is invalid".into());
                }
                if index == frontier {
                    if pending != 0 {
                        return Err("revision prefix still has unfinished operations".into());
                    }
                    if run.steering.effects.values().any(|effect| {
                        effect.statement_index < run.steering.replay_frontier
                            && !seen.contains(&effect.id)
                    }) {
                        return Err("revision prefix omitted a recorded workflow effect".into());
                    }
                    run.steering.suffix_open = true;
                } else if complete {
                    return Err("revision returned before proving its reusable prefix".into());
                }
                *expected = expected
                    .checked_add(1)
                    .ok_or("workflow checkpoint sequence exhausted")?;
            } else if !complete {
                return Err("unexpected private checkpoint after suffix admission".into());
            }
            Ok(json!({"budget":crate::orchestration::budget(run)}))
        })
    }
}

#[cfg(test)]
#[path = "effects_tests.rs"]
mod tests;
