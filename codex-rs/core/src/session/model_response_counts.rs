//! Counts usage-bearing responses before watch updates can coalesce.

use std::collections::BTreeMap;

// Match the existing per-turn response accounting bound. Once coverage
// exceeds this bound, an exact count is unavailable rather than a partial total.
const MAX_RESPONSES: usize = 4096;

#[derive(Clone, Default)]
pub(crate) struct ModelResponseCounts {
    responses: BTreeMap<String, String>,
    parents: BTreeMap<String, Option<String>>,
    incomplete: bool,
}

impl ModelResponseCounts {
    pub(crate) fn incomplete(&self) -> bool {
        self.incomplete
    }
    pub(crate) fn record_turn(&mut self, turn: &str, parent: Option<String>) {
        if turn.len() > 128 || parent.as_ref().is_some_and(|parent| parent.len() > 128) {
            self.incomplete = true;
            return;
        }
        if self.parents.contains_key(turn) || self.parents.len() < 128 {
            self.parents.insert(turn.into(), parent);
        } else {
            self.incomplete = true;
        }
    }

    pub(crate) fn for_parent_turns<'a>(
        &self,
        parents: impl IntoIterator<Item = &'a str>,
    ) -> Option<u64> {
        let parents = parents
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if self
            .responses
            .values()
            .any(|turn| !self.parents.contains_key(turn))
        {
            return None;
        }
        self.for_turns(self.parents.iter().filter_map(|(turn, parent)| {
            parent
                .as_deref()
                .filter(|parent| parents.contains(parent))
                .map(|_| turn.as_str())
        }))
    }
    pub(crate) fn record(&mut self, response_id: &str, turn_id: &str) -> bool {
        if let Some(previous) = self.responses.get(response_id) {
            self.incomplete |= previous != turn_id;
            return false;
        }
        if response_id.is_empty()
            || response_id.len() > 128
            || turn_id.len() > 128
            || self.responses.len() >= MAX_RESPONSES
        {
            self.incomplete = true;
            return true;
        }
        self.responses.insert(response_id.into(), turn_id.into());
        true
    }

    pub(crate) fn for_turns<'a>(&self, turns: impl IntoIterator<Item = &'a str>) -> Option<u64> {
        if self.incomplete {
            return None;
        }
        let turns = turns.into_iter().collect::<std::collections::BTreeSet<_>>();
        Some(
            self.responses
                .values()
                .filter(|turn| turns.contains(turn.as_str()))
                .count() as u64,
        )
    }
}
