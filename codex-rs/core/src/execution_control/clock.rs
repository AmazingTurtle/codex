//! Monotonic gate accounting remains exact when state-watch notifications coalesce.

use codex_protocol::protocol::ExecutionControlState;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;
use std::time::Instant;

pub(super) struct ExecutionClock(Mutex<ClockState>);

struct ClockState {
    elapsed: Duration,
    running_since: Option<Instant>,
}

pub(crate) struct ExecutionClockSnapshot {
    pub active_duration_ms: u64,
    pub at_ms: i64,
}

impl Default for ExecutionClock {
    fn default() -> Self {
        Self(Mutex::new(ClockState {
            elapsed: Duration::ZERO,
            running_since: Some(Instant::now()),
        }))
    }
}

impl ExecutionClock {
    pub(super) fn transition(&self, state: ExecutionControlState) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .transition(state, Instant::now());
    }

    pub(super) fn snapshot(&self) -> ExecutionClockSnapshot {
        let clock = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        ExecutionClockSnapshot {
            active_duration_ms: u64::try_from(clock.elapsed(Instant::now()).as_millis())
                .unwrap_or(u64::MAX),
            at_ms: chrono::Utc::now().timestamp_millis(),
        }
    }
}

impl ClockState {
    fn transition(&mut self, state: ExecutionControlState, now: Instant) {
        match (state == ExecutionControlState::Running, self.running_since) {
            (true, None) => self.running_since = Some(now),
            (false, Some(start)) => {
                self.elapsed = self
                    .elapsed
                    .saturating_add(now.saturating_duration_since(start));
                self.running_since = None;
            }
            (true, Some(_)) | (false, None) => {}
        }
    }

    fn elapsed(&self, now: Instant) -> Duration {
        self.elapsed.saturating_add(
            self.running_since
                .map_or(Duration::ZERO, |start| now.saturating_duration_since(start)),
        )
    }
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
