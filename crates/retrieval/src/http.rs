//! Shared HTTP helpers: honest identification, retries with backoff, and
//! polite handling of 429/5xx.

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER, USER_AGENT};

use crate::ratelimit::RateLimiter;

/// A non-success HTTP status that survived the retry budget, carried TYPED
/// so `SourceError::classify` can map it onto the failure taxonomy
/// (401/403 → auth, 429 → rate_limited, 400-class → unsupported_query,
/// the rest → transport). Display is byte-identical to the `bail!` string
/// it replaced — reporting output is pinned by tests and must not shift.
#[derive(Debug)]
pub struct HttpStatusFailure {
    pub status: reqwest::StatusCode,
    pub url: String,
    pub attempts: u32,
}

impl std::fmt::Display for HttpStatusFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HTTP {status} from {url} after {attempts} attempt(s)",
            status = self.status,
            url = self.url,
            attempts = self.attempts
        )
    }
}

impl std::error::Error for HttpStatusFailure {}

/// One retryable GET. Returns the body bytes on HTTP 200.
///
/// 429 and 5xx are retried with exponential backoff (1s, 2s) plus jitter,
/// honouring `Retry-After` when it carries seconds. Any other non-200 is an
/// error surfaced verbatim — a thin result set must be diagnosable.
pub async fn get_with_retry(
    client: &Client,
    limiter: &RateLimiter,
    url: &str,
    headers: &HeaderMap,
    max_attempts: u32,
) -> Result<bytes::Bytes> {
    // Hard offline, checked once before the retry loop.
    //
    // `crates/retrieval` had NO dependency on prism-runtime at all, so the
    // entire `prism papers` surface — eight literature sources plus
    // fulltext.rs's document download — ignored PRISM_OFFLINE completely. It
    // is also an agent tool with `requires_approval: false`
    // (agent/src/command_tools.rs), so a model could fetch from the live
    // internet under hard offline with no human gate.
    //
    // This function is the crate's SOLE outbound call — verified by grepping
    // every `client.get`/`.send()` in `crates/retrieval/src`, which returns
    // only the one below — so one check covers the whole surface, and
    // `fulltext.rs:241` inherits it.
    //
    // `check_url` rather than `enabled()`: a source could legitimately be a
    // loopback mirror, and that is the convention for URL-bearing calls
    // (llm, embed, workflows).
    prism_runtime::offline::check_url(url).map_err(|reason| anyhow::anyhow!(reason))?;

    let mut attempt = 0u32;
    loop {
        attempt += 1;
        limiter.wait().await;
        let response = client
            .get(url)
            .headers(headers.clone())
            .send()
            .await
            .with_context(|| format!("request to {url} failed to complete"))?;
        let status = response.status();
        if status.is_success() {
            return response
                .bytes()
                .await
                .with_context(|| format!("reading body from {url}"));
        }
        let retryable = status.as_u16() == 429 || status.is_server_error();
        if !retryable || attempt >= max_attempts {
            return Err(HttpStatusFailure {
                status,
                url: url.to_string(),
                attempts: attempt,
            }
            .into());
        }
        let retry_after_secs = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok());
        let backoff = Duration::from_millis(1000 * (1 << (attempt - 1)));
        let jitter = Duration::from_millis(rand_jitter_ms());
        let wait = retry_after_secs.map(Duration::from_secs).unwrap_or(backoff) + jitter;
        tokio::time::sleep(wait.min(Duration::from_secs(15))).await;
    }
}

/// Build the identification headers every request carries. Sources with a
/// polite pool (OpenAlex, Crossref) also get `mailto` in the URL itself.
pub fn identification_headers(user_agent: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(user_agent).context("user agent must be a valid header value")?,
    );
    Ok(headers)
}

/// Cheap deterministic jitter without pulling an RNG crate: sub-millisecond
/// clock noise, scaled. Fairness, not cryptography.
fn rand_jitter_ms() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    u64::from((nanos % 250) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_carry_user_agent() {
        let h = identification_headers("prism-retrieval/1.0 (test)").unwrap();
        assert_eq!(h.get(USER_AGENT).unwrap(), "prism-retrieval/1.0 (test)");
    }

    /// `PRISM_OFFLINE=1` must stop a literature fetch before it opens a
    /// socket. Before this, `crates/retrieval` had no dependency on
    /// prism-runtime at all, so `prism papers` — reachable as an agent tool
    /// with `requires_approval: false` — fetched from the live internet under
    /// hard offline with no human gate.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn offline_refuses_a_remote_source_before_any_request() {
        let _guard = env_lock();
        let _restore = OfflineGuard(std::env::var("PRISM_OFFLINE").ok());
        unsafe { std::env::set_var("PRISM_OFFLINE", "1") };

        let limiter = RateLimiter::new(Duration::ZERO);
        let err = get_with_retry(
            &Client::new(),
            &limiter,
            "https://export.arxiv.org/api/query?x=1",
            &HeaderMap::new(),
            1,
        )
        .await
        .expect_err("offline must refuse a remote source");
        let msg = err.to_string();
        assert!(msg.contains("offline mode"), "{msg}");
        assert!(
            msg.contains("export.arxiv.org"),
            "must name what it blocked: {msg}"
        );
    }

    /// A loopback mirror stays reachable — `check_url`, not a blanket refusal.
    /// Nothing is listening on port 1, so reaching a TRANSPORT error (rather
    /// than a policy one) is the proof the guard let it through.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn offline_still_permits_a_loopback_mirror() {
        let _guard = env_lock();
        let _restore = OfflineGuard(std::env::var("PRISM_OFFLINE").ok());
        unsafe { std::env::set_var("PRISM_OFFLINE", "1") };

        let limiter = RateLimiter::new(Duration::ZERO);
        let err = get_with_retry(
            &Client::new(),
            &limiter,
            "http://127.0.0.1:1/api/query",
            &HeaderMap::new(),
            1,
        )
        .await
        .expect_err("nothing is listening on port 1");
        assert!(
            !err.to_string().contains("offline mode"),
            "loopback must not be refused by policy: {err}"
        );
    }

    /// Without the guard the offline tests would pass even if it refused
    /// unconditionally.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn the_guard_is_inert_when_offline_is_unset() {
        let _guard = env_lock();
        let _restore = OfflineGuard(std::env::var("PRISM_OFFLINE").ok());
        unsafe { std::env::remove_var("PRISM_OFFLINE") };

        let limiter = RateLimiter::new(Duration::ZERO);
        let err = get_with_retry(
            &Client::new(),
            &limiter,
            "http://127.0.0.1:1/api/query",
            &HeaderMap::new(),
            1,
        )
        .await
        .expect_err("nothing is listening");
        assert!(
            !err.to_string().contains("offline mode"),
            "guard fired with offline unset: {err}"
        );
    }

    /// PRISM_OFFLINE is process-global; serialize the tests that set it.
    /// Delegates to the workspace lock rather than declaring a private one —
    /// a second mutex for the same process-global excludes nothing.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        prism_runtime::offline::test_support::env_lock()
    }

    /// Restores the var on drop, so a failed assertion cannot leave it set for
    /// the rest of the binary.
    struct OfflineGuard(Option<String>);
    impl Drop for OfflineGuard {
        fn drop(&mut self) {
            unsafe {
                match self.0.take() {
                    Some(v) => std::env::set_var("PRISM_OFFLINE", v),
                    None => std::env::remove_var("PRISM_OFFLINE"),
                }
            }
        }
    }
}
