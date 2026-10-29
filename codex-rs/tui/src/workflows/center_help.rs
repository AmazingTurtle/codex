//! Reuse the Agent center's shared three-group shortcut help renderer.
use super::*;
use crate::key_hint;
use crate::shortcut_help::Group;
use crate::shortcut_help::Shortcut;
use ratatui::text::Line;

pub(super) fn lines(view: &WorkflowCenter, width: u16) -> Vec<Line<'static>> {
    let mut navigation = Group {
        title: "Navigate",
        entries: Vec::new(),
    };
    for (action, label) in [
        (crate::keymap::ListAction::MoveUp, "Up"),
        (crate::keymap::ListAction::MoveDown, "Down"),
        (crate::keymap::ListAction::PageUp, "Top of details"),
        (crate::keymap::ListAction::PageDown, "Bottom of details"),
        (crate::keymap::ListAction::JumpTop, "First row"),
        (crate::keymap::ListAction::JumpBottom, "Last row"),
        (
            crate::keymap::ListAction::Accept,
            "Open Main / worker / run saved",
        ),
        (crate::keymap::ListAction::Cancel, "Back / close"),
    ] {
        navigation.push(view.keymap.primary_hint(action), label);
    }
    navigation.push(
        Some(key_hint::ShortcutHint::Single(key_hint::plain(
            KeyCode::Char('j'),
        ))),
        "Scroll details up 4 rows",
    );
    navigation.push(
        Some(key_hint::ShortcutHint::Single(key_hint::plain(
            KeyCode::Char('k'),
        ))),
        "Scroll details down 4 rows",
    );
    navigation.push(
        view.runtime_keymap
            .app
            .focus_activity
            .first()
            .copied()
            .map(Into::into),
        "Toggle visible tool details",
    );
    let workflows = Group {
        title: "Workflows",
        entries: [
            (' ', "Actions for selected work"),
            ('o', "Open agent chat"),
            ('s', "Stop"),
            ('r', "Retry"),
            ('c', "Recover"),
            ('w', "Save"),
        ]
        .into_iter()
        .map(|(key, label)| Shortcut::new(key_hint::plain(KeyCode::Char(key)), label))
        .collect(),
    };
    let views = Group {
        title: "Views",
        entries: vec![
            Shortcut {
                key: "Tab / Shift+Tab".into(),
                action: "Status filter",
            },
            Shortcut::new(key_hint::plain(KeyCode::Char('f')), "Choose scope"),
            Shortcut::new(key_hint::plain(KeyCode::Char('g')), "Group"),
            Shortcut::new(key_hint::plain(KeyCode::Char('l')), "Saved library / runs"),
            Shortcut::new(key_hint::plain(KeyCode::Char('a')), "Earlier attempts"),
            Shortcut {
                key: "1 / 2 / 3 · ← / →".into(),
                action: "Overview / transcript / source",
            },
            Shortcut {
                key: "[ / ]".into(),
                action: "Previous / next page",
            },
            Shortcut::new(
                key_hint::plain(KeyCode::Char('p')),
                "Older transcript items",
            ),
            Shortcut {
                key: "R".into(),
                action: "Refresh",
            },
        ],
    };
    crate::shortcut_help::group_lines([navigation, workflows, views], width)
}
