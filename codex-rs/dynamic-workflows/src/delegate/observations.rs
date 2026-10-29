//! Script-visible budget reads replay separately from live admission accounting.
use super::Delegate;
use crate::WorkflowHost;
use crate::WorkflowStatus;
use serde_json::Value;
use serde_json::json;

impl<H: WorkflowHost> Delegate<H> {
    pub(super) fn observe_budget(&self, input: Value) -> Result<Value, String> {
        let observations: std::collections::BTreeMap<String, crate::BudgetObservation> =
            serde_json::from_value(
                input
                    .get("observations")
                    .cloned()
                    .ok_or("budget observations missing")?,
            )
            .map_err(|error| error.to_string())?;
        if observations.len() > 64 {
            return Err("workflow budget observation batch exceeds 64 reads".into());
        }
        for key in observations.keys() {
            let owner = input
                .get("attribution")
                .and_then(|owners| owners.get(key))
                .cloned()
                .unwrap_or(Value::Null);
            self.effect(
                &owner,
                key,
                crate::steering::WorkflowEffectKind::Budget {
                    logical_id: key.clone(),
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
            for (key, observation) in observations {
                if key.is_empty() || key.len() > 2048 {
                    return Err("invalid workflow budget observation identity".into());
                }
                if let Some(existing) = run.budget_observations.get(&key) {
                    if existing != &observation {
                        return Err("inconsistent replay budget observation".into());
                    }
                } else {
                    if observation.total != run.limits.max_tokens
                        || observation.spent
                            > run
                                .metrics
                                .usage
                                .as_ref()
                                .map_or(0, |usage| usage.total_tokens)
                    {
                        return Err(
                            "workflow budget observation is not an observed native value".into(),
                        );
                    }
                    if run.budget_observations.len() >= crate::MAX_BUDGET_OBSERVATIONS {
                        return Err("workflow budget observation limit exhausted".into());
                    }
                    run.budget_observations.insert(key, observation);
                }
            }
            if serde_json::to_vec(&run.budget_observations)
                .map_err(|error| error.to_string())?
                .len()
                > crate::MAX_BUDGET_OBSERVATION_BYTES
            {
                return Err("workflow budget observation journal exceeds 256KiB".into());
            }
            Ok(json!({"budget":crate::orchestration::budget(run)}))
        })
    }
}
