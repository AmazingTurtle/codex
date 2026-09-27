use super::*;
use pretty_assertions::assert_eq;

#[test]
fn commands_survive_turn_completion_and_do_not_override_work() {
    let tracker = WakeTracker::default();
    let turn = tracker.turn();
    tracker.arm_command("one", /*generation*/ 0);
    tracker.arm_command("one", /*generation*/ 0);
    tracker.arm_command("two", /*generation*/ 0);
    assert_eq!(
        tracker.snapshot(),
        WakeState {
            pending_count: 2,
            waiting: false
        }
    );
    drop(turn);
    assert_eq!(
        tracker.snapshot(),
        WakeState {
            pending_count: 2,
            waiting: true
        }
    );
    let turn = tracker.turn();
    tracker.complete_command("one");
    assert_eq!(
        tracker.snapshot(),
        WakeState {
            pending_count: 1,
            waiting: false
        }
    );
    drop(turn);
    tracker.complete_command("two");
    assert_eq!(tracker.snapshot(), WakeState::default());
}

#[test]
fn interrupted_commands_cannot_rearm_in_an_old_generation() {
    let tracker = WakeTracker::default();
    tracker.arm_command("old", /*generation*/ 0);
    tracker.invalidate_commands(/*generation*/ 1);
    tracker.arm_command("old", /*generation*/ 0);
    assert_eq!(tracker.snapshot(), WakeState::default());
    tracker.arm_command("new", /*generation*/ 1);
    tracker.complete_command("old");
    assert_eq!(
        tracker.snapshot(),
        WakeState {
            pending_count: 1,
            waiting: true
        }
    );
}

#[test]
fn sampling_and_other_tools_take_precedence_over_waits() {
    let tracker = WakeTracker::default();
    let _turn = tracker.turn();
    let _tool = tracker.tool("sleep", /*cell*/ None);
    let wait = tracker.wait("sleep");
    let sampling = tracker.sampling();
    assert_eq!(
        tracker.snapshot(),
        WakeState {
            pending_count: 1,
            waiting: false
        }
    );
    drop(sampling);
    assert_eq!(
        tracker.snapshot(),
        WakeState {
            pending_count: 1,
            waiting: true
        }
    );
    let other = tracker.tool("other", /*cell*/ None);
    assert_eq!(
        tracker.snapshot(),
        WakeState {
            pending_count: 1,
            waiting: false
        }
    );
    drop(other);
    drop(wait);
    assert_eq!(tracker.snapshot(), WakeState::default());
}

#[test]
fn code_mode_wrappers_do_not_double_count_nested_waits() {
    let tracker = WakeTracker::default();
    let _turn = tracker.turn();
    let _tool = tracker.tool("exec", /*cell*/ None);
    let wrapper = tracker.wait_cell("exec", "cell", CellWait::Execution);
    assert_eq!(tracker.snapshot(), WakeState::default());
    let _nested = tracker.tool("agent", Some("cell".into()));
    let nested_wait = tracker.wait("agent");
    assert_eq!(
        tracker.snapshot(),
        WakeState {
            pending_count: 1,
            waiting: true
        }
    );
    drop(wrapper);
    assert_eq!(tracker.snapshot(), WakeState::default());
    let wrapper = tracker.wait_cell("exec", "cell", CellWait::ExplicitWait);
    drop(nested_wait);
    assert_eq!(
        tracker.snapshot(),
        WakeState {
            pending_count: 1,
            waiting: false
        }
    );
    drop(wrapper);
    assert_eq!(tracker.snapshot(), WakeState::default());
}

#[test]
fn subscribers_receive_only_changed_snapshots() {
    let tracker = WakeTracker::default();
    let mut updates = tracker.subscribe();
    tracker.arm_command("one", /*generation*/ 0);
    assert!(updates.has_changed().unwrap());
    assert_eq!(*updates.borrow_and_update(), tracker.snapshot());
    tracker.arm_command("one", /*generation*/ 0);
    assert!(!updates.has_changed().unwrap());
    tracker.complete_command("one");
    assert!(updates.has_changed().unwrap());
}
