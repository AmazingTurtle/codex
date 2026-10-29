//! Opening a run returns to the displayed owning conversation through normal selection.
use super::*;

#[test]
fn run_row_opens_main_from_a_worker_without_reopening_the_center() {
    let state = fixture();
    let owner = ThreadId::from_u128(7);
    {
        let mut state = state.lock().unwrap();
        state.owner = Some(owner);
        state.displayed_thread = Some(ThreadId::from_u128(42));
        state.child = None;
    }
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state, AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Enter.into());
    assert!(
        matches!(events.try_recv(), Ok(AppEvent::Workflow(WorkflowEvent::SelectAgent(id))) if id == owner)
    );
    assert!(
        events.try_recv().is_err(),
        "returning to Main must not schedule reopening the center"
    );
    assert!(center.is_complete());
}

#[test]
fn run_enter_closes_an_already_displayed_main_and_opens_the_explicit_foreign_owner() {
    let state = fixture();
    let owner = ThreadId::from_u128(7);
    {
        let mut state = state.lock().unwrap();
        state.owner = Some(owner);
        state.displayed_thread = Some(owner);
        state.child = None;
    }
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state.clone(), AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Enter.into());
    assert!(center.is_complete());
    assert!(events.try_recv().is_err());
    let other = ThreadId::from_u128(8);
    state.lock().unwrap().rows[0].owner = Some(other);
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let mut center = WorkflowCenter::new(state, AppEventSender::new(tx));
    center.handle_key_event(KeyCode::Enter.into());
    assert!(
        matches!(events.try_recv(), Ok(AppEvent::Workflow(WorkflowEvent::OpenOwner(id))) if id == other)
    );
    assert!(center.is_complete());
}
