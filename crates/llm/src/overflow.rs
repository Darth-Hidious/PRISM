//! Provider-neutral classification of context-window overflow errors.
//!
//! A request the provider rejected because it exceeds the model's context
//! window is recoverable by the CALLER (shrink the surface, retry) — but
//! only if the loop can tell that failure apart from auth, billing, or
//! transport failures. This module is the classifier; recovery policy stays
//! with each loop.
//!
//! # Attribution
//!
//! The FIRST FIVE recognition patterns are translated verbatim from
//! `isContextWindowExceededError` in `packages/llm/llm/src/error.ts` of the
//! DeepSeek harness (MIT, Copyright (c) 2026 DeepSeek). See the repository
//! `NOTICE` and `LICENSES/DEEPSEEK_MIT` for the license terms.
//!
//! The five patterns below them, marked PRISM-ORIGINAL, are not derived from
//! that source. They were added after an adversarial review measured the
//! upstream set against the providers PRISM actually routes to and found it
//! silently missed Anthropic, Gemini, Ollama/llama.cpp, Bedrock and
//! Mistral/Cohere — the upstream harness targets OpenAI-compatible wording,
//! which is a narrower world than PRISM's.

use std::sync::LazyLock;

use regex::Regex;

/// Structured codes and plain phrases that explicitly name a context bound
/// being exceeded.
static STRUCTURED_CONTEXT_OVERFLOW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(?:^|[^a-z0-9])context[\s_-](?:length|window)[\s_-]",
        r"(?:exceed(?:ed|s)?|overflow(?:ed)?|limit[\s_-]exceeded)(?:$|[^a-z0-9])",
    ))
    .expect("STRUCTURED_CONTEXT_OVERFLOW is a valid pattern")
});

/// "Maximum context length/window" wording with optional allowed/supported.
static MAX_CONTEXT_BOUND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:maximum|max)(?:\s+(?:allowed|supported))?\s+context\s+(?:length|window)\b",
    )
    .expect("MAX_CONTEXT_BOUND is a valid pattern")
});

/// Request-size wording that ties "too large" directly to model context
/// capacity.
static TOO_LARGE_FOR_CONTEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\b(?:request|prompt|input|messages?)\s+(?:is\s+|are\s+)?",
        r"too\s+(?:large|long)\s+for\s+(?:(?:this|the)\s+)?",
        r"(?:model(?:'s)?\s+)?context(?:\s+window)?\b",
    ))
    .expect("TOO_LARGE_FOR_CONTEXT is a valid pattern")
});

/// Plain "too long/large for this/the model" wording.
static TOO_LONG_FOR_MODEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:input|prompt|request)\s+(?:is\s+)?too\s+(?:long|large)\s+for\s+(?:this|the)\s+model\b",
    )
    .expect("TOO_LONG_FOR_MODEL is a valid pattern")
});

/// "Exceeds" wording is safe only when its object is explicitly the model
/// context.
static EXCEEDS_MODEL_CONTEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\b(?:input|prompt|request|messages?)\b.{0,40}",
        r"\b(?:exceed(?:s|ed)?|overflows?|is\s+larger\s+than)\b.{0,40}",
        r"\b(?:the\s+)?(?:model(?:'s)?\s+)?context(?:\s+(?:length|window))?\b",
    ))
    .expect("EXCEEDS_MODEL_CONTEXT is a valid pattern")
});

// ---------------------------------------------------------------------------
// PRISM-ORIGINAL PATTERNS (not derived from the DeepSeek harness).
//
// The five patterns above cover OpenAI-compatible wording. An adversarial
// review evaluated them against the providers PRISM actually routes to and
// found five MISSES — and a miss is not cosmetic: the loop propagates the
// error and discards every proposal the run had already made, which is the
// exact failure the classifier exists to prevent.
//
// Each pattern below is anchored on the word `token(s)` or an explicit
// too-long-for-model phrase. That is deliberate: classifying an unrelated
// failure (auth, billing, transport) AS overflow would silently truncate a
// healthy run, so these are narrow by design rather than generous.
// ---------------------------------------------------------------------------

/// Anthropic: `prompt is too long: 216654 tokens > 200000 maximum`.
static PROMPT_IS_TOO_LONG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bprompt\s+is\s+too\s+long\b").expect("PROMPT_IS_TOO_LONG is a valid pattern")
});

/// Gemini: `The input token count (X) exceeds the maximum number of tokens
/// allowed (Y)`.
///
/// Anchored on INPUT/PROMPT deliberately. A bare `token count … exceeds …
/// maximum` also matches the OUTPUT-cap rejection, which is not recoverable
/// by shrinking the transcript: misreading it as overflow would halve the
/// budget, retry, and then stop with `Overflow` and partial facts and NO
/// error — a truncated run that looks like a quiet paper.
static TOKEN_COUNT_EXCEEDS_MAXIMUM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\b(?:input|prompt|request|context)\b[^.]{0,40}",
        r"\btoken\s+count\b.{0,60}\bexceed(?:s|ed)?\b.{0,60}\b(?:maximum|max)\b",
    ))
    .expect("TOKEN_COUNT_EXCEEDS_MAXIMUM is a valid pattern")
});

/// Ollama / llama.cpp: `Requested tokens (5000) exceed context window of
/// 4096`. The upstream `\brequest\b` alternation cannot match "Requested" —
/// the word boundary fails on the `ed` — so this is its own pattern rather
/// than an edit to the copied one.
static REQUESTED_TOKENS_EXCEED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\brequested\s+tokens\b.{0,40}\bexceed(?:s|ed)?\b")
        .expect("REQUESTED_TOKENS_EXCEED is a valid pattern")
});

/// Bedrock: `Input is too long for requested model.` — "requested model",
/// which the copied `for (this|the) model` wording does not cover.
static TOO_LONG_FOR_REQUESTED_MODEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:input|prompt|request)\s+is\s+too\s+long\s+for\s+requested\s+model\b")
        .expect("TOO_LONG_FOR_REQUESTED_MODEL is a valid pattern")
});

/// Mistral / Cohere: `Too many tokens in prompt for model context`, and
/// `total number of tokens exceeds the model limit`.
///
/// Both halves require the sentence to be ABOUT the prompt/input/context. A
/// bare `too many tokens` also matches billing and quota wording ("too many
/// tokens consumed this period"), and quota is the one failure that must
/// never be swallowed: classified as overflow it would shrink, retry, and
/// return `Ok` with partial facts and no error — the operator learns nothing
/// while being billed. This project has already lost €24 to a silent
/// model-routing failure; the classifier fails closed instead.
static TOO_MANY_TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(?:\btoo\s+many\s+tokens\b[^.]{0,40}\b(?:in|for)\b[^.]{0,40}",
        r"\b(?:prompt|input|request|message|context|model)\b",
        r"|\b(?:total\s+)?number\s+of\s+tokens\b[^.]{0,40}\bexceed(?:s|ed)?\b",
        r"[^.]{0,40}\b(?:model|context|prompt|input)\b[^.]{0,20}\blimit\b)",
    ))
    .expect("TOO_MANY_TOKENS is a valid pattern")
});

/// Recognize the context-overflow wording used by OpenAI-compatible
/// providers and library adapters. `detail` is all available provider
/// code/type/message text joined into one string, so both thrown and
/// in-band delivery styles share one classifier.
#[must_use]
pub fn is_context_window_exceeded(detail: &str) -> bool {
    // Upstream (MIT) patterns.
    STRUCTURED_CONTEXT_OVERFLOW.is_match(detail)
        || MAX_CONTEXT_BOUND.is_match(detail)
        || TOO_LARGE_FOR_CONTEXT.is_match(detail)
        || TOO_LONG_FOR_MODEL.is_match(detail)
        || EXCEEDS_MODEL_CONTEXT.is_match(detail)
        // PRISM-original patterns for the providers PRISM routes to.
        || PROMPT_IS_TOO_LONG.is_match(detail)
        || TOKEN_COUNT_EXCEEDS_MAXIMUM.is_match(detail)
        || REQUESTED_TOKENS_EXCEED.is_match(detail)
        || TOO_LONG_FOR_REQUESTED_MODEL.is_match(detail)
        || TOO_MANY_TOKENS.is_match(detail)
}

/// Classify a complete request failure: the whole anyhow chain is joined so
/// transport wrappers around a provider body are read together with it.
#[must_use]
pub fn error_is_context_window_exceeded(error: &anyhow::Error) -> bool {
    is_context_window_exceeded(&format!("{error:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every provider PRISM actually routes to, in that provider's own
    /// wording. An adversarial review found all five of these MISSED by the
    /// upstream-only pattern set; a miss discards the whole run's proposals,
    /// so this table is the regression net for that.
    #[test]
    fn the_providers_prism_routes_to_are_classified() {
        for wording in [
            // Anthropic
            "prompt is too long: 216654 tokens > 200000 maximum",
            // Gemini
            "The input token count (1048577) exceeds the maximum number of tokens allowed (1048576)",
            // Ollama / llama.cpp
            "Requested tokens (5000) exceed context window of 4096",
            // Bedrock
            "Input is too long for requested model.",
            // Mistral
            "Too many tokens in prompt for model context",
            // Cohere
            "total number of tokens exceeds the model limit",
        ] {
            assert!(
                is_context_window_exceeded(wording),
                "provider overflow wording must be classified: {wording:?}"
            );
        }
    }

    /// The other half of the contract, and the dangerous one: classifying an
    /// UNRELATED failure as overflow would make the loop silently shrink and
    /// truncate a perfectly healthy run. These must all stay unmatched.
    #[test]
    fn unrelated_failures_are_not_mistaken_for_overflow() {
        for wording in [
            "401 Unauthorized: invalid api key",
            "402 Payment Required: insufficient credits",
            "429 Too Many Requests: rate limit exceeded, retry after 30s",
            "connection reset by peer",
            "model not found: no such model",
            "the requested model is not available in this region",
            "output token limit reached; increase max_output_tokens",
            // Quota and billing. These are the expensive misclassification:
            // read as overflow, the loop shrinks, retries, and returns Ok
            // with partial facts and NO error while the account is empty.
            "too many tokens consumed this billing period; upgrade your plan",
            "quota exceeded: too many tokens used this month",
            "insufficient_quota: your token-plan quota has been exhausted",
            // Gemini's OUTPUT cap — not recoverable by shrinking the prompt.
            "the output token count exceeds the maximum number of output tokens allowed",
        ] {
            assert!(
                !is_context_window_exceeded(wording),
                "must NOT be read as a context overflow: {wording:?}"
            );
        }
    }

    /// The wording the real incident carried: a 36-page paper run died at
    /// turn 41 on exactly this provider answer and the loop threw the whole
    /// run away. The classifier exists so a loop can answer it instead.
    #[test]
    fn the_measured_incident_wording_is_classified() {
        assert!(is_context_window_exceeded(
            "request (32786 tokens) exceeds the available context size (32768)"
        ));
    }

    #[test]
    fn openai_compatible_wordings_are_classified() {
        for wording in [
            "This model's maximum context length is 128000 tokens. However, \
             your messages resulted in 130000 tokens.",
            "context length exceeded",
            "context_length_exceeded",
            "maximum context length is 8192 tokens",
            "max supported context window is 32768",
            "request is too large for the model context window",
            "prompt too long for this model",
            "input is larger than the context",
            "messages overflow the model's context",
            "Context-Window limit exceeded",
        ] {
            assert!(
                is_context_window_exceeded(wording),
                "not classified: {wording:?}"
            );
        }
    }

    #[test]
    fn unrelated_provider_failures_are_not_classified() {
        for wording in [
            "invalid api key",
            "rate limit reached, retry after 10s",
            "insufficient quota",
            "model 'nonexistent' not found",
            "the attachment exceeds the file size limit",
            "connection reset by peer",
            "maximum output tokens exceeded",
        ] {
            assert!(
                !is_context_window_exceeded(wording),
                "misclassified: {wording:?}"
            );
        }
    }

    #[test]
    fn the_anyhow_chain_is_read_whole() {
        let inner =
            anyhow::anyhow!("request (32786 tokens) exceeds the available context size (32768)");
        let wrapped = anyhow::anyhow!(inner).context("LLM returned HTTP 400");
        assert!(error_is_context_window_exceeded(&wrapped));
        let other =
            anyhow::anyhow!(anyhow::anyhow!("invalid api key")).context("LLM returned HTTP 401");
        assert!(!error_is_context_window_exceeded(&other));
    }
}
