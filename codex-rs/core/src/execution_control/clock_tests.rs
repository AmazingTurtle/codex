use super::*;
use pretty_assertions::assert_eq;

#[test]
fn rapid_pause_resume_preserves_elapsed_without_observing_intermediate_states() {
    let start = Instant::now();
    let mut clock = ClockState {
        elapsed: Duration::ZERO,
        running_since: Some(start),
    };
    clock.transition(
        ExecutionControlState::Pausing,
        start + Duration::from_secs(2),
    );
    clock.transition(
        ExecutionControlState::Paused,
        start + Duration::from_secs(3),
    );
    clock.transition(
        ExecutionControlState::Running,
        start + Duration::from_secs(8),
    );
    assert_eq!(
        clock.elapsed(start + Duration::from_secs(9)),
        Duration::from_secs(3)
    );
    clock.transition(
        ExecutionControlState::Running,
        start + Duration::from_secs(10),
    );
    assert_eq!(
        clock.elapsed(start + Duration::from_secs(11)),
        Duration::from_secs(5)
    );
}

#[test]
fn paused_restore_and_new_observer_baseline_exclude_offline_and_prior_execution() {
    let start = Instant::now();
    let mut clock = ClockState {
        elapsed: Duration::ZERO,
        running_since: Some(start),
    };
    clock.transition(ExecutionControlState::Paused, start);
    assert_eq!(
        clock.elapsed(start + Duration::from_secs(60)),
        Duration::ZERO
    );
    clock.transition(
        ExecutionControlState::Running,
        start + Duration::from_secs(60),
    );
    let baseline = clock.elapsed(start + Duration::from_secs(63));
    clock.transition(
        ExecutionControlState::Pausing,
        start + Duration::from_secs(65),
    );
    assert_eq!(
        clock
            .elapsed(start + Duration::from_secs(90))
            .saturating_sub(baseline),
        Duration::from_secs(2)
    );
}
