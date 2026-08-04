// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Proves the shared retry policy is actually *wired into* `PlatformClient`,
//! not merely available to it.
//!
//! Every platform call in PRISM funnels through `get`/`post`/`delete`, so
//! these two tests cover the whole platform surface: a transient failure has
//! to come back, and a rejected credential has to fail on the first attempt.
//! The second one is the one that matters — retry that does not know when to
//! stop is worse than no retry at all, because it spends the user's time and,
//! on a metered platform, their money.
//!
//! The last two tests guard the seam where retry meets error honesty. A
//! failed response has to answer both "try again?" and "what do I tell the
//! user?", and the answers live in different places: the status (borrowed)
//! and the body (consumed). Collapsing them either way is a silent
//! regression — classify off the body and a 401 gets retried four times;
//! render off the status alone and the user is back to "returned error status
//! 401" with no idea that `prism login` fixes it.

use prism_client::{DeviceFlowAuth, PlatformClient};
use serde_json::Value;

/// Captured live from the platform: an expired session token.
const LIVE_TOKEN_EXPIRED: &str = r#"{"error":{"code":"token_expired","message":"token expired — refresh it and retry"},"help":{"refresh":"POST /api/v1/auth/refresh with {\"refresh_token\": \"m27r_...\"}"}}"#;

#[tokio::test]
async fn a_transient_platform_failure_is_retried_and_then_succeeds() {
    let mut server = mockito::Server::new_async().await;

    // Mockito serves the first mock that still has hits outstanding, so this
    // pair means: 503, 503, then 200.
    let flaky = server
        .mock("GET", "/users/me")
        .with_status(503)
        .expect(2)
        .create_async()
        .await;
    let recovered = server
        .mock("GET", "/users/me")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"id":"u1"}"#)
        .expect(1)
        .create_async()
        .await;

    let client = PlatformClient::new(server.url()).with_token("m27_test");
    let user: Value = client.get("/users/me").await.expect("should recover");

    assert_eq!(user["id"], "u1");
    flaky.assert_async().await;
    recovered.assert_async().await;
}

#[tokio::test]
async fn a_rejected_credential_fails_on_the_first_attempt() {
    let mut server = mockito::Server::new_async().await;
    let unauthorized = server
        .mock("GET", "/users/me")
        .with_status(401)
        .with_body(r#"{"error":"invalid token"}"#)
        // Exactly one. Retrying a 401 cannot succeed; it only makes the
        // failure slower.
        .expect(1)
        .create_async()
        .await;

    let client = PlatformClient::new(server.url()).with_token("m27_expired");
    let err = client
        .get::<Value>("/users/me")
        .await
        .expect_err("401 must not be retried into a success");

    assert!(format!("{err:#}").contains("401"), "{err:#}");
    unauthorized.assert_async().await;
}

#[tokio::test]
async fn out_of_credits_is_not_retried_either() {
    let mut server = mockito::Server::new_async().await;
    let payment_required = server
        .mock("POST", "/compute/submit")
        .with_status(402)
        .with_body(r#"{"error":"insufficient credits"}"#)
        .expect(1)
        .create_async()
        .await;

    let client = PlatformClient::new(server.url()).with_token("m27_test");
    let err = client
        .post::<_, Value>("/compute/submit", &serde_json::json!({}))
        .await
        .expect_err("402 must fail immediately");

    assert!(format!("{err:#}").contains("402"), "{err:#}");
    payment_required.assert_async().await;
}

#[tokio::test]
async fn a_terminal_platform_error_keeps_both_the_verdict_and_the_reason() {
    // The merge seam: `send_retrying` classifies from a borrow of the
    // response, then consumes it for the platform's own words. Both halves
    // are asserted here because either one alone still passes half the time.
    let mut server = mockito::Server::new_async().await;
    let expired = server
        .mock("GET", "/billing/balance")
        .with_status(401)
        .with_header("content-type", "application/json")
        .with_body(LIVE_TOKEN_EXPIRED)
        // The retry verdict half: a rejected credential is spent on the
        // first attempt, not four. This only holds if `HttpStatus` survives
        // into the cause chain underneath the rich error.
        .expect(1)
        .create_async()
        .await;

    let client = PlatformClient::new(server.url()).with_token("m27_expired");
    let err = client
        .get::<Value>("/billing/balance")
        .await
        .expect_err("401 must be an error");
    let rendered = format!("{err:#}");

    // The error-honesty half: the platform's own code and message, the help
    // block it attached, and the action that actually fixes it.
    assert!(
        rendered.contains("token expired — refresh it and retry"),
        "server message discarded: {rendered}"
    );
    assert!(
        rendered.contains("token_expired"),
        "server error code discarded: {rendered}"
    );
    assert!(
        rendered.contains("POST /api/v1/auth/refresh"),
        "help block discarded: {rendered}"
    );
    assert!(
        rendered.contains("prism login"),
        "did not point at `prism login`: {rendered}"
    );
    // The regression this test exists to catch: the retry layer's own
    // status-only message replacing the platform's reason.
    assert!(
        !rendered.contains("returned error status"),
        "rich error replaced by the bare status line: {rendered}"
    );
    expired.assert_async().await;
}

#[tokio::test]
async fn a_rejected_refresh_token_is_spent_once_and_says_why() {
    // `DeviceFlowAuth::refresh_token` runs unattended and carries the same
    // reconciliation as `send_retrying`, so it gets the same guard. It is
    // `Idempotency::Billable`: the platform rotates the refresh token, so a
    // replay burns a rotation and logs the user out for real.
    let mut server = mockito::Server::new_async().await;
    let rejected = server
        .mock("POST", "/auth/refresh")
        .with_status(401)
        .with_header("content-type", "application/json")
        .with_body(LIVE_TOKEN_EXPIRED)
        .expect(1)
        .create_async()
        .await;

    let err = DeviceFlowAuth::refresh_token(&reqwest::Client::new(), &server.url(), "m27r_stale")
        .await
        .expect_err("a rejected refresh token must be an error");
    let rendered = format!("{err:#}");

    assert!(
        rendered.contains("token expired — refresh it and retry"),
        "server message discarded: {rendered}"
    );
    assert!(
        rendered.contains("prism login"),
        "did not point at `prism login`: {rendered}"
    );
    assert!(
        !rendered.contains("returned error status"),
        "rich error replaced by the bare status line: {rendered}"
    );
    rejected.assert_async().await;
}
