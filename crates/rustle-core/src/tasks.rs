//! Helpers for spawning Tokio tasks with panic logging.

use std::future::Future;

use tokio::task::JoinHandle;
use tracing::error;

/// Spawns an async task and logs if it panics before completion.
pub fn spawn_logged<F>(name: &'static str, future: F) -> JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let task = tokio::spawn(future);
    tokio::spawn(async move {
        if let Err(error) = task.await {
            if error.is_panic() {
                error!(task = name, "async task panicked");
            } else {
                error!(task = name, %error, "async task was cancelled");
            }
        }
    })
}
