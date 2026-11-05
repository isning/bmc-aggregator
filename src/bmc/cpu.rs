//! Bounded execution for encoding and checksumming; never run CPU work on Tokio workers.
use std::sync::{Arc, LazyLock};
use tokio::sync::Semaphore;
static REALTIME: LazyLock<Arc<Semaphore>> = LazyLock::new(|| {
    Arc::new(Semaphore::new(
        std::thread::available_parallelism()
            .map_or(2, usize::from)
            .clamp(1, 8),
    ))
});
static BATCH: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(2)));

pub async fn run<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    execute(REALTIME.clone(), work).await
}

/// Whole-file work has an independent budget and cannot queue ahead of video frames.
pub async fn run_batch<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    execute(BATCH.clone(), work).await
}

async fn execute<T, F>(budget: Arc<Semaphore>, work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let permit = budget
        .acquire_owned()
        .await
        .map_err(|_| "CPU workers unavailable".to_owned())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|_| "CPU worker failed".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn realtime_work_runs_while_batch_budget_is_exhausted() {
        let _all_batch_slots = BATCH.acquire_many(2).await.unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), run(|| 42))
                .await
                .unwrap()
                .unwrap(),
            42
        );
    }
    #[tokio::test]
    async fn cancellation_holds_budget_until_the_blocking_job_finishes() {
        let budget = Arc::new(Semaphore::new(1));
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let task = tokio::spawn(execute(budget.clone(), move || {
            let _ = started.send(());
            wait.recv().unwrap();
        }));
        ready.await.unwrap();
        task.abort();
        let _ = task.await;
        assert!(budget.try_acquire().is_err());
        release.send(()).unwrap();
        let _permit = tokio::time::timeout(std::time::Duration::from_secs(2), budget.acquire())
            .await
            .unwrap()
            .unwrap();
    }
}
