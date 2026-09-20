use anyhow::{Context, Result};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

/// Bound OS calls that can wait indefinitely for privacy consent or a filesystem.
pub struct BlockingOperations {
    permits: Arc<Semaphore>,
    deadline: Duration,
}

impl Default for BlockingOperations {
    fn default() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(8)),
            // Return before the relay's 25-second response deadline.
            deadline: Duration::from_secs(20),
        }
    }
}

impl BlockingOperations {
    pub async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let permit = self.permits.clone().try_acquire_owned().context(
            "OS operations are busy, possibly waiting for macOS privacy permission; no operation was started",
        )?;
        let task = tokio::task::spawn_blocking(move || {
            // A timeout cannot cancel a running syscall. Keep its slot until the
            // syscall actually returns, so retries cannot leak unbounded threads.
            let _permit = permit;
            operation()
        });
        tokio::time::timeout(self.deadline, task)
            .await
            .context("OS operation timed out, possibly waiting for macOS privacy permission or a filesystem. It may still complete; inspect state before retrying a change.")?
            .context("OS operation failed")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn timed_out_work_keeps_its_slot_until_it_finishes() {
        let operations = BlockingOperations {
            permits: Arc::new(Semaphore::new(1)),
            deadline: Duration::from_millis(50),
        };
        let (release, blocked) = std::sync::mpsc::channel();
        let result = operations
            .run(move || {
                blocked.recv()?;
                Ok(())
            })
            .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("may still complete")
        );
        let second = operations.run(|| Ok(())).await;
        assert!(
            second
                .unwrap_err()
                .to_string()
                .contains("no operation was started")
        );
        release.send(()).unwrap();
        let permit = tokio::time::timeout(Duration::from_secs(1), operations.permits.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(permit);
        assert_eq!(operations.run(|| Ok(42)).await.unwrap(), 42);
    }
}
