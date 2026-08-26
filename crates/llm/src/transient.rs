//! Classify a request failure as a TRANSIENT TRANSPORT fault worth retrying.
//!
//! Sibling of [`crate::overflow`], and written for the same reason: the one
//! authoritative statement of what went wrong arrives in the provider's own
//! wording, at the moment it matters.
//!
//! Measured 2026-08-25: four consecutive long-generation turns died with
//! `error reading SSE chunk: … operation timed out` and
//! `… connection reset`. Every one discarded a turn that had already read a
//! paper, extracted its equations and provisioned a venv. The agent loop
//! guarded exactly one error — context overflow — and let every transport
//! fault through to kill the turn, while `prism_ingest`'s induction path had
//! retried the same class of failure all along.
//!
//! DELIBERATELY NARROW. A retry re-bills the prompt, so this must match only
//! faults where the request provably did not land. Anything that carries a
//! provider VERDICT — 400, 401, 402, quota, content filter — is not transient
//! and must not match, no matter how it is wrapped; retrying those burns money
//! and never succeeds. The negative table in the tests is the contract.

/// Wordings for a connection that dropped or never completed. Substring
/// matching on the joined anyhow chain, lowercased — the same shape
/// `overflow::error_is_context_window_exceeded` uses, because the transport
/// wrapper and the provider body have to be read together.
const TRANSIENT_MARKERS: &[&str] = &[
    // Observed live against the z.ai GLM endpoint, both variants.
    "operation timed out",
    "connection reset",
    // hyper/reqwest transport faults for a stream that died mid-flight.
    "connection closed before message completed",
    // hyper renders this both ways depending on whether the Display or the
    // Debug form reaches the chain: "incomplete message" and
    // "hyper::Error(IncompleteMessage)". The test table caught the missing
    // second form.
    "incomplete message",
    "incompletemessage",
    "broken pipe",
    "error reading a body from connection",
    "connection refused",
    "timed out reading",
    "request timeout",
    "tls handshake",
    "dns error",
];

/// Wordings that must NEVER be treated as transient even when a transport
/// wrapper surrounds them: the provider reached a decision, and repeating the
/// request repeats the charge without changing the answer.
const TERMINAL_MARKERS: &[&str] = &[
    "insufficient credit",
    "insufficient_quota",
    "quota",
    "unauthorized",
    "invalid api key",
    "authentication",
    "permission denied",
    "content filter",
    "content_policy",
    "400 bad request",
    "401",
    "402",
    "403",
    "404",
    "422",
];

/// True when `detail` describes a transport fault that is worth one retry.
///
/// Terminal markers win: a 402 delivered inside a stream error is still a 402.
#[must_use]
pub fn is_transient_transport(detail: &str) -> bool {
    let haystack = detail.to_ascii_lowercase();
    if TERMINAL_MARKERS.iter().any(|m| haystack.contains(m)) {
        return false;
    }
    TRANSIENT_MARKERS.iter().any(|m| haystack.contains(m))
}

/// Classify a complete request failure. The whole anyhow chain is joined so a
/// transport wrapper is read together with the body it wraps.
#[must_use]
pub fn error_is_transient_transport(error: &anyhow::Error) -> bool {
    is_transient_transport(&format!("{error:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two failures actually observed, verbatim from the TUI, plus the
    /// transport faults a streaming client sees in practice. A miss here costs
    /// a whole turn's work.
    #[test]
    fn the_faults_that_killed_four_turns_are_classified_transient() {
        for wording in [
            "error reading SSE chunk: error decoding response body: request or response \
             body error: operation timed out",
            "error reading SSE chunk: error decoding response body: request or response \
             body error: error reading a body from connection: connection reset",
            "connection closed before message completed",
            "hyper::Error(IncompleteMessage)",
            "error sending request: Broken pipe (os error 32)",
        ] {
            assert!(
                is_transient_transport(wording),
                "must retry a dropped stream: {wording}"
            );
        }
    }

    /// The contract that keeps a retry from burning the user's money. Each of
    /// these is a provider VERDICT; repeating the request repeats the charge
    /// and cannot change the outcome.
    #[test]
    fn provider_verdicts_are_never_retried_even_wrapped_in_transport_words() {
        for wording in [
            "HTTP status client error (402 Insufficient Credits) for url",
            "error reading SSE chunk: 402 insufficient credits",
            "401 Unauthorized: invalid api key",
            "400 Bad Request: messages: text content blocks must be non-empty",
            "content filter triggered",
            "You exceeded your current quota",
        ] {
            assert!(
                !is_transient_transport(wording),
                "must NOT retry a provider verdict: {wording}"
            );
        }
    }

    /// Overflow has its own recovery (compact, then retry). It must not be
    /// swallowed by this one, which would retry the identical oversized
    /// request instead of shrinking it.
    #[test]
    fn context_overflow_is_not_classified_as_transient() {
        for wording in [
            "prompt is too long: 216654 tokens > 200000 maximum",
            "Requested tokens (5000) exceed context window of 4096",
        ] {
            assert!(
                !is_transient_transport(wording),
                "overflow is not transport: {wording}"
            );
        }
    }
}
