//! Minimum-interval politeness limiter, one per source.
//!
//! Serializes requests to a single host so sweeps never burst past a source's
//! stated rate. Concurrency happens ACROSS sources, not within one.

use std::time::{Duration, Instant};

use tokio::sync::Mutex;

pub struct RateLimiter {
    min_interval: Duration,
    last: Mutex<Option<Instant>>,
}

impl RateLimiter {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last: Mutex::new(None),
        }
    }

    /// Wait until the next request slot, then claim it.
    pub async fn wait(&self) {
        let mut last = self.last.lock().await;
        let now = Instant::now();
        match *last {
            Some(t) if now < t + self.min_interval => {
                let gap = t + self.min_interval - now;
                drop(last);
                tokio::time::sleep(gap).await;
                *self.last.lock().await = Some(Instant::now());
            }
            _ => *last = Some(now),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn limiter_enforces_minimum_interval() {
        let limiter = RateLimiter::new(Duration::from_millis(120));
        let start = Instant::now();
        limiter.wait().await;
        limiter.wait().await;
        limiter.wait().await;
        assert!(start.elapsed() >= Duration::from_millis(240));
    }
}
