//! Live wake registrations. These describe execution; they never schedule it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::watch;

/// Current runtime waits, independent of whether the thread accepts user input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WakeState {
    pub pending_count: u32,
    pub waiting: bool,
}

#[derive(Default)]
struct State {
    turns: usize,
    sampling: usize,
    generation: u64,
    commands: HashMap<String, u64>,
    tools: HashMap<String, Option<String>>,
    waits: HashMap<String, Wait>,
}

struct Wait {
    cell: Option<String>,
    wrapper: Option<CellWait>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CellWait {
    Execution,
    ExplicitWait,
}

impl State {
    fn snapshot(&self) -> WakeState {
        let waits = self
            .waits
            .values()
            .filter(|wait| {
                // A yielded code cell needs an explicit exec/wait consumer. Its
                // detached callbacks cannot by themselves wake an idle model.
                self.turns > 0
                    && if let Some(kind) = wait.wrapper {
                        kind == CellWait::ExplicitWait
                            && !self.waits.values().any(|other| {
                                other.wrapper.is_none()
                                    && other.cell.is_some()
                                    && other.cell == wait.cell
                            })
                    } else {
                        wait.cell.is_none()
                            || self
                                .waits
                                .values()
                                .any(|other| other.wrapper.is_some() && other.cell == wait.cell)
                    }
            })
            .count();
        let working = self.tools.keys().any(|id| match self.waits.get(id) {
            None => true,
            Some(wait) if wait.wrapper == Some(CellWait::Execution) => !self
                .waits
                .values()
                .any(|other| other.wrapper.is_none() && other.cell == wait.cell),
            Some(_) => false,
        });
        let pending_count = u32::try_from(self.commands.len() + waits).unwrap_or(u32::MAX);
        WakeState {
            pending_count,
            waiting: pending_count > 0
                && (self.turns == 0 || (self.sampling == 0 && !working && waits > 0)),
        }
    }
}

struct Inner {
    state: Mutex<State>,
    updates: watch::Sender<WakeState>,
}

#[derive(Clone)]
pub(crate) struct WakeTracker(Arc<Inner>);

impl Default for WakeTracker {
    fn default() -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            updates: watch::channel(WakeState::default()).0,
        }))
    }
}

enum Registration {
    Turn,
    Sampling,
    Tool(String),
    Wait(String),
}

pub(crate) struct WakeGuard {
    tracker: WakeTracker,
    registration: Registration,
}

impl WakeTracker {
    fn update(&self, update: impl FnOnce(&mut State)) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        update(&mut state);
        let snapshot = state.snapshot();
        self.0.updates.send_if_modified(|current| {
            if *current == snapshot {
                return false;
            }
            *current = snapshot;
            true
        });
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<WakeState> {
        self.0.updates.subscribe()
    }

    pub(crate) fn snapshot(&self) -> WakeState {
        *self.0.updates.borrow()
    }

    pub(crate) fn turn(&self) -> WakeGuard {
        self.update(|state| state.turns += 1);
        WakeGuard {
            tracker: self.clone(),
            registration: Registration::Turn,
        }
    }

    pub(crate) fn sampling(&self) -> WakeGuard {
        self.update(|state| state.sampling += 1);
        WakeGuard {
            tracker: self.clone(),
            registration: Registration::Sampling,
        }
    }

    pub(crate) fn tool(&self, id: &str, cell: Option<String>) -> WakeGuard {
        self.update(|state| {
            state.tools.insert(id.to_owned(), cell);
        });
        WakeGuard {
            tracker: self.clone(),
            registration: Registration::Tool(id.to_owned()),
        }
    }

    pub(crate) fn wait(&self, id: &str) -> WakeGuard {
        self.update(|state| {
            let cell = state.tools.get(id).cloned().flatten();
            state.waits.insert(
                id.to_owned(),
                Wait {
                    cell,
                    wrapper: None,
                },
            );
        });
        WakeGuard {
            tracker: self.clone(),
            registration: Registration::Wait(id.to_owned()),
        }
    }

    pub(crate) fn wait_cell(&self, id: &str, cell: &str, kind: CellWait) -> WakeGuard {
        self.update(|state| {
            state.waits.insert(
                id.to_owned(),
                Wait {
                    cell: Some(cell.to_owned()),
                    wrapper: Some(kind),
                },
            );
        });
        WakeGuard {
            tracker: self.clone(),
            registration: Registration::Wait(id.to_owned()),
        }
    }

    pub(crate) fn arm_command(&self, id: &str, generation: u64) {
        self.update(|state| {
            if state.generation == generation {
                state.commands.insert(id.to_owned(), generation);
            }
        });
    }

    pub(crate) fn complete_command(&self, id: &str) {
        self.update(|state| {
            state.commands.remove(id);
        });
    }

    pub(crate) fn invalidate_commands(&self, generation: u64) {
        self.update(|state| {
            state.generation = generation;
            state.commands.clear();
        });
    }
}

impl Drop for WakeGuard {
    fn drop(&mut self) {
        self.tracker.update(|state| match &self.registration {
            Registration::Turn => state.turns -= 1,
            Registration::Sampling => state.sampling -= 1,
            Registration::Tool(id) => {
                state.tools.remove(id);
            }
            Registration::Wait(id) => {
                state.waits.remove(id);
            }
        });
    }
}

#[cfg(test)]
#[path = "wake_state_tests.rs"]
mod tests;
