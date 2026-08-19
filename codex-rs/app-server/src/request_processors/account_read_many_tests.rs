use super::*;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tokio::sync::Semaphore;

#[tokio::test]
async fn account_reads_are_bounded_and_preserve_input_order() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Semaphore::new(/*permits*/ 0));

    let task = tokio::spawn({
        let active = Arc::clone(&active);
        let max_active = Arc::clone(&max_active);
        let started = Arc::clone(&started);
        let release = Arc::clone(&release);
        async move {
            collect_account_reads(0..6, move |index| {
                let active = Arc::clone(&active);
                let max_active = Arc::clone(&max_active);
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                async move {
                    let current = active.fetch_add(/*val*/ 1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(current, Ordering::SeqCst);
                    started.fetch_add(/*val*/ 1, Ordering::SeqCst);
                    release
                        .acquire()
                        .await
                        .expect("release semaphore should remain open")
                        .forget();
                    active.fetch_sub(/*val*/ 1, Ordering::SeqCst);
                    index
                }
            })
            .await
        }
    });

    while started.load(Ordering::SeqCst) < ACCOUNT_READ_MANY_CONCURRENCY {
        tokio::task::yield_now().await;
    }
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(started.load(Ordering::SeqCst), 5);
    assert_eq!(max_active.load(Ordering::SeqCst), 5);

    release.add_permits(/*n*/ 1);
    while started.load(Ordering::SeqCst) < 6 {
        tokio::task::yield_now().await;
    }
    release.add_permits(/*n*/ 5);

    assert_eq!(
        task.await.expect("account read task"),
        (0..6).collect::<Vec<_>>()
    );
    assert_eq!(max_active.load(Ordering::SeqCst), 5);
}
