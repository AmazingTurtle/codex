use super::AgentSupervision;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::oneshot;

#[tokio::test]
async fn a_discarded_callback_does_not_cancel_observation_or_release_the_revision_fence()
-> Result<(), String> {
    let supervision = Arc::new(AgentSupervision::default());
    let recorded = Arc::new(AtomicBool::new(false));
    let (entered, observing) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let captured = Arc::clone(&recorded);
    let callback = supervision.spawn(async move {
        entered
            .send(())
            .map_err(|_| "observation fixture was discarded")?;
        released.await.map_err(|error| error.to_string())?;
        captured.store(true, Ordering::SeqCst);
        Ok(())
    })?;
    tokio::time::timeout(Duration::from_secs(20), observing)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    drop(callback);
    supervision.fence()?;
    assert!(supervision.spawn(async { Ok(()) }).is_err());
    assert!(!supervision.tasks.is_empty());
    assert!(!recorded.load(Ordering::SeqCst));
    let drain = {
        let supervision = Arc::clone(&supervision);
        tokio::spawn(async move { supervision.drain().await })
    };
    assert!(
        !drain.is_finished(),
        "a live observation must retain the revision drain"
    );
    release
        .send(())
        .map_err(|_| "owned observation disappeared with its callback")?;
    tokio::time::timeout(Duration::from_secs(20), drain)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    assert_eq!(
        (
            supervision.tasks.is_empty(),
            recorded.load(Ordering::SeqCst)
        ),
        (true, true)
    );
    Ok(())
}
