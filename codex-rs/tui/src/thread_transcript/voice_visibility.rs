//! The same voice-handoff visibility rule applies to turns and canonical timeline pages.

use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadTimelineEntry;
use codex_app_server_protocol::Turn;
use std::collections::HashMap;
use std::collections::HashSet;

pub(crate) fn is_voice_handoff_item(item: &ThreadItem) -> bool {
    matches!(item, ThreadItem::UserMessage { content, .. }
        if crate::chatwidget::realtime_delegation_input(content).is_some())
}

struct VoiceHandoffVisibility {
    voice_started: bool,
}

impl VoiceHandoffVisibility {
    fn new(has_marker: bool, delegated: bool) -> Self {
        // Without a retained marker, known delegation cannot establish an earlier typed prefix.
        Self {
            voice_started: delegated && !has_marker,
        }
    }

    fn keep(&mut self, item: &ThreadItem) -> bool {
        if is_voice_handoff_item(item) {
            self.voice_started = true;
        }
        !self.voice_started || !crate::chatwidget::is_private_realtime_agent_item(item)
    }
}

pub(crate) fn hide_private_items_after_voice_handoff(items: &mut Vec<ThreadItem>, delegated: bool) {
    let mut visibility =
        VoiceHandoffVisibility::new(items.iter().any(is_voice_handoff_item), delegated);
    items.retain(|item| visibility.keep(item));
}

pub(crate) fn hide_private_timeline_items(
    entries: &mut Vec<ThreadTimelineEntry>,
    turns: &[Turn],
    known_delegated: &[String],
) {
    let markers: HashSet<_> = entries
        .iter()
        .filter_map(|entry| match entry {
            ThreadTimelineEntry::Item { turn_id, item, .. } if is_voice_handoff_item(item) => {
                Some(turn_id.clone())
            }
            _ => None,
        })
        .collect();
    let delegated: HashSet<_> = turns
        .iter()
        .filter(|turn| turn.items.iter().any(is_voice_handoff_item))
        .map(|turn| turn.id.clone())
        .chain(known_delegated.iter().cloned())
        .collect();
    let mut visibility = HashMap::new();
    entries.retain(|entry| match entry {
        ThreadTimelineEntry::Item { turn_id, item, .. } => visibility
            .entry(turn_id.clone())
            .or_insert_with(|| {
                VoiceHandoffVisibility::new(markers.contains(turn_id), delegated.contains(turn_id))
            })
            .keep(item),
        _ => true,
    });
}
