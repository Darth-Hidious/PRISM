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
    let url = format!("{}/api/sessions", base_url.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "user_id": user_id,
            "display_name": display_name,
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
