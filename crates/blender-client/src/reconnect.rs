//! Bind retry with bounded exponential backoff.
//!
//! Binding the listener can fail transiently: a previous instance of the server
//! may still be shutting down, or the OS may hold the port in `TIME_WAIT`.
//! Giving up immediately would mean the MCP client sees a dead server for no
//! good reason, so the bind is retried -- but with a ceiling, and with a final
//! error that says what to do.

use std::time::Duration;

use tokio::net::TcpListener;

use crate::{Config, error::ClientError};

/// Backoff schedule.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
    pub multiplier: u32,
    /// Give up after this many attempts. `None` retries forever.
    pub max_attempts: Option<u32>,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(250),
            max: Duration::from_secs(5),
            multiplier: 2,
            max_attempts: Some(8),
        }
    }
}

impl Backoff {
    /// Delay before attempt number `attempt` (1-based).
    pub fn delay_for(&self, attempt: u32) -> Duration {
        if attempt <= 1 {
            return self.initial;
        }
        let factor = self
            .multiplier
            .saturating_pow(attempt.saturating_sub(1).min(16));
        self.initial.saturating_mul(factor).min(self.max)
    }

    pub fn should_retry(&self, attempt: u32) -> bool {
        self.max_attempts.is_none_or(|max| attempt < max)
    }
}

/// Bind the listener, retrying on transient failures.
pub async fn bind_with_retry(
    config: &Config,
    backoff: Backoff,
) -> Result<TcpListener, ClientError> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match TcpListener::bind(config.bind).await {
            Ok(listener) => {
                if attempt > 1 {
                    tracing::info!(attempt, address = %config.bind, "bound after retrying");
                }
                return Ok(listener);
            }
            Err(source) => {
                if !backoff.should_retry(attempt) {
                    return Err(ClientError::Bind {
                        address: config.bind.to_string(),
                        source,
                    });
                }
                let delay = backoff.delay_for(attempt);
                tracing::warn!(
                    attempt,
                    address = %config.bind,
                    error = %source,
                    "bind failed, retrying in {delay:?}"
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use super::*;

    #[test]
    fn backoff_grows_then_plateaus() {
        let backoff = Backoff::default();
        assert_eq!(backoff.delay_for(1), Duration::from_millis(250));
        assert_eq!(backoff.delay_for(2), Duration::from_millis(500));
        assert_eq!(backoff.delay_for(3), Duration::from_millis(1000));
        // Capped, not unbounded.
        assert_eq!(backoff.delay_for(20), backoff.max);
    }

    #[test]
    fn attempts_are_bounded_by_default() {
        let backoff = Backoff::default();
        assert!(backoff.should_retry(1));
        assert!(!backoff.should_retry(8));
        let forever = Backoff {
            max_attempts: None,
            ..backoff
        };
        assert!(forever.should_retry(10_000));
    }

    #[tokio::test]
    async fn binds_an_available_port() {
        let config = Config {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            ..Config::default()
        };
        let listener = bind_with_retry(&config, Backoff::default()).await.unwrap();
        assert!(listener.local_addr().unwrap().port() > 0);
    }

    #[tokio::test]
    async fn gives_up_on_a_port_that_stays_busy() {
        let first = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let taken = first.local_addr().unwrap();
        let config = Config {
            bind: taken,
            ..Config::default()
        };
        let backoff = Backoff {
            initial: Duration::from_millis(1),
            max: Duration::from_millis(2),
            multiplier: 2,
            max_attempts: Some(2),
        };
        // Windows allows rebinding in some configurations; only assert on the
        // platforms where a second bind genuinely fails.
        if TcpListener::bind(taken).await.is_ok() {
            return;
        }
        let error = bind_with_retry(&config, backoff).await.unwrap_err();
        assert!(matches!(error, ClientError::Bind { .. }));
    }
}
