use futures::StreamExt;
use std::future::Future;

const ACCOUNT_READ_MANY_CONCURRENCY: usize = 5;

pub(super) async fn collect_account_reads<I, F, Fut, Output>(items: I, read: F) -> Vec<Output>
where
    I: IntoIterator,
    F: FnMut(I::Item) -> Fut,
    Fut: Future<Output = Output>,
{
    futures::stream::iter(items)
        .map(read)
        .buffered(ACCOUNT_READ_MANY_CONCURRENCY)
        .collect()
        .await
}

#[cfg(test)]
#[path = "account_read_many_tests.rs"]
mod tests;
