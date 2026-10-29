use super::*;
use crate::context::world_state::WorldState;
use pretty_assertions::assert_eq;

fn rendered_text(fragment: &dyn ContextualUserFragment) -> String {
    match fragment.render() {
        ContentItem::InputText { text } => text,
        content => panic!("expected workflow policy text, got {content:?}"),
    }
}

#[test]
fn policy_updates_append_once_and_retire_when_tools_disappear() {
    let mut history = Vec::new();
    let mut previous = None;
    for (mode, availability, expected_count, expected_text) in [
        (
            DynamicWorkflowsMode::Auto,
            WorkflowAvailability::Unavailable,
            0,
            "",
        ),
        (
            DynamicWorkflowsMode::Auto,
            WorkflowAvailability::Available,
            1,
            "Mode: auto.",
        ),
        (
            DynamicWorkflowsMode::Auto,
            WorkflowAvailability::Available,
            0,
            "",
        ),
        (
            DynamicWorkflowsMode::Explicit,
            WorkflowAvailability::Available,
            1,
            "Mode: explicit.",
        ),
        (
            DynamicWorkflowsMode::Off,
            WorkflowAvailability::Available,
            1,
            "Mode: off.",
        ),
        (
            DynamicWorkflowsMode::Off,
            WorkflowAvailability::Unavailable,
            1,
            "previous dynamic-workflow policy no longer applies",
        ),
        (
            DynamicWorkflowsMode::Off,
            WorkflowAvailability::Unavailable,
            0,
            "",
        ),
    ] {
        let mut world = WorldState::default();
        world.add_section(DynamicWorkflowsState::new(
            mode,
            availability,
            /*messages*/ None,
        ));
        let old_history = history.clone();
        let (snapshot, updates) = world.render_history_fragment_diff(previous.as_ref(), &history);
        assert_eq!(updates.len(), expected_count);
        for update in &updates {
            assert!(rendered_text(update.as_ref()).contains(expected_text));
        }
        history.extend(
            updates
                .into_iter()
                .map(ContextualUserFragment::into_boxed_response_item),
        );
        assert_eq!(&history[..old_history.len()], old_history.as_slice());
        previous = Some(snapshot);
    }
}

#[test]
fn retained_policy_is_replaced_without_a_snapshot() {
    let retained = ContextualUserFragment::into(DynamicWorkflowsState::new(
        DynamicWorkflowsMode::Auto,
        WorkflowAvailability::Available,
        /*messages*/ None,
    ));
    let mut world = WorldState::default();
    world.add_section(DynamicWorkflowsState::new(
        DynamicWorkflowsMode::Off,
        WorkflowAvailability::Available,
        /*messages*/ None,
    ));
    let updates = world
        .render_history_fragment_diff(/*previous*/ None, &[retained])
        .1;
    assert_eq!(updates.len(), 1);
    assert!(rendered_text(updates[0].as_ref()).contains(REPLACEMENT_NOTICE));
    assert!(rendered_text(updates[0].as_ref()).contains("Mode: off."));
}

#[test]
fn catalog_guidance_is_bounded_without_suppressing_policy() {
    for (guidance, expected) in [
        (None, DEFAULT_GUIDANCE),
        (Some("".to_string()), ""),
        (
            Some("Custom coordination guidance.".to_string()),
            "Custom coordination guidance.",
        ),
        (Some("x".repeat(MAX_GUIDANCE_BYTES + 1)), DEFAULT_GUIDANCE),
        (Some("é".repeat(MAX_GUIDANCE_BYTES)), DEFAULT_GUIDANCE),
    ] {
        let state = DynamicWorkflowsState::new(
            DynamicWorkflowsMode::Explicit,
            WorkflowAvailability::Available,
            Some(&DynamicWorkflowToolMessages {
                guidance,
                ..Default::default()
            }),
        );
        assert!(state.instructions.ends_with(expected));
        assert!(
            state
                .instructions
                .contains("human invokes $dynamic-workflow")
        );
        assert!(state.instructions.len() <= MAX_GUIDANCE_BYTES + 600);
    }
}
