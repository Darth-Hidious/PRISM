//! Shared HTTP helpers: honest identification, retries with backoff, and
//! polite handling of 429/5xx.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER, USER_AGENT};

use crate::ratelimit::RateLimiter;

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
            bail!("HTTP {status} from {url} after {attempt} attempt(s)");
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
}
