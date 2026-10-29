//! Cumulative native observations share the existing contributor and COW checkpoint boundaries.
use super::Store;
use super::entity_metrics;
use super::metrics;
use crate::WorkflowStatus;

impl Store {
    pub(crate) fn record_model_turns(
        &self,
        id: &str,
        attempt: &str,
        thread: &str,
        count: Option<u64>,
    ) -> Result<(), String> {
        self.observe_model_turns(id, attempt, thread, count, /*incomplete*/ false)
    }

    pub(crate) fn mark_model_turns_incomplete(
        &self,
        id: &str,
        attempt: &str,
        thread: &str,
    ) -> Result<(), String> {
        self.observe_model_turns(id, attempt, thread, None, /*incomplete*/ true)
    }

    fn observe_model_turns(
        &self,
        id: &str,
        attempt: &str,
        thread: &str,
        count: Option<u64>,
        incomplete: bool,
    ) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let cached = self.cached(&mut state, id)?;
        if !cached.usage.contains_key(thread) && cached.usage.len() >= crate::MAX_OBSERVED_THREADS {
            // An omitted contributor makes the total unknown without dropping proven counters.
            let key = cached
                .usage
                .iter()
                .find(|(_, entry)| entry.attempt_id == attempt)
                .or_else(|| cached.usage.iter().next())
                .map(|(key, _)| key.clone());
            if let Some(entry) = key.and_then(|key| cached.usage.get_mut(&key)) {
                if entry.model_turns_incomplete {
                    return Ok(());
                }
                entry.model_turns_incomplete = true;
                return self.commit_progress(cached);
            }
            return Ok(());
        }
        let previous = cached.run.clone();
        let inserted = !cached.usage.contains_key(thread);
        metrics::observe(
            &cached.run,
            &mut cached.usage,
            attempt,
            thread,
            None,
            /*complete*/ false,
        )?;
        let observation = cached
            .usage
            .get_mut(thread)
            .ok_or("workflow count contributor missing")?;
        let before = (observation.model_turns, observation.model_turns_incomplete);
        if let Some(count) = count {
            observation.model_turns = Some(observation.model_turns.unwrap_or_default().max(count));
        }
        observation.model_turns_incomplete |= incomplete;
        if !inserted && before == (observation.model_turns, observation.model_turns_incomplete) {
            return Ok(());
        }
        let now = metrics::now_ms();
        metrics::refresh(&mut cached.run, &cached.usage, now);
        entity_metrics::stamp(
            &previous,
            &mut cached.run,
            now / 1000,
            entity_metrics::ChangeSource::Observation,
        );
        let terminal = !matches!(
            cached.run.status,
            WorkflowStatus::Pending | WorkflowStatus::Running
        );
        if terminal
            || incomplete
            || (inserted && count.is_none())
            || cached.last_checkpoint.elapsed() >= super::CHECKPOINT_INTERVAL
        {
            self.commit_progress(cached)?;
        } else if cached.last_event.elapsed() >= std::time::Duration::from_millis(100) {
            let _ = self
                .progress
                .send(super::summary(&cached.run, cached.run.steps.len() as u32));
            cached.last_event = std::time::Instant::now();
        }
        // The existing execution checkpoint and terminal barrier flush a quiet stream too.
        if terminal {
            state.active.remove(id);
        }
        Ok(())
    }

    pub(crate) fn record_attempt_clock(
        &self,
        id: &str,
        execution: &str,
        attempt_id: &str,
        elapsed_ms: u64,
        at_ms: i64,
    ) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let cached = self.cached(&mut state, id)?;
        if cached.run.execution_id != execution {
            return Ok(());
        }
        let previous = cached.run.clone();
        let Some(attempt) = cached
            .run
            .steps
            .values_mut()
            .flat_map(|step| &mut step.attempts)
            .find(|attempt| attempt.id == attempt_id)
        else {
            if cached.run.steering.known_attempt_ids.contains(attempt_id) {
                return Ok(());
            }
            return Err("workflow attempt clock identity missing".into());
        };
        if attempt.completed_at.is_some() && attempt.metrics.active_duration_ms.is_some() {
            return Ok(());
        }
        let first = attempt.clock_execution_id.is_none();
        if attempt.clock_execution_id.as_deref() != Some(execution) {
            attempt.activity.execution_duration_ms = 0;
            attempt.clock_execution_id = Some(execution.to_string());
        }
        // A newly observed suffix cannot prove the time spent by an old uninstrumented attempt.
        if first
            && attempt.execution_id != execution
            && attempt.metrics.active_duration_ms.is_none()
        {
            attempt.clock_incomplete = true;
        }
        let delta = elapsed_ms.saturating_sub(attempt.activity.execution_duration_ms);
        attempt.activity.active_duration_ms =
            attempt.activity.active_duration_ms.saturating_add(delta);
        attempt.activity.execution_duration_ms =
            attempt.activity.execution_duration_ms.max(elapsed_ms);
        attempt.activity.checkpoint_at_ms = Some(at_ms);
        attempt.metrics.active_duration_ms =
            (!attempt.clock_incomplete).then_some(attempt.activity.active_duration_ms);
        let terminal = attempt.completed_at.is_some();
        metrics::refresh(&mut cached.run, &cached.usage, at_ms);
        entity_metrics::stamp(
            &previous,
            &mut cached.run,
            at_ms / 1000,
            entity_metrics::ChangeSource::Observation,
        );
        if terminal || cached.last_checkpoint.elapsed() >= super::CHECKPOINT_INTERVAL {
            self.commit_progress(cached)?;
        }
        Ok(())
    }
}
