//! Boot checks shared by `prism` startup and `prism doctor`.
//!
//! Two halves. The LOCAL half (node daemon, policy engine) always runs.
//! The PLATFORM half (auth, knowledge graph, models, compute, marketplace)
//! only runs when a platform credential actually exists — see
//! [`platform_configured`].
//!
//! Extracted from `main.rs` so `prism doctor` can run the same checks
//! without duplicating the logic — keeps the doctor a true superset of
//! the boot screen (local setup + platform connectivity).

use std::time::Duration;

use prism_client::PlatformError;
use prism_runtime::platform_env::PlatformVar;
use prism_runtime::{PlatformEndpoints, StoredCredentials};

use crate::boot;

/// Settings that carry a platform credential on the headless/agent path
/// (no `prism login`, no `~/.prism` state).
///
/// Each is checked under BOTH its neutral `PRISM_*` name and its historical
/// `MARC27_*` alias, because `PlatformVar::get()` resolves both. Before this
/// used `PlatformVar`, the list was three hardcoded `MARC27_*` strings, so an
/// operator who set only `PRISM_API_KEY` got a CLI that authenticated
/// perfectly on every request path but reported "not configured" at boot and
/// never ran the marketplace tool sync — the neutral name worked everywhere
/// except the one check that decides whether the platform exists.
const PLATFORM_TOKEN_VARS: [PlatformVar; 3] = [
    PlatformVar::API_KEY,
    PlatformVar::TOKEN,
    PlatformVar::API_TOKEN,
];

/// One boot-banner line for a rejected credential: the platform's own
/// `error.code` plus the action that code implies.
async fn rejection_line(resp: reqwest::Response) -> String {
    let status = resp.status();
    let url = resp.url().to_string();
    let body = match resp.text().await {
        Ok(body) => body,
        Err(error) => {
            return format!("HTTP {} — response unreadable: {error}", status.as_u16());
        }
    };
    let error = PlatformError::parse(status, &url, &body);
    let reason = error
        .code
        .clone()
        .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
    let action = match error.action() {
        Some(action) if action.contains("prism login") => "not authenticated",
        Some(_) => "credential not permitted here",
        None => "see prism doctor",
    };
    format!("{reason} — {action}")
}

/// Whether this install has a platform credential at all.
///
/// PRISM works fully locally, so a user who never signed in has not
/// configured a platform — and startup must not reach out to a network
/// it was never pointed at. Before this check, every launch made an
/// unconditional HTTPS request to the hosted API and then rendered a red
/// `[--]` line about it, which is both a phone-home on a local-only tool
/// and a standing advert for a service the user declined.
pub fn platform_configured(creds: Option<&StoredCredentials>) -> bool {
    if creds.is_some_and(|c| !c.access_token.trim().is_empty()) {
        return true;
    }
    // `get()` already treats unset, empty and whitespace-only alike as
    // absent, which is the same rule `blank_env_key_is_not_a_credential`
    // pins below — so the explicit trim check the old list needed is gone,
    // not lost.
    PLATFORM_TOKEN_VARS.iter().any(|var| var.get().is_some())
}

/// Run the boot checks.
///
/// With no platform credential this touches the network only to probe the
/// local node on loopback. Otherwise each platform check times out after
/// 5s; failures are reported as `[--]` so the boot screen never hangs.
pub async fn run_boot_checks(
    creds: Option<&StoredCredentials>,
    endpoints: &PlatformEndpoints,
) -> Vec<boot::BootCheck> {
    run_boot_checks_with(creds, endpoints, platform_configured(creds)).await
}

/// The body of [`run_boot_checks`] with the configured/not-configured
/// decision passed in rather than read from the process environment.
///
/// The seam exists so the tests can pin either branch without mutating
/// `std::env`, which is process-global and races across parallel tests.
async fn run_boot_checks_with(
    creds: Option<&StoredCredentials>,
    endpoints: &PlatformEndpoints,
    configured: bool,
) -> Vec<boot::BootCheck> {
    let mut checks = Vec::new();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    if !configured {
        // Stated once, neutrally, and never as a failure: nothing is
        // broken about running PRISM without an account.
        checks.push(boot::BootCheck {
            name: "Platform".into(),
            result: "not configured — running local-only".into(),
            ok: true,
            dots: 8,
            delay_ms: 30,
        });
        push_local_checks(&client, &mut checks).await;
        return checks;
    }

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
        // We only reach here with a platform configured, so an empty
        // session token means the credential came from the environment
        // (the headless/agent path). Telling that user to "run prism
        // login" was a lying check: they have a working credential and
        // deliberately did not want a session.
        checks.push(boot::BootCheck {
            name: "Auth".into(),
            result: "API key from environment (no session)".into(),
            ok: true,
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

    push_local_checks(&client, &mut checks).await;

    checks
}

/// The checks that need no account and no internet: the local node daemon
/// on loopback and the in-process policy engine. Appended by both the
/// configured and the local-only path.
async fn push_local_checks(client: &reqwest::Client, checks: &mut Vec<boot::BootCheck>) {
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
            "offline — node not started".into()
        },
        ok: node_ok,
        dots: 4,
        delay_ms: 20,
    });

    checks.push(boot::BootCheck {
        name: "Policy Engine".into(),
        result: "OPA/Rego loaded".into(),
        ok: true,
        dots: 4,
        delay_ms: 15,
    });
}

/// `set_var`/`remove_var` are process-global; serialize every env-touching
/// test through this guard. `pub(crate)` so tests outside this module that
/// exercise [`platform_configured`] share the SAME lock — two private locks
/// would not serialize against each other, and both would be clearing the
/// same three variables.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Remove every platform token env var, so a test can pin the
/// no-credential branch regardless of the developer's shell.
///
/// BOTH spellings, deliberately: this is a test-isolation primitive, and
/// clearing only the historical name would let a `PRISM_API_KEY` sitting in
/// the developer's shell leak in and flip `platform_configured` — a flake
/// that reproduces on one machine and nowhere else.
#[cfg(test)]
pub(crate) fn clear_platform_env() {
    for var in PLATFORM_TOKEN_VARS {
        unsafe {
            std::env::remove_var(var.preferred);
            std::env::remove_var(var.alias);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds_with(token: &str) -> StoredCredentials {
        StoredCredentials {
            access_token: token.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn no_credential_anywhere_means_not_configured() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_env();
        assert!(!platform_configured(None));
    }

    /// A credentials file left behind by a logout — or written empty —
    /// is not a credential. Treating it as one is what made a signed-out
    /// user still phone home on every launch.
    #[test]
    fn blank_stored_token_is_not_a_credential() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_env();
        assert!(!platform_configured(Some(&creds_with("   "))));
    }

    #[test]
    fn stored_token_means_configured() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_env();
        assert!(platform_configured(Some(&creds_with("token-abc"))));
    }

    /// The headless path never runs `prism login`, so the env key alone
    /// has to count — otherwise CI/agent installs would skip the very
    /// checks they need.
    /// Both spellings of every credential must count. The neutral name is
    /// the one that used to be missed: `platform_configured` checked three
    /// hardcoded `MARC27_*` strings, so a `PRISM_API_KEY`-only operator got
    /// a CLI that authenticated on every request path while the boot screen
    /// said "not configured" and the tool sync never ran.
    #[test]
    fn env_key_alone_means_configured() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        for var in PLATFORM_TOKEN_VARS {
            for key in [var.preferred, var.alias] {
                clear_platform_env();
                unsafe { std::env::set_var(key, "m27_test") };
                assert!(platform_configured(None), "{key} should count");
            }
        }
        clear_platform_env();
    }

    #[test]
    fn blank_env_key_is_not_a_credential() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_env();
        unsafe { std::env::set_var("MARC27_API_KEY", "  ") };
        let configured = platform_configured(None);
        clear_platform_env();
        assert!(!configured);
    }

    /// The regression guard for the phone-home: with nothing configured,
    /// the boot screen must contain only local checks — no auth line, no
    /// knowledge-graph line, nothing that implies a missing account.
    #[tokio::test]
    async fn unconfigured_boot_runs_local_checks_only() {
        // A URL that would fail loudly (and slowly) if it were ever hit.
        let endpoints = PlatformEndpoints {
            api_base: "https://platform.invalid/api/v1".to_string(),
            node_ws: "wss://platform.invalid/api/v1/nodes/connect".to_string(),
        };
        let checks = run_boot_checks_with(None, &endpoints, false).await;

        let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Platform", "Local Node", "Policy Engine"]);
        let platform = &checks[0];
        assert!(
            platform.ok,
            "an unconfigured platform is a choice, not a failure"
        );
        assert!(
            !platform.result.contains("login"),
            "the boot screen must not nag about signing in: {:?}",
            platform.result
        );
    }

    /// The headless path has a real credential and deliberately no
    /// session — telling it to `prism login` is a lying check.
    #[tokio::test]
    async fn env_key_path_is_not_told_to_log_in() {
        // Configured (the env key is present) but with no stored session
        // — exactly the headless/agent shape.
        let endpoints = PlatformEndpoints {
            api_base: "http://127.0.0.1:1/api/v1".to_string(),
            node_ws: "ws://127.0.0.1:1/api/v1/nodes/connect".to_string(),
        };
        let checks = run_boot_checks_with(None, &endpoints, true).await;

        let auth = checks
            .iter()
            .find(|c| c.name == "Auth")
            .expect("a configured platform still reports Auth");
        assert!(
            !auth.result.contains("prism login"),
            "env-key installs have no session to log in to: {:?}",
            auth.result
        );
        assert!(auth.ok, "a working API key is not an auth failure");
    }
}
