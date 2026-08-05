//! Mint a session token against the *local* PRISM node's HTTP API.
//!
//! This is distinct from [`crate::PlatformClient`], which talks to the MARC27
//! platform. Here we hit a node's own `POST /api/sessions` endpoint on
//! loopback. The node grants same-machine (loopback) callers local trust, so a
//! bare `user_id` is accepted — no platform token needed. Used by in-process
//! callers (the agent) that must authenticate a follow-up request to the local
//! node, e.g. a workflow `tool` step calling `/api/tools/{name}/run`.

use serde::Deserialize;

#[derive(Deserialize)]
struct SessionResponse {
    session_id: String,
}

/// Mint a loopback session on the local node at `base_url` (e.g.
/// `http://127.0.0.1:7327`) for `user_id`, returning the session token.
///
/// Fails if the node is unreachable or refuses the mint. Callers that want
/// best-effort behaviour should `.ok()` the result.
pub async fn mint_local_session(
    base_url: &str,
    user_id: &str,
    display_name: Option<&str>,
) -> anyhow::Result<String> {
    mint_local_session_with_platform_token(base_url, user_id, display_name, None).await
}

/// Mint a loopback session while proving the launching platform identity.
///
/// The node deliberately ignores `user_id` unless this token verifies against
/// the linked platform. This is the path workflow launchers use on linked
/// nodes; a bare `mint_local_session` remains anonymous-local for legacy local
/// callers.
pub async fn mint_local_session_with_platform_token(
    base_url: &str,
    user_id: &str,
    display_name: Option<&str>,
    platform_token: Option<&str>,
) -> anyhow::Result<String> {
    let url = format!("{}/api/sessions", base_url.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "user_id": user_id,
            "display_name": display_name,
            "platform_token": platform_token,
        }))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("local node session mint failed: {status} — {body}");
    }

    Ok(resp.json::<SessionResponse>().await?.session_id)
}

#[cfg(test)]
mod tests {
    use super::mint_local_session_with_platform_token;
    use mockito::Matcher;

    #[tokio::test]
    async fn workflow_session_mint_carries_launching_platform_identity() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/sessions")
            .match_body(Matcher::Json(serde_json::json!({
                "user_id": "owner-123",
                "display_name": null,
                "platform_token": "owner-platform-token",
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"session_id":"verified-node-session"}"#)
            .create_async()
            .await;

        let token = mint_local_session_with_platform_token(
            &server.url(),
            "owner-123",
            None,
            Some("owner-platform-token"),
        )
        .await
        .expect("session mint");
        assert_eq!(token, "verified-node-session");
        mock.assert_async().await;
    }
}
