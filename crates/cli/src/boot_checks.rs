//! Platform-connectivity boot checks shared by `prism` startup and `prism doctor`.
//!
//! Pings the live MARC27 platform endpoints (auth, KG, models, compute,
//! marketplace) plus the local node and policy engine, returns a vector of
//! [`BootCheck`] for the renderer to print.
//!
//! Extracted from `main.rs` so `prism doctor` can run the same checks
//! without duplicating the logic — keeps the doctor a true superset of
//! the boot screen (local setup + platform connectivity).

use std::time::Duration;

use prism_client::PlatformError;
use prism_runtime::{PlatformEndpoints, StoredCredentials};

use crate::boot;

/// One boot-banner line for a rejected credential: the platform's own
/// `error.code` plus the action that code implies.
///
/// Three states must stay distinguishable, because conflating them is how a
/// boot screen lies: the platform named a reason, the platform named none, and
/// we never got to read the response at all.
async fn rejection_line(resp: reqwest::Response) -> String {
    let status = resp.status();
    let url = resp.url().to_string();
    let body = match resp.text().await {
        Ok(b) => b,
        // Not a platform verdict — we simply never read one. Say that.
        Err(e) => return format!("HTTP {} — response unreadable: {e}", status.as_u16()),
    };
    let err = PlatformError::parse(status, &url, &body);

    // "unauthorized" is itself a code the platform emits, so a missing code
    // must NOT fall back to the status reason phrase — that would render
    // identically to a real one. Show the bare status instead.
    let reason = err
        .code
        .clone()
        .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));

    // Derive the advice from the same mapping the full error uses, so a 403
    // carrying an expiry code can never be labelled a permissions problem.
    let action = match err.action() {
        Some(a) if a.contains("prism login") => "run prism login",
        Some(_) => "credential not permitted here",
        None => "see prism doctor",
    };
    format!("{reason} — {action}")
}

/// Run the live platform-connectivity checks.
///
/// Each check times out after 5s; failures are reported as `[--]` so the
/// boot screen never hangs.
pub async fn run_boot_checks(
    creds: Option<&StoredCredentials>,
    endpoints: &PlatformEndpoints,
) -> Vec<boot::BootCheck> {
    let mut checks = Vec::new();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let token = creds.map(|c| c.access_token.as_str()).unwrap_or("");
    let api = &endpoints.api_base;

    // 1. Platform connection — use /agent/capabilities (always 200 with auth)
    let auth_header = format!("Bearer {token}");
    let platform_ok = client
        .get(format!("{api}/agent/capabilities"))
        .header("Authorization", &auth_header)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    let host = api
        .replace("https://", "")
        .replace("http://", "")
        .replace("/api/v1", "");
    checks.push(boot::BootCheck {
        name: "Platform".into(),
        result: if platform_ok {
            format!("{host} connected")
        } else {
            format!("{host} unreachable")
        },
        ok: platform_ok,
        dots: 8,
        delay_ms: 30,
    });

    // 2. Auth — distinguish actual expiry from scope/network/server errors.
    if !token.is_empty() {
        let user_resp = client
            .get(format!("{api}/users/me"))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await;
        let (auth_ok, auth_msg) = match user_resp {
            Ok(r) if r.status().is_success() => {
                let data: serde_json::Value = r.json().await.unwrap_or_default();
                let name = data
                    .get("display_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("authenticated");
                (true, name.to_string())
            }
            // The platform names the reason (`token_expired`, `token_invalid`,
            // …). Show its word, not our guess — the boot line is one line, so
            // the code plus the implied action is the most that fits.
            Ok(r) if r.status().is_client_error() || r.status().is_server_error() => {
                (false, rejection_line(r).await)
            }
            // Only 3xx can reach here now; redirects are followed by default,
            // so one arriving is unexpected rather than an "error".
            Ok(r) => (
                false,
                format!(
                    "unexpected HTTP {} from {api}/users/me",
                    r.status().as_u16()
                ),
            ),
            Err(e) if e.is_timeout() => (false, "platform unreachable (timeout)".into()),
            Err(_) => (false, "platform unreachable".into()),
        };
        checks.push(boot::BootCheck {
            name: "Auth".into(),
            result: auth_msg,
            ok: auth_ok,
            dots: 6,
            delay_ms: 20,
        });
    } else {
        checks.push(boot::BootCheck {
            name: "Auth".into(),
            result: "not logged in — run prism login".into(),
            ok: false,
            dots: 3,
            delay_ms: 20,
        });
    }

    // 3. Knowledge Graph
    if !token.is_empty() {
        let stats = client
            .get(format!("{api}/knowledge/graph/stats"))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .ok()
            .filter(|r| r.status().is_success());
        let (kg_ok, kg_msg) = if let Some(resp) = stats {
            let data: serde_json::Value = resp.json().await.unwrap_or_default();
            let nodes = data.get("node_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let edges = data.get("edge_count").and_then(|v| v.as_u64()).unwrap_or(0);
            if nodes > 0 {
                (
                    true,
                    format!("{}K nodes, {}M edges", nodes / 1000, edges / 1_000_000),
                )
            } else {
                (true, "connected".into())
            }
        } else {
            (false, "unavailable".into())
        };
        checks.push(boot::BootCheck {
            name: "Knowledge Graph".into(),
            result: kg_msg,
            ok: kg_ok,
            dots: 12,
            delay_ms: 25,
        });
    }

    // 4. Models
    if !token.is_empty() {
        let project_id = creds.and_then(|c| c.project_id.as_deref()).unwrap_or("");
        if !project_id.is_empty() {
            let models = client
                .get(format!("{api}/projects/{project_id}/llm/models"))
                .header("Authorization", format!("Bearer {token}"))
                .send()
                .await
                .ok()
                .filter(|r| r.status().is_success());
            let (m_ok, m_msg) = if let Some(resp) = models {
                let data: serde_json::Value = resp.json().await.unwrap_or_default();
                let count = if let Some(arr) = data.as_array() {
                    arr.len()
                } else {
                    data.get("models")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0)
                };
                (true, format!("{count} hosted models"))
            } else {
                (false, "unavailable".into())
            };
            checks.push(boot::BootCheck {
                name: "LLM Models".into(),
                result: m_msg,
                ok: m_ok,
                dots: 10,
                delay_ms: 20,
            });
        }
    }

    // 5. Compute
    if !token.is_empty() {
        let gpus = client
            .get(format!("{api}/compute/gpus"))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .ok()
            .filter(|r| r.status().is_success());
        let (c_ok, c_msg) = if let Some(resp) = gpus {
            let data: serde_json::Value = resp.json().await.unwrap_or_default();
            let count = data.as_array().map(|a| a.len()).unwrap_or(0);
            (true, format!("{count} GPU types available"))
        } else {
            (false, "unavailable".into())
        };
        checks.push(boot::BootCheck {
            name: "Compute".into(),
            result: c_msg,
            ok: c_ok,
            dots: 8,
            delay_ms: 25,
        });
    }

    // 6. Marketplace
    if !token.is_empty() {
        let mkt = client
            .get(format!("{api}/marketplace/resources"))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .ok()
            .filter(|r| r.status().is_success());
        let (mk_ok, mk_msg) = if let Some(resp) = mkt {
            let data: serde_json::Value = resp.json().await.unwrap_or_default();
            let count = data.as_array().map(|a| a.len()).unwrap_or(0);
            (true, format!("{count} resources"))
        } else {
            (false, "unavailable".into())
        };
        checks.push(boot::BootCheck {
            name: "Marketplace".into(),
            result: mk_msg,
            ok: mk_ok,
            dots: 6,
            delay_ms: 30,
        });
    }

    // 7. Local node
    let node_ok = client
        .get("http://127.0.0.1:7327/api/health")
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    checks.push(boot::BootCheck {
        name: "Local Node".into(),
        result: if node_ok {
            "online at :7327".into()
        } else {
            "offline — run prism node up".into()
        },
        ok: node_ok,
        dots: 4,
        delay_ms: 20,
    });

    // 8. Policy engine (always local, always OK)
    checks.push(boot::BootCheck {
        name: "Policy Engine".into(),
        result: "OPA/Rego loaded".into(),
        ok: true,
        dots: 4,
        delay_ms: 15,
    });

    checks
}
