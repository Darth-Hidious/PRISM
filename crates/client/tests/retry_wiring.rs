// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Proves the shared retry policy is actually *wired into* `PlatformClient`,
//! not merely available to it.
//!
//! Every platform call in PRISM funnels through `get`/`post`/`delete`, so
//! these two tests cover the whole platform surface: a transient failure has
//! to come back, and a rejected credential has to fail on the first attempt.
//! The second one is the one that matters — retry that does not know when to
//! stop is worse than no retry at all, because it spends the user's time and,
//! on a metered platform, their money.

use prism_client::PlatformClient;
use serde_json::Value;

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
