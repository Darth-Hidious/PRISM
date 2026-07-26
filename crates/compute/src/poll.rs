//! Poll-to-terminal driver for compute jobs.
//!
//! [`poll_to_terminal`] blocks on a [`ComputeBackend`]'s `status()` until the
//! job reaches a terminal state (`Completed`, `Failed`, `Cancelled`), then —
//! for `Completed` — fetches and returns the result via `results()`. This is
//! the shape `handle_run` (the `prism run` CLI path) needs so that
//! `prism run --backend local <image>` blocks until the container exits and
//! surfaces the output, instead of submitting and returning immediately.
//!
//! The loop mirrors `run_compute_job` (prism-cli) but operates through the
//! [`ComputeBackend`] trait abstraction so it covers the local, BYOC, and
//! marc27 backends uniformly, and — crucially — so it can be unit-tested
//! against a fake backend with no Docker and no network (AGENTS.md).

use std::time::Duration;

use anyhow::{Result, bail};

use crate::{ComputeBackend, JobStatus};
use uuid::Uuid;

/// Hard cap on the poll window. Prevents an absurd `--timeout` (e.g.
/// `u64::MAX`) from panicking in `Instant::now() + Duration::from_secs(...)`,
/// which overflows the representable instant range. 31_536_000s ≈ 1 year is
/// far beyond any realistic single-CLI run; callers wanting longer should use
/// the agent/tool path, not block the CLI.
const MAX_POLL_TIMEOUT_SECS: u64 = 31_536_000;

/// Outcome of polling a job to terminal.
#[derive(Debug)]
pub enum PollOutcome {
    /// Job reached `Completed` and `results()` returned a value.
    Completed(serde_json::Value),
    /// Job reached `Completed` but `results()` itself errored (e.g. no
    /// result.json and no logs available). Honest success-without-output: the
    /// job did finish, we just have nothing to show for it. Distinct from a
    /// `Failed`/`Cancelled` job, which is an `Err` return.
    CompletedNoOutput,
}

/// Poll `backend.status(job_id)` until a terminal state is reached or the
/// deadline expires.
///
/// - `poll_timeout_secs` bounds the total poll window; on expiry this returns
///   an `Err` (the caller surfaces it and exits non-zero). It is the caller's
///   responsibility to honour a `--timeout` flag.
/// - `poll_interval` is the sleep between status checks (sleep-then-check, so
///   the first check happens after one interval — matching `run_compute_job`).
///
/// Returns:
/// - `Ok(PollOutcome::Completed(value))` — `Completed`, results available.
/// - `Ok(PollOutcome::CompletedNoOutput)` — `Completed`, but `results()`
///   failed. Not an error: the job succeeded.
/// - `Err(_)` — `Failed`/`Cancelled`, deadline expired, or `status()`
///   errored persistently. The caller must surface this honestly (non-zero
///   exit) — this function never fabricates a success.
pub async fn poll_to_terminal(
    backend: &dyn ComputeBackend,
    job_id: Uuid,
    poll_timeout_secs: u64,
    poll_interval: Duration,
) -> Result<PollOutcome> {
    // Cap and compute the deadline defensively: an absurd `--timeout` (e.g.
    // u64::MAX) must not panic the CLI. `checked_add` falls back to a 1-year
    // deadline if the platform's Instant range would overflow.
    let secs = poll_timeout_secs.clamp(1, MAX_POLL_TIMEOUT_SECS);
    let deadline = std::time::Instant::now()
        .checked_add(Duration::from_secs(secs))
        .unwrap_or_else(|| std::time::Instant::now() + Duration::from_secs(MAX_POLL_TIMEOUT_SECS));

    loop {
        tokio::time::sleep(poll_interval).await;

        let status = match backend.status(job_id).await {
            Ok(status) => status,
            Err(error) => {
                // A transient status error (e.g. a flaky broker, or — for the
                // local backend — a momentarily-unavailable `docker inspect`)
                // should not abort polling prematurely. Keep trying until the
                // deadline, then surface the most recent error honestly.
                if std::time::Instant::now() >= deadline {
                    return Err(anyhow::anyhow!(
                        "status check for job {job_id} failed after {poll_timeout_secs}s: {error}"
                    ));
                }
                continue;
            }
        };

        match status {
            JobStatus::Completed => {
                // Honour the backend's terminal verdict. If it says Completed
                // but results() has nothing, that is a successful-but-empty
                // job — NOT a failure to paper over with an error.
                return match backend.results(job_id).await {
                    Ok(value) => Ok(PollOutcome::Completed(value)),
                    Err(_) => Ok(PollOutcome::CompletedNoOutput),
                };
            }
            JobStatus::Failed { error } => {
                bail!("compute job {job_id} failed: {error}");
            }
            JobStatus::Cancelled => {
                bail!("compute job {job_id} was cancelled");
            }
            JobStatus::Queued | JobStatus::Running { .. } => {
                if std::time::Instant::now() >= deadline {
                    bail!(
                        "compute job {job_id} still running after {poll_timeout_secs}s; \
                         check later with `prism job-status {job_id}`"
                    );
                }
                // otherwise keep polling
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// A scriptable `ComputeBackend` double for testing the poll loop with no
    /// Docker and no network. `statuses` is consumed in order; if exhausted,
    /// the last entry repeats. `results_value`/`results_error` decide what
    /// `results()` returns. Counters record call counts.
    struct FakeBackend {
        statuses: Mutex<Vec<JobStatus>>,
        results_value: serde_json::Value,
        results_error: Mutex<Option<String>>,
        status_calls: Mutex<usize>,
        results_calls: Mutex<usize>,
    }

    impl FakeBackend {
        fn new(statuses: Vec<JobStatus>) -> Self {
            Self {
                statuses: Mutex::new(statuses),
                results_value: serde_json::json!({"score": 0.9}),
                results_error: Mutex::new(None),
                status_calls: Mutex::new(0),
                results_calls: Mutex::new(0),
            }
        }

        fn with_results_error(mut self, msg: &str) -> Self {
            *self.results_error.get_mut().unwrap() = Some(msg.to_string());
            self
        }

        fn status_calls(&self) -> usize {
            *self.status_calls.lock().unwrap()
        }

        fn results_calls(&self) -> usize {
            *self.results_calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl ComputeBackend for FakeBackend {
        async fn submit(&self, _plan: &crate::ExperimentPlan) -> Result<Uuid> {
            unreachable!("poll_to_terminal does not call submit")
        }
        async fn status(&self, _job_id: Uuid) -> Result<JobStatus> {
            *self.status_calls.lock().unwrap() += 1;
            let mut statuses = self.statuses.lock().unwrap();
            if statuses.is_empty() {
                return Ok(JobStatus::Running { progress: 0.5 });
            }
            if statuses.len() == 1 {
                return Ok(statuses[0].clone());
            }
            Ok(statuses.remove(0))
        }
        async fn results(&self, _job_id: Uuid) -> Result<serde_json::Value> {
            *self.results_calls.lock().unwrap() += 1;
            if let Some(msg) = self.results_error.lock().unwrap().clone() {
                bail!("{msg}");
            }
            Ok(self.results_value.clone())
        }
        async fn cancel(&self, _job_id: Uuid) -> Result<()> {
            Ok(())
        }
    }

    fn running() -> JobStatus {
        JobStatus::Running { progress: 0.5 }
    }

    #[tokio::test]
    async fn completes_and_returns_result() {
        // Running → Running → Completed; results() returns {"score":0.9}.
        let backend = FakeBackend::new(vec![running(), running(), JobStatus::Completed]);
        let job_id = Uuid::new_v4();

        let outcome = poll_to_terminal(&backend, job_id, 30, Duration::from_millis(1)).await;

        assert!(outcome.is_ok(), "expected Ok, got {:?}", outcome);
        match outcome.unwrap() {
            PollOutcome::Completed(value) => {
                assert_eq!(value, serde_json::json!({"score": 0.9}));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        // Sleep-then-check: 3 statuses consumed == 3 status calls.
        assert_eq!(backend.status_calls(), 3);
        assert_eq!(backend.results_calls(), 1);
    }

    #[tokio::test]
    async fn failed_is_error_not_success() {
        // A Failed status must surface as Err — never as Ok(Completed).
        let backend = FakeBackend::new(vec![
            running(),
            JobStatus::Failed {
                error: "boom".into(),
            },
        ]);
        let job_id = Uuid::new_v4();

        let outcome = poll_to_terminal(&backend, job_id, 30, Duration::from_millis(1)).await;

        assert!(outcome.is_err(), "Failed must be an error, not a success");
        let msg = outcome.unwrap_err().to_string();
        assert!(
            msg.contains("failed"),
            "error should mention failure: {msg}"
        );
        assert!(
            msg.contains("boom"),
            "error should carry backend's error text: {msg}"
        );
        assert_eq!(
            backend.results_calls(),
            0,
            "results() must not be called on Failed"
        );
    }

    #[tokio::test]
    async fn cancelled_is_error() {
        // Cancelled is treated as an honest error, mirroring run_compute_job.
        let backend = FakeBackend::new(vec![JobStatus::Cancelled]);
        let job_id = Uuid::new_v4();

        let outcome = poll_to_terminal(&backend, job_id, 30, Duration::from_millis(1)).await;

        assert!(outcome.is_err(), "Cancelled must be an error");
        let msg = outcome.unwrap_err().to_string();
        assert!(
            msg.contains("cancelled"),
            "error should mention cancellation: {msg}"
        );
    }

    #[tokio::test]
    async fn timeout_is_error() {
        // Status stays Running forever; tiny timeout + 1ms interval must hit
        // the deadline and bail rather than loop indefinitely.
        let backend = FakeBackend::new(vec![running()]);
        let job_id = Uuid::new_v4();

        let outcome = poll_to_terminal(&backend, job_id, 1, Duration::from_millis(1)).await;

        assert!(outcome.is_err(), "deadline expiry must be an error");
        let msg = outcome.unwrap_err().to_string();
        assert!(
            msg.contains("still running"),
            "error should explain the job timed out: {msg}"
        );
    }

    #[tokio::test]
    async fn absurd_timeout_does_not_panic() {
        // F4: `--timeout` near u64::MAX must not panic in
        // `Instant::now() + Duration::from_secs(...)` (overflow). The deadline
        // is computed before the loop, so even a backend that returns Completed
        // immediately exercises the overflow path. We assert no panic and that
        // the helper still returns its honest result.
        let backend = FakeBackend::new(vec![JobStatus::Completed]);
        let job_id = Uuid::new_v4();

        let outcome = poll_to_terminal(&backend, job_id, u64::MAX, Duration::from_millis(1)).await;

        assert!(outcome.is_ok(), "absurd timeout must not panic or error");
        assert!(matches!(outcome.unwrap(), PollOutcome::Completed(_)));
    }

    #[tokio::test]
    async fn completed_with_results_error_returns_no_output() {
        // Completed but results() fails: honest success-without-output, NOT an
        // error and NOT a fake fabricated result.
        let backend =
            FakeBackend::new(vec![JobStatus::Completed]).with_results_error("no result file");
        let job_id = Uuid::new_v4();

        let outcome = poll_to_terminal(&backend, job_id, 30, Duration::from_millis(1)).await;

        assert!(outcome.is_ok(), "Completed is success even without results");
        match outcome.unwrap() {
            PollOutcome::CompletedNoOutput => {}
            other => panic!("expected CompletedNoOutput, got {other:?}"),
        }
        assert_eq!(backend.results_calls(), 1);
    }

    #[tokio::test]
    async fn transient_status_error_is_retried_then_deadlines() {
        // A backend whose status() always errors should be retried until the
        // deadline, then surfaced as Err (not swallowed as success).
        struct AlwaysErrBackend {
            calls: Mutex<usize>,
        }
        #[async_trait]
        impl ComputeBackend for AlwaysErrBackend {
            async fn submit(&self, _plan: &crate::ExperimentPlan) -> Result<Uuid> {
                unreachable!()
            }
            async fn status(&self, _job_id: Uuid) -> Result<JobStatus> {
                *self.calls.lock().unwrap() += 1;
                bail!("broker hiccup");
            }
            async fn results(&self, _job_id: Uuid) -> Result<serde_json::Value> {
                unreachable!()
            }
            async fn cancel(&self, _job_id: Uuid) -> Result<()> {
                Ok(())
            }
        }
        let backend = AlwaysErrBackend {
            calls: Mutex::new(0),
        };
        let job_id = Uuid::new_v4();

        let outcome = poll_to_terminal(&backend, job_id, 1, Duration::from_millis(1)).await;

        assert!(outcome.is_err(), "persistent status error must surface");
        let msg = outcome.unwrap_err().to_string();
        assert!(
            msg.contains("status check"),
            "error should explain status kept failing: {msg}"
        );
        assert!(
            *backend.calls.lock().unwrap() > 1,
            "status should have been retried, not aborted on first error"
        );
    }

    #[tokio::test]
    async fn router_trait_impl_delegates_without_recursion() {
        // The trait impl on ComputeRouter must forward to the inherent methods
        // and not recurse. Cancel of an unknown job is the cheapest docker-free
        // probe: inherent ComputeRouter::cancel returns Ok(()) for unknown jobs
        // (it only acts on tracked jobs), so a prompt Ok(()) proves delegation
        // works and the trait method did not blow the stack.
        use crate::ComputeRouter;
        let router = ComputeRouter::local_only();
        let backend: &dyn ComputeBackend = &router;
        let unknown = Uuid::new_v4();
        let result = backend.cancel(unknown).await;
        assert!(
            result.is_ok(),
            "router trait cancel(unknown) should be Ok(()): {:?}",
            result
        );
    }
}
