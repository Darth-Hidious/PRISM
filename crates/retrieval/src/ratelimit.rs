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

    /// Wait until the next request slot, then claim it. The guard is held
    /// across the sleep: concurrent waiters reserve successive slots
    /// instead of all computing the same gap from the same `last` and
    /// bursting together when it elapses.
    pub async fn wait(&self) {
        let mut last = self.last.lock().await;
        let now = Instant::now();
        let slot = match *last {
            Some(t) if now < t + self.min_interval => t + self.min_interval,
            _ => now,
        };
        *last = Some(slot);
        if slot > now {
            tokio::time::sleep(slot - now).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

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

    /// F6 regression: with the lock dropped before sleeping, concurrent
    /// waiters all computed the same gap from the same `last` and fired
    /// together (measured: 3 waiters on a 400 ms limiter all at 403 ms).
    #[tokio::test]
    async fn concurrent_waiters_do_not_burst() {
        let limiter = Arc::new(RateLimiter::new(Duration::from_millis(200)));
        let start = Instant::now();
        let mut handles = Vec::new();
        for _ in 0..3 {
            let l = Arc::clone(&limiter);
            handles.push(tokio::spawn(async move {
                l.wait().await;
                Instant::now()
            }));
        }
        let mut fired: Vec<Duration> = Vec::new();
        for h in handles {
            fired.push(h.await.unwrap().duration_since(start));
        }
        fired.sort();
        // The first slot fires at once; every later slot must respect the
        // minimum interval after the previous one. Generous slack for
        // scheduler jitter — the burst fired all three at ~200 ms.
        assert!(
            fired[1] >= Duration::from_millis(150),
            "second waiter fired at {:?}",
            fired[1]
        );
        assert!(
            fired[2] >= Duration::from_millis(350),
            "third waiter fired at {:?} — the waiters burst",
            fired[2]
        );
    }
}
