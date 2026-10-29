//! Pause-aware status time owned independently of the optional status row.
//! A phase can override the displayed clock without changing turn accounting.

use std::time::Duration;
use std::time::Instant;

#[derive(Debug)]
pub(crate) struct StatusTimer {
    /// Optional wall-clock origin for the displayed phase, without resetting turn accounting.
    pub(crate) display_started_at: Option<Instant>,
    pub(super) elapsed_running: Duration,
    pub(super) last_resume_at: Instant,
    pub(super) is_paused: bool,
    pub(super) modal_paused: bool,
    pub(super) execution_paused: bool,
    pub(super) paused_at: Option<Instant>,
}

impl Default for StatusTimer {
    fn default() -> Self {
        Self {
            display_started_at: None,
            elapsed_running: Duration::ZERO,
            last_resume_at: Instant::now(),
            is_paused: false,
            modal_paused: false,
            execution_paused: false,
            paused_at: None,
        }
    }
}

impl StatusTimer {
    pub(crate) fn reset(&mut self, elapsed: Duration) {
        self.elapsed_running = elapsed;
        self.last_resume_at = Instant::now();
        // A turn can start while an MCP elicitation or approval is still open.
        // Reset elapsed time without changing that modal's pause state.
    }

    pub(crate) fn pause_at(&mut self, now: Instant) {
        self.modal_paused = true;
        self.sync_pause_at(now);
    }

    pub(crate) fn resume_at(&mut self, now: Instant) {
        self.modal_paused = false;
        self.sync_pause_at(now);
    }

    pub(crate) fn set_execution_paused(&mut self, paused: bool, now: Instant) {
        self.execution_paused = paused;
        self.sync_pause_at(now);
    }

    fn sync_pause_at(&mut self, now: Instant) {
        let paused = self.modal_paused || self.execution_paused;
        if paused && !self.is_paused {
            self.elapsed_running += now.saturating_duration_since(self.last_resume_at);
            self.is_paused = true;
            self.paused_at = Some(now);
        } else if !paused && self.is_paused {
            if let (Some(started), Some(paused_at)) = (self.display_started_at, self.paused_at) {
                self.display_started_at =
                    Some(started + now.saturating_duration_since(paused_at.max(started)));
            }
            self.last_resume_at = now;
            self.is_paused = false;
            self.paused_at = None;
        }
    }

    pub(crate) fn displayed_elapsed_at(&self, now: Instant) -> Duration {
        self.display_started_at.map_or_else(
            || self.elapsed_at(now),
            |started| {
                self.paused_at
                    .unwrap_or(now)
                    .saturating_duration_since(started)
            },
        )
    }

    pub(crate) fn elapsed_at(&self, now: Instant) -> Duration {
        self.elapsed_running
            + if self.is_paused {
                Duration::ZERO
            } else {
                now.saturating_duration_since(self.last_resume_at)
            }
    }
}

#[cfg(test)]
#[path = "timer_tests.rs"]
mod tests;
