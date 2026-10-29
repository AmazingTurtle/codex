use super::*;
use pretty_assertions::assert_eq;
use std::sync::Arc;

#[test]
fn attempt_clock_excludes_queue_time_and_freezes_at_owned_settlement() {
    let clocks = AttemptClocks::default();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let observer: WorkflowObserver = Arc::new(move |event| {
        if let WorkflowHostEvent::AttemptClock {
            attempt_id,
            active_duration_ms,
            ..
        } = event
        {
            captured
                .lock()
                .unwrap()
                .push((attempt_id, active_duration_ms));
        } else {
            panic!("unexpected clock observation");
        }
        Ok(())
    });
    // Recovered/queued attempts without a native baseline have no invented time.
    clocks
        .publish(&observer, "recovered", 100, /*complete*/ true)
        .unwrap();
    clocks.admitted("accepted", 100);
    clocks
        .publish(&observer, "accepted", 110, /*complete*/ false)
        .unwrap();
    // The existing gate clock stays unchanged while the conversation is paused.
    clocks
        .publish(&observer, "accepted", 110, /*complete*/ false)
        .unwrap();
    clocks
        .publish(&observer, "accepted", 125, /*complete*/ true)
        .unwrap();
    // Later independent human work on the same thread cannot extend this attempt.
    clocks
        .publish(&observer, "accepted", 900, /*complete*/ true)
        .unwrap();
    clocks.begin_observation();
    clocks
        .publish(&observer, "accepted", 5000, /*complete*/ true)
        .unwrap();
    clocks.resume_active("live-resumed", 5000);
    clocks
        .publish(&observer, "live-resumed", 5010, /*complete*/ true)
        .unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            ("accepted".into(), 10),
            ("accepted".into(), 10),
            ("accepted".into(), 25),
            ("accepted".into(), 25),
            ("live-resumed".into(), 10),
        ]
    );
}
