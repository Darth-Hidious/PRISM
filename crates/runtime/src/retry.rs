// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Shared retry policy — the one place that decides whether a failure is
//! worth trying again.
//!
//! PRISM touches the network on every interesting path: the platform API, the
//! LLM stream, MCP servers, the TUI's backend subprocess. Until this module
//! existed each of those treated a dropped connection exactly like a rejected
//! credential — fatal, first time, no second chance — so a Wi-Fi blip or a
//! 503 during a deploy window read to the user as "PRISM is broken".
//!
//! Three rules keep that fix from becoming a worse problem:
//!
//! 1. **Only transient failures are retried.** A 429/503/504, a connect
//!    timeout, a reset socket: worth another attempt. A 400, 401, 402, 403,
//!    404 or a decode error is the server saying the *request* is wrong;
//!    retrying burns the user's time and, on a metered platform, their money.
//!    Anything this module does not recognise is treated as **fatal** —
//!    never as "probably transient".
//! 2. **Only replayable requests are retried.** "Transient" and "safe to send
//!    again" are different questions — see [`Idempotency`]. A read timeout on
//!    a long completion is transient *and* means the model may already have
//!    generated and billed the answer.
//! 3. **Bounded and visible.** [`MAX_RETRIES`] extra attempts, exponential
//!    backoff with jitter, and a `tracing::warn!` before every sleep, so a
//!    slow path reads as slow instead of hung.
//!
//! Call sites that build their own error message from a response (rather than
//! letting `error_for_status` produce a typed `reqwest::Error`) must attach
//! [`HttpStatus`] so the status survives into [`is_retryable`].

use std::future::Future;
use std::io::ErrorKind;
use std::time::Duration;

use backon::{ExponentialBuilder, Retryable};

/// Extra attempts after the first. Three retries = four attempts worst case.
pub const MAX_RETRIES: usize = 3;

/// Longest we will honour a server's `Retry-After` before ignoring it. A
/// server asking us to wait ten minutes is not something to sit through
/// silently.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// What a second attempt would cost if the first one actually landed.
///
/// This is the axis that stops retry from being a footgun. "Transient" is not
/// the same question as "safe to send again": a read timeout on a 300-second
/// LLM completion is transient *and* means the model may have generated —
/// and billed — the whole response already.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Idempotency {
    /// Replaying is free: a GET, a status poll, a local process spawn, a
    /// handshake. Anything transient may be retried.
    #[default]
    Safe,
    /// Replaying may duplicate work, a side effect, or a charge: an LLM
    /// completion, a job submission, a resource creation. Only failures that
    /// prove the request never reached the server are retried.
    Billable,
}

/// HTTP statuses worth another attempt for an idempotent request.
///
/// Deliberately narrow:
///
/// - `408`, `425`, `429` — the server is explicitly telling us to come back.
/// - `502`, `503`, `504` — gateway / availability signals. By construction
///   these are about the hop, not about the request we sent.
///
/// `500` is **not** on the list. It means a handler threw, which is a bug
/// rather than weather, and on a billable endpoint the work may already have
/// been charged before it blew up — so a retry can double-bill for nothing.
/// Every 4xx not listed above is the request's own fault (`400` malformed,
/// `401` bad credential, `402` out of credits, `403` forbidden, `404` wrong
/// URL, `409`/`422` schema) and retrying only wastes the user's time.
#[must_use]
pub fn status_is_retryable(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 502 | 503 | 504)
}

/// Statuses worth another attempt for a request we must not duplicate.
///
/// Only outright refusals survive: `429` (rate limited — the server declined
/// to do the work) and `503` (not serving). `502` and `504` are dropped
/// because a gateway that timed out or got a bad reply may be sitting in
/// front of an upstream that *did* the work; replaying bills it twice. `408`
/// is dropped for the same reason — the server saw part of the request.
#[must_use]
pub fn status_is_retryable_when_billable(status: u16) -> bool {
    matches!(status, 429 | 503)
}

/// IO failures that mean "the pipe broke", not "the request was wrong".
///
/// `NotFound` (missing binary) and `PermissionDenied` are deliberately
/// absent: spawning the same missing executable four times helps nobody.
#[must_use]
pub fn io_is_retryable(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionRefused
            | ErrorKind::BrokenPipe
            | ErrorKind::UnexpectedEof
            | ErrorKind::TimedOut
            | ErrorKind::NotConnected
            | ErrorKind::Interrupted
    )
}

/// Classify a `reqwest` failure.
#[must_use]
pub fn reqwest_is_retryable(err: &reqwest::Error, idem: Idempotency) -> bool {
    // A response came back and the server rejected it — the status decides.
    if let Some(status) = err.status() {
        return match idem {
            Idempotency::Safe => status_is_retryable(status.as_u16()),
            Idempotency::Billable => status_is_retryable_when_billable(status.as_u16()),
        };
    }
    // Bad URL, bad header, unexpected body shape: deterministic, our fault.
    if err.is_builder() || err.is_redirect() || err.is_decode() {
        return false;
    }
    // The connection was never established, so the request cannot have been
    // acted on. Safe to send again whatever it was.
    if err.is_connect() {
        return true;
    }
    if idem == Idempotency::Billable {
        // Everything past this point happened *after* the bytes went out.
        // `is_timeout()` in particular covers the client's own end-to-end
        // request timeout (reqwest cannot tell it apart from a connect
        // timeout), so on a 300 s LLM completion it fires exactly when the
        // model has been generating — and billing — the whole time. Retrying
        // that pays twice for one answer.
        return false;
    }
    if err.is_timeout() {
        return true;
    }
    // Otherwise look for a transport-level IO error underneath (hyper wraps
    // connection resets and mid-stream EOFs several layers down).
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return io_is_retryable(io.kind());
        }
        source = cause.source();
    }
    false
}

/// An HTTP status carried as a typed error, for call sites that render their
/// own message from the response body instead of using `error_for_status`.
///
/// Attach it as the *cause* so the human-readable message stays on top:
///
/// ```ignore
/// return Err(HttpStatus::from_response(&resp))
///     .with_context(|| format!("LLM returned HTTP {status}: {body}"));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("HTTP {status}")]
pub struct HttpStatus {
    pub status: u16,
    /// `Retry-After`, seconds form, when the server sent one.
    pub retry_after_secs: Option<u64>,
}

impl HttpStatus {
    #[must_use]
    pub const fn new(status: u16) -> Self {
        Self {
            status,
            retry_after_secs: None,
        }
    }

    /// Read the status and any `Retry-After` off a response. Borrows, so the
    /// caller can still consume the body afterwards.
    #[must_use]
    pub fn from_response(resp: &reqwest::Response) -> Self {
        Self {
            status: resp.status().as_u16(),
            retry_after_secs: resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok()),
        }
    }
}

/// The single classification entry point.
///
/// Walks the `anyhow` cause chain and takes the verdict of the first link it
/// recognises. An error shape nobody here knows about is **not retryable** —
/// guessing "probably transient" is how retry turns a fast failure into a
/// slow one.
#[must_use]
pub fn is_retryable(err: &anyhow::Error, idem: Idempotency) -> bool {
    for cause in err.chain() {
        if let Some(http) = cause.downcast_ref::<HttpStatus>() {
            return match idem {
                Idempotency::Safe => status_is_retryable(http.status),
                Idempotency::Billable => status_is_retryable_when_billable(http.status),
            };
        }
        if let Some(req) = cause.downcast_ref::<reqwest::Error>() {
            return reqwest_is_retryable(req, idem);
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            // A refused connection never reached anyone; anything else here
            // is post-send and unsafe to replay when duplication costs money.
            return match idem {
                Idempotency::Safe => io_is_retryable(io.kind()),
                Idempotency::Billable => io.kind() == ErrorKind::ConnectionRefused,
            };
        }
    }
    false
}

/// The shared backoff: 250 ms, doubling, jittered, capped at 8 s, at most
/// [`MAX_RETRIES`] extra attempts. Worst case is a few seconds of waiting —
/// long enough to ride out a blip, short enough that nobody thinks it hung.
#[must_use]
pub fn backoff() -> ExponentialBuilder {
    ExponentialBuilder::new()
        .with_min_delay(Duration::from_millis(250))
        .with_factor(2.0)
        .with_max_delay(Duration::from_secs(8))
        .with_max_times(MAX_RETRIES)
        .with_jitter()
}

/// Run `op`, retrying only while [`is_retryable`] says the failure was
/// transient *and* safe to replay. `label` names the operation in the retry
/// log.
pub async fn retrying<T, F, Fut>(label: &str, idem: Idempotency, op: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    retrying_with(label, idem, backoff(), op).await
}

/// [`retrying`] with an explicit backoff — for paths that need a tighter
/// bound (a 15 s MCP handshake cannot afford three retries) or a faster one
/// (tests).
pub async fn retrying_with<T, F, Fut>(
    label: &str,
    idem: Idempotency,
    builder: ExponentialBuilder,
    op: F,
) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    op.retry(builder)
        .when(|err: &anyhow::Error| is_retryable(err, idem))
        // A server-supplied `Retry-After` beats our guess, clamped so it can
        // never park the user indefinitely.
        .adjust(|err, delay| {
            delay.map(|d| server_retry_after(err).unwrap_or(d).min(MAX_RETRY_AFTER))
        })
        .notify(|err: &anyhow::Error, delay: Duration| {
            tracing::warn!(
                op = label,
                delay_ms = delay.as_millis() as u64,
                error = %err,
                "transient failure — retrying",
            );
        })
        // `retrying` is the only executor; every wired path goes through it,
        // so the log line above is the single place a retry becomes visible.
        .await
}

fn server_retry_after(err: &anyhow::Error) -> Option<Duration> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<HttpStatus>())
        .and_then(|http| http.retry_after_secs)
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use anyhow::{Context, anyhow};

    use super::*;

    fn fast() -> ExponentialBuilder {
        ExponentialBuilder::new()
            .with_min_delay(Duration::from_millis(1))
            .with_max_delay(Duration::from_millis(4))
            .with_max_times(MAX_RETRIES)
    }

    #[test]
    fn transient_statuses_retry_and_client_errors_do_not() {
        for status in [408, 425, 429, 502, 503, 504] {
            assert!(status_is_retryable(status), "{status} should be retryable");
        }
        // 402 = out of credits, 401 = bad token: retrying costs the user
        // time and, on a metered platform, money. 500 = handler threw.
        for status in [200, 400, 401, 402, 403, 404, 409, 422, 500, 501] {
            assert!(
                !status_is_retryable(status),
                "{status} must NOT be retryable"
            );
        }
    }

    #[test]
    fn classification_reads_through_the_context_chain() {
        let transient = Err::<(), _>(HttpStatus::new(503))
            .context("fetching balance")
            .context("prism billing")
            .unwrap_err();
        assert!(is_retryable(&transient, Idempotency::Safe));

        let fatal = Err::<(), _>(HttpStatus::new(401))
            .context("fetching balance")
            .unwrap_err();
        assert!(!is_retryable(&fatal, Idempotency::Safe));
    }

    #[test]
    fn broken_pipes_retry_but_missing_binaries_do_not() {
        let eof = Err::<(), _>(std::io::Error::new(ErrorKind::UnexpectedEof, "child died"))
            .context("backend init")
            .unwrap_err();
        assert!(is_retryable(&eof, Idempotency::Safe));

        let missing = Err::<(), _>(std::io::Error::new(ErrorKind::NotFound, "no such binary"))
            .context("spawn")
            .unwrap_err();
        assert!(!is_retryable(&missing, Idempotency::Safe));
    }

    #[test]
    fn unrecognised_errors_are_fatal_not_transient() {
        assert!(!is_retryable(
            &anyhow!("something went sideways"),
            Idempotency::Safe
        ));
    }

    #[tokio::test]
    async fn transient_failure_succeeds_after_retry() {
        let attempts = AtomicUsize::new(0);
        let result: anyhow::Result<&str> =
            retrying_with("test", Idempotency::Safe, fast(), || async {
                if attempts.fetch_add(1, Ordering::SeqCst) < 2 {
                    return Err(HttpStatus::new(503)).context("gateway down");
                }
                Ok("recovered")
            })
            .await;

        assert_eq!(result.unwrap(), "recovered");
        assert_eq!(attempts.load(Ordering::SeqCst), 3, "two retries, then ok");
    }

    #[tokio::test]
    async fn non_retryable_failure_burns_exactly_one_attempt() {
        let attempts = AtomicUsize::new(0);
        let result: anyhow::Result<()> =
            retrying_with("test", Idempotency::Safe, fast(), || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(HttpStatus::new(401)).context("unauthorized")
            })
            .await;

        assert!(result.is_err());
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "a rejected credential must fail on the first attempt",
        );
    }

    #[tokio::test]
    async fn payment_required_never_retries() {
        let attempts = AtomicUsize::new(0);
        let result: anyhow::Result<()> =
            retrying_with("test", Idempotency::Safe, fast(), || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(HttpStatus::new(402)).context("out of credits")
            })
            .await;

        assert!(result.is_err());
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "402 must not be retried"
        );
    }

    /// The distinction that keeps retry from costing money. A gateway
    /// timeout is transient for a GET and unsafe for a completion, because
    /// the upstream behind that gateway may have generated — and billed —
    /// the whole answer before the gateway gave up.
    #[test]
    fn a_billable_request_only_retries_outright_refusals() {
        for status in [408, 425, 502, 504] {
            assert!(
                status_is_retryable(status),
                "{status} is transient for an idempotent request"
            );
            assert!(
                !status_is_retryable_when_billable(status),
                "{status} must NOT be replayed when duplication costs money"
            );
        }
        // The server refused outright, so nothing was done and nothing was
        // billed. These two are what the old hand-rolled 429 loop covered.
        for status in [429, 503] {
            assert!(status_is_retryable_when_billable(status), "{status}");
        }
        for status in [400, 401, 402, 403, 500] {
            assert!(!status_is_retryable_when_billable(status), "{status}");
        }
    }

    #[tokio::test]
    async fn a_billable_request_is_not_replayed_after_a_read_timeout() {
        // `reqwest` cannot tell its own end-to-end request timeout from a
        // connect timeout, so both surface as `io::ErrorKind::TimedOut`. For
        // a completion that means "the model may have finished and charged
        // us" — one attempt only.
        let attempts = AtomicUsize::new(0);
        let result: anyhow::Result<()> =
            retrying_with("test", Idempotency::Billable, fast(), || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(std::io::Error::new(ErrorKind::TimedOut, "read timed out"))
                    .context("LLM request failed")
            })
            .await;

        assert!(result.is_err());
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "a timed-out completion must never be paid for twice",
        );
    }

    #[tokio::test]
    async fn a_billable_request_still_retries_a_refused_connection() {
        // Nothing reached the server, so nothing was billed.
        let attempts = AtomicUsize::new(0);
        let result: anyhow::Result<&str> =
            retrying_with("test", Idempotency::Billable, fast(), || async {
                if attempts.fetch_add(1, Ordering::SeqCst) < 1 {
                    return Err(std::io::Error::new(
                        ErrorKind::ConnectionRefused,
                        "nobody home",
                    ))
                    .context("LLM request failed");
                }
                Ok("sent once")
            })
            .await;

        assert_eq!(result.unwrap(), "sent once");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_billable_request_still_retries_a_429() {
        // Preserves what the hand-rolled LLM loop this replaced already did.
        let attempts = AtomicUsize::new(0);
        let result: anyhow::Result<&str> =
            retrying_with("test", Idempotency::Billable, fast(), || async {
                if attempts.fetch_add(1, Ordering::SeqCst) < 1 {
                    return Err(HttpStatus::new(429)).context("rate limited");
                }
                Ok("through")
            })
            .await;

        assert_eq!(result.unwrap(), "through");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retries_are_bounded() {
        let attempts = AtomicUsize::new(0);
        let result: anyhow::Result<()> =
            retrying_with("test", Idempotency::Safe, fast(), || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(HttpStatus::new(503)).context("still down")
            })
            .await;

        assert!(result.is_err());
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            MAX_RETRIES + 1,
            "never retry forever",
        );
    }

    #[tokio::test]
    async fn server_retry_after_is_honoured_and_clamped() {
        let err = Err::<(), _>(HttpStatus {
            status: 429,
            retry_after_secs: Some(3600),
        })
        .context("rate limited")
        .unwrap_err();
        assert_eq!(server_retry_after(&err), Some(Duration::from_secs(3600)));
        assert_eq!(
            server_retry_after(&err).map(|d| d.min(MAX_RETRY_AFTER)),
            Some(MAX_RETRY_AFTER),
        );
    }
}
