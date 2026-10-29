//! Named child definitions are immutable snapshots in the owning run.
use super::Delegate;
use crate::WorkflowHost;
use crate::types::bounded_value;
use serde_json::Value;
use serde_json::json;

impl<H: WorkflowHost> Delegate<H> {
    pub(super) async fn nested(&self, input: Value) -> Result<Value, String> {
        if input.get("depth").and_then(Value::as_u64) != Some(1) {
            return Err("workflows may nest only one level".into());
        }
        let logical = input
            .get("logicalId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 2048)
            .ok_or("nested workflow requires a logical identity")?;
        if !self
            .seen_nested
            .lock()
            .map_err(|error| error.to_string())?
            .insert(logical.to_string())
        {
            return Err("duplicate nested workflow logical identity".into());
        }
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .ok_or("nested workflow requires a name")?;
        let cache_only = self.effect(
            &input,
            logical,
            crate::steering::WorkflowEffectKind::Nested {
                logical_id: logical.to_string(),
            },
        )?;
        let args = input.get("args").cloned().unwrap_or_else(|| json!({}));
        bounded_value(&args)?;
        let existing = self
            .store
            .get(&self.run_id)?
            .nested_workflows
            .get(logical)
            .cloned();
        let snapshot = if let Some(snapshot) = existing {
            if snapshot.name != name
                || snapshot.input != args
                || snapshot.source_hash != crate::orchestration::source_hash(&snapshot.source)
            {
                return Err("inconsistent replay arguments for nested workflow".into());
            }
            snapshot
        } else {
            if cache_only {
                return Err("revision prefix has no pinned nested workflow definition".into());
            }
            let resolved = self
                .host
                .load_workflow(self.run_id.clone(), name.to_string())
                .await?;
            let compiled = crate::compile_script(&resolved.source)?;
            let root = self.store.get(&self.run_id)?;
            if root.source_hash == crate::orchestration::source_hash(&resolved.source) {
                return Err("a workflow cannot invoke itself".into());
            }
            let snapshot = crate::NestedWorkflow {
                name: name.to_string(),
                resolved_id: resolved.id,
                source_hash: crate::orchestration::source_hash(&resolved.source),
                source: resolved.source,
                input: args,
                metadata: compiled.metadata,
            };
            self.store.update(&self.run_id, |run| {
                crate::orchestration::validate_effect_execution(
                    run,
                    &self.execution_id,
                    &self.revision_id,
                )?;
                if run.nested_workflows.len() >= 32 {
                    return Err("workflow exceeds 32 nested invocations".into());
                }
                for phase in &snapshot.metadata.phases {
                    let title = format!("{} / {}", snapshot.metadata.name, phase.title);
                    if !run
                        .phase_progress
                        .iter()
                        .any(|progress| progress.title == title)
                    {
                        run.phases.push(title.clone());
                        run.phase_progress.push(crate::PhaseProgress {
                            title,
                            status: crate::PhaseStatus::Pending,
                            steps_discovered: 0,
                            steps_completed: 0,
                            updated_at: None,
                            completed_at: None,
                            metrics: Default::default(),
                        });
                    }
                }
                run.nested_workflows
                    .insert(logical.to_string(), snapshot.clone());
                Ok(())
            })?;
            snapshot
        };
        let compiled = crate::compile_script(&snapshot.source)?;
        self.reply(json!({"body":compiled.body,"metadata":snapshot.metadata,"snapshotId":logical}))
    }
}
