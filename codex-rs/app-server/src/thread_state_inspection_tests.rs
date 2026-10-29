//! Inspectors receive native events without acquiring the composer's residency lease.
use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn workflow_inspection_delivers_to_its_connection_without_retaining_an_idle_worker() {
    let manager = ThreadStateManager::new();
    let connection = ConnectionId(11);
    let worker = ThreadId::new();
    manager
        .connection_initialized(connection, ConnectionCapabilities::default())
        .await;
    let state = manager
        .try_ensure_workflow_inspection(worker, connection)
        .await
        .unwrap();
    let retained = manager.subscribe_to_has_connections(worker).await.unwrap();
    assert_eq!(
        manager.subscribed_connection_ids(worker).await,
        vec![connection]
    );
    assert!(
        !*retained.borrow(),
        "a read-only inspector cannot hold the native idle residency lease"
    );
    let generation = state.lock().await.listener_generation;
    assert!(
        manager
            .remove_unsubscribed_listener(worker, &state, generation)
            .await,
        "normal idle cleanup must remain possible while only an inspector is attached"
    );
    assert!(manager.subscribed_connection_ids(worker).await.is_empty());
    assert!(
        manager
            .state
            .lock()
            .await
            .thread_ids_by_connection
            .is_empty(),
        "idle cleanup removes the observer's reverse subscription index too"
    );
}

#[tokio::test]
async fn opening_a_composer_promotes_inspection_and_inspector_cleanup_cannot_remove_it() {
    let manager = ThreadStateManager::new();
    let connection = ConnectionId(12);
    let worker = ThreadId::new();
    manager
        .connection_initialized(connection, ConnectionCapabilities::default())
        .await;
    manager
        .try_ensure_workflow_inspection(worker, connection)
        .await
        .unwrap();
    let retained = manager.subscribe_to_has_connections(worker).await.unwrap();
    manager
        .try_ensure_connection_subscribed(
            worker, connection, /*experimental_raw_events*/ false,
        )
        .await
        .unwrap();
    assert!(
        *retained.borrow(),
        "the real composer acquires its normal retention lease"
    );
    manager
        .try_ensure_workflow_inspection(worker, connection)
        .await
        .unwrap();
    assert!(
        *retained.borrow(),
        "refreshing inspection cannot downgrade a real composer"
    );
    assert!(
        !manager
            .unsubscribe_workflow_inspection(worker, connection)
            .await
    );
    assert_eq!(
        manager.subscribed_connection_ids(worker).await,
        vec![connection]
    );
    assert!(
        *retained.borrow(),
        "an inspector's close or failed attach cannot unsubscribe the composer"
    );
    assert_eq!(manager.remove_connection(connection).await, vec![worker]);
    assert!(manager.subscribed_connection_ids(worker).await.is_empty());
    assert!(!*retained.borrow());
    assert!(
        manager
            .state
            .lock()
            .await
            .thread_ids_by_connection
            .is_empty()
    );
}
