//! Per-attempt observations of the existing conversation gate's monotonic clock.

use codex_dynamic_workflows::WorkflowHostEvent;
use codex_dynamic_workflows::WorkflowObserver;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::PoisonError;

#[derive(Default)]
pub(in crate::workflows) struct AttemptClocks(Mutex<HashMap<String, Clock>>);

struct Clock {
    baseline: u64,
    final_elapsed: Option<u64>,
}

#[cfg(test)]
#[path = "attempt_clock_tests.rs"]
mod tests;

impl AttemptClocks {
    pub(in crate::workflows) fn begin_observation(&self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    pub(in crate::workflows) fn resume_active(&self, attempt: &str, active_ms: u64) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(attempt.into())
            .or_insert(Clock {
                baseline: active_ms,
                final_elapsed: None,
            });
    }

    pub(in crate::workflows) fn admitted(&self, attempt: &str, active_ms: u64) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                attempt.into(),
                Clock {
                    baseline: active_ms,
                    final_elapsed: None,
                },
            );
    }

    pub(in crate::workflows) fn publish(
        &self,
        observer: &WorkflowObserver,
        attempt: &str,
        active_ms: u64,
        complete: bool,
    ) -> Result<(), String> {
        let elapsed = {
            let mut clocks = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(clock) = clocks.get_mut(attempt) else {
                // Recovery has no live acceptance baseline. Keep persisted time,
                // rather than counting offline time or inventing an initial zero.
                return Ok(());
            };
            let elapsed = clock
                .final_elapsed
                .unwrap_or_else(|| active_ms.saturating_sub(clock.baseline));
            if complete {
                clock.final_elapsed = Some(elapsed);
            }
            elapsed
        };
        observer(WorkflowHostEvent::AttemptClock {
            attempt_id: attempt.into(),
            active_duration_ms: elapsed,
            at_ms: chrono::Utc::now().timestamp_millis(),
        })
    }
}
