// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Honest translation of a failed platform HTTP response into a CLI error.
//!
//! The MARC27 platform already returns a precise, machine-readable reason for
//! every failure. Its documented shape (`GET /api/v1/agent/capabilities` →
//! `error_handling`) is:
//!
//! ```json
//! {"error": {"code": "string", "message": "string"},
//!  "help": {"...": "..."}, "hint": "...", "suggestions": [...]}
//! ```
//!
//! Before this module the CLI called `.error_for_status()` (which drops the
//! body outright) or substituted a hardcoded human string — so a
//! `token_expired` 401 surfaced as "Not authorized", sending the user hunting
//! for a permissions problem when they only needed `prism login`.
//!
//! The rule here: **the server's own `code` and `message` are always printed
//! verbatim.** The action line is an addition, never a replacement, so this
//! module cannot assert a cause the platform did not report. When the response
//! carries no structured error, we say exactly that and show the status plus a
//! body excerpt rather than guessing.

use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use reqwest::{Response, StatusCode};
use serde_json::Value;

/// Maximum characters of an unstructured body echoed back to the user.
const BODY_EXCERPT_LIMIT: usize = 400;

/// A failed platform response, translated but not editorialised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformError {
    /// HTTP status the platform returned.
    pub status: StatusCode,
    /// Endpoint that failed.
    pub url: String,
    /// The platform's own `error.code`, when it sent one.
    pub code: Option<String>,
    /// The platform's own `error.message`, when it sent one.
    pub message: Option<String>,
    /// `help` / `hint` / `suggestions` the platform attached, flattened to
    /// `(label, text)` pairs in the order they should be shown.
    pub help: Vec<(String, String)>,
    /// Whitespace-collapsed, truncated body — populated **only** when no
    /// structured error could be parsed, so the user still sees what happened.
    pub body_excerpt: Option<String>,
}

impl PlatformError {
    /// Parse a non-2xx response body. Pure and sync so it is unit-testable
    /// without a live server.
    #[must_use]
    pub fn parse(status: StatusCode, url: &str, body: &str) -> Self {
        let json: Option<Value> = serde_json::from_str(body).ok();
        let (code, message) = json.as_ref().map_or((None, None), extract_code_message);

        let help = json.as_ref().map(collect_help).unwrap_or_default();

        // Only fall back to the raw body when the platform gave us nothing
        // structured to show. Never both — that would just be noise.
        let body_excerpt = if code.is_none() && message.is_none() {
            Some(excerpt(body))
        } else {
            None
        };

        // Everything below is server-controlled text we are about to print.
        // Mask credential-shaped values before they can reach a terminal.
        Self {
            status,
            url: url.to_string(),
            code,
            message: message.as_deref().map(redact_secrets),
            help: help
                .into_iter()
                .map(|(k, v)| (k, redact_secrets(&v)))
                .collect(),
            body_excerpt: body_excerpt.as_deref().map(redact_secrets),
        }
    }

    /// Consume a failed response and translate it.
    ///
    /// Async, and consuming, because the platform's reason lives in the body.
    /// Callers that also need a *retry* verdict must take it off the response
    /// first — [`prism_runtime::retry::HttpStatus::from_response`] only
    /// borrows, so classify, then call this. See `PlatformClient::send_retrying`.
    ///
    /// Unlike [`PlatformResponseExt::platform_error_for_status`] this does not
    /// check the status: the caller has already decided the response is a
    /// failure, which lets a call site with a stricter notion of success (any
    /// non-2xx) keep it.
    pub async fn from_response(resp: Response) -> Self {
        let status = resp.status();
        let url = resp.url().to_string();
        // A body we cannot read is itself information — say so rather than
        // pretending the response was empty.
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<response body could not be read: {e}>"));
        Self::parse(status, &url, &body)
    }

    /// What the user should actually do about this.
    ///
    /// Keyed on the platform's own `error.code` first (the codes below are
    /// ones the live API is observed to emit), then on the HTTP status, whose
    /// meaning is fixed by the HTTP spec rather than assumed. Returns `None`
    /// when we have nothing defensible to add — silence beats a wrong guess.
    #[must_use]
    pub fn action(&self) -> Option<&'static str> {
        match self.code.as_deref() {
            Some("token_expired") => {
                Some("your session expired — run `prism login` to sign in again")
            }
            Some("token_invalid") => Some(
                "the stored credential is not a valid platform token — run `prism login` \
                 to replace it",
            ),
            _ => match self.status.as_u16() {
                401 => Some(
                    "PRISM could not authenticate with the platform — run `prism login` \
                     (`prism status` shows the current session)",
                ),
                402 => Some(
                    "the platform refused this call for payment reasons — check the balance \
                     with `prism billing`, then add credits with `prism billing topup <package>`",
                ),
                403 => Some(
                    "you are authenticated, but this account is not permitted to do that — \
                     ask an org owner for access (`prism status` shows the active org and project)",
                ),
                // 404s arrive with the platform's own `hint` + `suggestions`,
                // which are more specific than anything we could add.
                404 => None,
                429 => Some("the platform is rate-limiting this credential — wait, then retry"),
                s if s >= 500 => Some(
                    "this is a platform-side fault, not your CLI — retry shortly; if it \
                     persists, report the status and code above",
                ),
                _ => None,
            },
        }
    }
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Headline: the platform's own words when it gave any.
        match &self.message {
            Some(m) => write!(f, "{m}")?,
            None => write!(
                f,
                "HTTP {} {}",
                self.status.as_u16(),
                self.status.canonical_reason().unwrap_or("error")
            )?,
        }

        if let Some(action) = self.action() {
            write!(f, "\n  what to do: {action}")?;
        }

        match &self.code {
            Some(c) => write!(
                f,
                "\n  reported by the platform as: {c} (HTTP {})",
                self.status.as_u16()
            )?,
            None if self.message.is_some() => {
                write!(f, "\n  HTTP {}", self.status.as_u16())?;
            }
            None => {}
        }

        write!(f, "\n  endpoint: {}", self.url)?;

        for (label, text) in &self.help {
            write!(f, "\n  {label}: {text}")?;
        }

        if let Some(body) = &self.body_excerpt {
            if body.is_empty() {
                write!(
                    f,
                    "\n  the server sent no structured error and an empty body"
                )?;
            } else {
                write!(f, "\n  the server sent no structured error; body: {body}")?;
            }
        }

        Ok(())
    }
}

impl std::error::Error for PlatformError {}

/// A non-blank string field, or nothing. `""` is not a reason — treating it as
/// one would render a confident, empty error line, which is the same defect
/// this module exists to remove.
fn field(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Pull `error.code` / `error.message` out of either shape the fleet emits:
/// the platform's nested `{"error":{"code","message"}}` and the PRISM node
/// server's flat `{"error":"code","message":"..."}`.
fn extract_code_message(json: &Value) -> (Option<String>, Option<String>) {
    match json.get("error") {
        Some(Value::Object(err)) => (field(err.get("code")), field(err.get("message"))),
        Some(code @ Value::String(_)) => (field(Some(code)), field(json.get("message"))),
        _ => (None, field(json.get("message"))),
    }
}

/// Flatten `help` / `hint` / `suggestions` into displayable label/text pairs.
fn collect_help(json: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();

    if let Some(help) = json.get("help").and_then(Value::as_object) {
        for (k, v) in help {
            if let Some(text) = v.as_str() {
                out.push((format!("help.{k}"), text.to_string()));
            }
        }
    }

    if let Some(hint) = json.get("hint").and_then(Value::as_str) {
        out.push(("hint".to_string(), hint.to_string()));
    }

    if let Some(suggestions) = json.get("suggestions").and_then(Value::as_array) {
        for s in suggestions {
            let method = s.get("method").and_then(Value::as_str).unwrap_or("");
            let path = s.get("path").and_then(Value::as_str).unwrap_or("");
            let desc = s.get("description").and_then(Value::as_str).unwrap_or("");
            if !path.is_empty() {
                out.push(("try".to_string(), format!("{method} {path} — {desc}")));
            }
        }
    }

    out
}

/// Collapse whitespace and truncate, so an HTML error page or a stack trace
/// stays readable on one or two terminal lines.
fn excerpt(body: &str) -> String {
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= BODY_EXCERPT_LIMIT {
        return collapsed;
    }
    let truncated: String = collapsed.chars().take(BODY_EXCERPT_LIMIT).collect();
    format!("{truncated}…")
}

/// Mask anything that looks like a live credential before we print it.
///
/// This module's whole design is to echo the server's words, and `/auth/refresh`
/// and `/auth/device/*` are called with a secret in the request body. If the
/// platform ever quotes that value back in an error, the old `.error_for_status()`
/// would have dropped it and we would now print it into a terminal, a scrollback,
/// or a pasted support ticket. Masking here costs nothing and closes that door.
///
/// Only long, real-looking values are masked: the platform's own help text
/// contains the literal placeholder `m27r_...`, which must survive intact.
/// Platform API keys (`m27_`), refresh tokens (`m27r_`) and JWTs (`eyJ…`).
/// The `{12,}` tail is what keeps the platform's literal `m27r_...` placeholder
/// out of the match — it has three characters after the prefix, not twelve.
static SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(m27r_|m27_|eyJ)[A-Za-z0-9_.-]{12,}").expect("literal regex is valid")
});

fn redact_secrets(text: &str) -> String {
    SECRET.replace_all(text, "${1}<redacted>").into_owned()
}

/// `.error_for_status()`, but it keeps the reason the platform gave.
///
/// Drop-in replacement on any call against the MARC27 platform: swap
/// `.error_for_status()?` for `.platform_error_for_status().await?`. The
/// success path returns the response untouched, so the body is still
/// available to `.json()` / `.text()`.
pub trait PlatformResponseExt: Sized {
    /// Non-2xx → an error carrying the platform's own code, message and help.
    fn platform_error_for_status(
        self,
    ) -> impl std::future::Future<Output = anyhow::Result<Self>> + Send;
}

impl PlatformResponseExt for Response {
    async fn platform_error_for_status(self) -> anyhow::Result<Self> {
        let status = self.status();
        if !status.is_client_error() && !status.is_server_error() {
            return Ok(self);
        }
        Err(PlatformError::from_response(self).await.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured live from `GET https://api.marc27.com/api/v1/billing/balance`
    /// with the expired session token that produced the original bug report.
    const LIVE_TOKEN_EXPIRED: &str = r#"{"error":{"code":"token_expired","message":"token expired — refresh it and retry"},"help":{"login":"POST /api/v1/auth/token exchanges a Supabase JWT; device flow: POST /api/v1/auth/device/start","refresh":"POST /api/v1/auth/refresh with {\"refresh_token\": \"m27r_...\"}"}}"#;

    const BALANCE_URL: &str = "https://api.marc27.com/api/v1/billing/balance";

    fn render(status: u16, body: &str) -> String {
        PlatformError::parse(StatusCode::from_u16(status).unwrap(), BALANCE_URL, body).to_string()
    }

    #[test]
    fn expired_token_names_session_expiry_and_prism_login() {
        let rendered = render(401, LIVE_TOKEN_EXPIRED);

        // The platform's own words survive.
        assert!(
            rendered.contains("token expired — refresh it and retry"),
            "server message discarded: {rendered}"
        );
        assert!(
            rendered.contains("token_expired"),
            "server error code discarded: {rendered}"
        );
        // The action is the one that actually fixes it.
        assert!(
            rendered.contains("session expired"),
            "did not name session expiry: {rendered}"
        );
        assert!(
            rendered.contains("prism login"),
            "did not point at `prism login`: {rendered}"
        );
        // The regression this module exists to prevent: an expired token
        // must never be described as a permissions problem.
        assert!(
            !rendered.contains("Not authorized"),
            "reintroduced the wrong diagnosis: {rendered}"
        );
        assert!(
            !rendered.to_lowercase().contains("permitted"),
            "described expiry as a permission denial: {rendered}"
        );
    }

    #[test]
    fn expired_token_surfaces_the_help_block() {
        let rendered = render(401, LIVE_TOKEN_EXPIRED);
        assert!(
            rendered.contains("POST /api/v1/auth/refresh"),
            "help block discarded: {rendered}"
        );
        assert!(
            rendered.contains("help.refresh"),
            "help keys discarded: {rendered}"
        );
    }

    #[test]
    fn permission_denial_still_reads_as_a_permission_denial() {
        let rendered = render(
            403,
            r#"{"error":{"code":"forbidden","message":"role 'viewer' cannot read billing"}}"#,
        );
        assert!(
            rendered.contains("role 'viewer' cannot read billing"),
            "server message discarded: {rendered}"
        );
        assert!(
            rendered.contains("not permitted"),
            "403 lost its permission-denial meaning: {rendered}"
        );
        // …and must not send the user to re-login for a role problem.
        assert!(
            !rendered.contains("prism login"),
            "403 misdiagnosed as an auth problem: {rendered}"
        );
    }

    #[test]
    fn unstructured_500_reports_status_and_body_not_a_cause() {
        let rendered = render(500, "<html><body>\n  502 Bad Gateway\n</body></html>");
        assert!(rendered.contains("500"), "status dropped: {rendered}");
        assert!(
            rendered.contains("502 Bad Gateway"),
            "body excerpt dropped: {rendered}"
        );
        assert!(
            rendered.contains("no structured error"),
            "did not admit the response was unstructured: {rendered}"
        );
        // No invented auth/permission diagnosis.
        assert!(
            !rendered.contains("prism login") && !rendered.contains("not permitted"),
            "fabricated a cause for an opaque 5xx: {rendered}"
        );
    }

    #[test]
    fn empty_body_says_so_rather_than_guessing() {
        let rendered = render(502, "");
        assert!(rendered.contains("502"), "status dropped: {rendered}");
        assert!(
            rendered.contains("empty body"),
            "did not report the empty body: {rendered}"
        );
    }

    #[test]
    fn insolvency_points_at_topup_and_keeps_the_server_reason() {
        let rendered = render(
            402,
            r#"{"error":{"code":"insufficient_credits","message":"org balance 0 mcr; job needs 5000 mcr"}}"#,
        );
        assert!(
            rendered.contains("org balance 0 mcr; job needs 5000 mcr"),
            "server message discarded: {rendered}"
        );
        assert!(
            rendered.contains("prism billing topup"),
            "did not say how to top up: {rendered}"
        );
    }

    #[test]
    fn missing_credential_401_points_at_login() {
        // Live shape when no Authorization header is sent at all.
        let rendered = render(
            401,
            r#"{"error":{"code":"unauthorized","message":"missing authorization header"}}"#,
        );
        assert!(rendered.contains("missing authorization header"));
        assert!(
            rendered.contains("prism login"),
            "no action given: {rendered}"
        );
    }

    #[test]
    fn invalid_token_401_points_at_login() {
        // Live shape for a bearer token the platform cannot parse.
        let rendered = render(
            401,
            r#"{"error":{"code":"token_invalid","message":"invalid bearer token (not a valid Supabase or platform JWT)"},"help":{"how_to_fix":"Send a platform JWT from POST /api/v1/auth/token, a Supabase session JWT, or an m27_* API key in X-API-Key."}}"#,
        );
        assert!(rendered.contains("invalid bearer token"));
        assert!(rendered.contains("prism login"));
        assert!(rendered.contains("help.how_to_fix"));
    }

    #[test]
    fn smart_404_forwards_the_platforms_own_suggestions() {
        // Live shape: the platform tells you the right endpoint. Adding our
        // own guess on top would only bury it.
        let rendered = render(
            404,
            r#"{"error":{"code":"not_found","message":"GET /api/v1/nope is not a valid endpoint"},"hint":"Did you mean: GET /?","suggestions":[{"description":"Status page","method":"GET","path":"/"}]}"#,
        );
        assert!(rendered.contains("is not a valid endpoint"));
        assert!(rendered.contains("Did you mean: GET /?"));
        assert!(rendered.contains("GET / — Status page"));
    }

    #[test]
    fn flat_node_server_error_shape_is_understood() {
        // The PRISM node's own auth middleware emits {"error":"…","message":"…"}.
        let e = PlatformError::parse(
            StatusCode::UNAUTHORIZED,
            "http://127.0.0.1:7777/api/nodes",
            r#"{"error":"unauthorized","message":"Session expired or invalid."}"#,
        );
        assert_eq!(e.code.as_deref(), Some("unauthorized"));
        assert_eq!(e.message.as_deref(), Some("Session expired or invalid."));
        assert!(e.body_excerpt.is_none(), "duplicated the body needlessly");
    }

    #[test]
    fn blank_code_and_message_are_not_treated_as_a_reason() {
        // A confident, empty error line is the same defect in a new place.
        // Blank fields must fall through to the status + body path.
        let e = PlatformError::parse(
            StatusCode::UNAUTHORIZED,
            BALANCE_URL,
            r#"{"error":{"code":"","message":"   "}}"#,
        );
        assert!(e.code.is_none(), "empty code kept: {:?}", e.code);
        assert!(e.message.is_none(), "blank message kept: {:?}", e.message);

        let rendered = e.to_string();
        assert!(
            rendered.contains("HTTP 401 Unauthorized"),
            "no status headline: {rendered}"
        );
        assert!(
            rendered.contains("no structured error"),
            "did not admit there was no reason: {rendered}"
        );
        // Nothing renders as a bare "reported by the platform as: " with a gap.
        assert!(
            !rendered.contains("reported by the platform as: ("),
            "blank code rendered: {rendered}"
        );
    }

    #[test]
    fn endpoint_is_always_named() {
        assert!(render(401, LIVE_TOKEN_EXPIRED).contains(BALANCE_URL));
        assert!(render(500, "boom").contains(BALANCE_URL));
    }

    #[test]
    fn long_bodies_are_collapsed_and_truncated() {
        let body = format!("line one\n\n{}", "x".repeat(1000));
        let e = PlatformError::parse(StatusCode::BAD_GATEWAY, BALANCE_URL, &body);
        let excerpt = e.body_excerpt.expect("excerpt missing");
        assert!(
            excerpt.starts_with("line one x"),
            "not collapsed: {excerpt}"
        );
        assert!(excerpt.chars().count() <= BODY_EXCERPT_LIMIT + 1);
        assert!(excerpt.ends_with('…'));
    }

    #[test]
    fn rate_limit_names_the_limit_not_a_login_problem() {
        let rendered = render(
            429,
            r#"{"error":{"code":"rate_limited","message":"too many requests for this key"}}"#,
        );
        assert!(rendered.contains("too many requests for this key"));
        assert!(rendered.contains("rate-limiting"), "{rendered}");
        assert!(
            !rendered.contains("prism login") && !rendered.contains("not permitted"),
            "429 misdiagnosed as auth/permissions: {rendered}"
        );
    }

    #[test]
    fn smart_404_adds_no_advice_of_its_own() {
        // The platform's hint+suggestions are more specific than anything we
        // could invent, so `action()` must stay silent for 404.
        let e = PlatformError::parse(
            StatusCode::NOT_FOUND,
            BALANCE_URL,
            r#"{"error":{"code":"not_found","message":"nope"},"hint":"Did you mean: GET /?"}"#,
        );
        assert!(e.action().is_none(), "404 editorialised: {:?}", e.action());
        assert!(!e.to_string().contains("what to do:"));
    }

    #[test]
    fn a_credential_echoed_back_by_the_server_is_masked() {
        // We now print bodies that `.error_for_status()` used to discard, and
        // /auth/refresh is called WITH a secret in the request body.
        let rendered = render(
            401,
            r#"{"error":{"code":"token_invalid","message":"refresh_token m27r_a7e9c1d4b8f206e35a9c not found"}}"#,
        );
        assert!(
            !rendered.contains("m27r_a7e9c1d4b8f206e35a9c"),
            "credential leaked into the error: {rendered}"
        );
        assert!(rendered.contains("m27r_<redacted>"), "{rendered}");
        // The rest of the server's sentence must survive.
        assert!(rendered.contains("not found"), "{rendered}");
    }

    #[test]
    fn the_platforms_own_placeholder_survives_redaction() {
        // Live `help.refresh` literally contains `m27r_...` — masking that
        // would destroy the instructions the user needs.
        let rendered = render(401, LIVE_TOKEN_EXPIRED);
        assert!(
            rendered.contains(r#"{"refresh_token": "m27r_..."}"#),
            "help placeholder mangled: {rendered}"
        );
    }

    #[test]
    fn a_jwt_in_an_unstructured_body_is_masked() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.payloadpayload.sigsig";
        let rendered = render(500, &format!("upstream rejected {jwt} — retry"));
        assert!(
            !rendered.contains("payloadpayload"),
            "JWT leaked: {rendered}"
        );
        assert!(rendered.contains("eyJ<redacted>"), "{rendered}");
        assert!(rendered.contains("upstream rejected"), "{rendered}");
    }

    // ── over the wire ──────────────────────────────────────────────
    //
    // The unit tests above cover translation; these cover the trait doing
    // the body read on a real HTTP response, which is where
    // `.error_for_status()` used to throw the reason away.

    #[tokio::test]
    async fn wire_401_carries_the_servers_reason_through() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/billing/balance")
            .with_status(401)
            .with_header("content-type", "application/json")
            .with_body(LIVE_TOKEN_EXPIRED)
            .create_async()
            .await;

        let err = reqwest::get(format!("{}/billing/balance", server.url()))
            .await
            .unwrap()
            .platform_error_for_status()
            .await
            .expect_err("401 must be an error");

        let rendered = format!("{err}");
        assert!(rendered.contains("token expired"), "{rendered}");
        assert!(rendered.contains("token_expired"), "{rendered}");
        assert!(rendered.contains("prism login"), "{rendered}");
        assert!(!rendered.contains("Not authorized"), "{rendered}");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn wire_success_leaves_the_body_readable() {
        // The success path must not consume the response — every caller
        // still does `.json()` after this.
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/billing/balance")
            .with_status(200)
            .with_body(r#"{"credits":1.5}"#)
            .create_async()
            .await;

        let body = reqwest::get(format!("{}/billing/balance", server.url()))
            .await
            .unwrap()
            .platform_error_for_status()
            .await
            .expect("200 must pass through")
            .text()
            .await
            .unwrap();
        assert_eq!(body, r#"{"credits":1.5}"#);
    }

    #[tokio::test]
    async fn wire_matches_error_for_status_on_the_4xx_5xx_boundary() {
        // This is a DROP-IN replacement for reqwest's `.error_for_status()`,
        // so it must error on exactly the same statuses and no others. A
        // "simplification" to `>= 300` would break 55 call sites silently.
        let mut server = mockito::Server::new_async().await;
        for (code, path) in [
            (204_u16, "/no-content"),
            (304, "/not-modified"),
            (399, "/odd-3xx"),
            (400, "/bad-request"),
            (500, "/boom"),
        ] {
            server
                .mock("GET", path)
                .with_status(code.into())
                .create_async()
                .await;

            let resp = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap()
                .get(format!("{}{path}", server.url()))
                .send()
                .await
                .unwrap();
            let reqwest_errs = resp.error_for_status_ref().is_err();
            let ours_errs = resp.platform_error_for_status().await.is_err();
            assert_eq!(
                ours_errs, reqwest_errs,
                "HTTP {code}: ours={ours_errs} reqwest={reqwest_errs}"
            );
        }
    }
}
