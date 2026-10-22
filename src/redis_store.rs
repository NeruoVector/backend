use std::{sync::Arc, time::Duration};

use redis::{
    Client, RedisResult,
    aio::{ConnectionManager, ConnectionManagerConfig},
};
use tokio::sync::OnceCell;

/// Lazily connect once and clone a multiplexed manager for concurrent callers.
/// A cache outage does not prevent startup; failed initialization can be retried.
#[derive(Clone)]
pub struct RedisStore {
    client: Client,
    connection: Arc<OnceCell<ConnectionManager>>,
    timeout: Duration,
}

impl RedisStore {
    pub fn new(client: Client, timeout: Duration) -> Self {
        Self {
            client,
            connection: Arc::new(OnceCell::new()),
            timeout,
        }
    }

    /// The budget includes initialization and manager reconnection, not just
    /// waiting for a response on an already connected socket.
    pub async fn execute<T, F, Fut>(&self, operation: F) -> RedisResult<T>
    where
        F: FnOnce(ConnectionManager) -> Fut,
        Fut: std::future::Future<Output = RedisResult<T>>,
    {
        tokio::time::timeout(self.timeout, async {
            let connection = self.connection().await?;
            operation(connection).await
        })
        .await
        .map_err(|_| {
            redis::RedisError::from(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Redis operation timed out",
            ))
        })?
    }

    async fn connection(&self) -> RedisResult<ConnectionManager> {
        let manager = tokio::time::timeout(
            self.timeout,
            self.connection.get_or_try_init(|| async {
                let config = ConnectionManagerConfig::new()
                    .set_connection_timeout(Some(self.timeout))
                    .set_response_timeout(Some(self.timeout))
                    .set_number_of_retries(1)
                    .set_min_delay(Duration::from_millis(25))
                    .set_max_delay(Duration::from_millis(50));
                self.client.get_connection_manager_with_config(config).await
            }),
        )
        .await
        .map_err(|_| {
            redis::RedisError::from(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Redis connection timed out",
            ))
        })??;
        Ok(manager.clone())
    }
}
