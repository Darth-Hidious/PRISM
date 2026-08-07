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
use prism_runtime::auth::PlatformAuth;
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
    // Hard offline mode is a policy about the process, not about one command.
    // `Commands::Tui` skipped the boot checks itself (main.rs), but `Setup` and
    // `Resume` called straight through — so `PRISM_OFFLINE=1 prism setup` ran
    // every platform check anyway. Enforcing it here covers all eight call
    // sites at once instead of relying on each to remember.
    //
    // This matters more since boot checks started carrying a real credential:
    // before, the offline bypass leaked an empty Bearer; now it would send the
    // operator's actual key to a remote host they explicitly asked not to
    // contact.
    if prism_runtime::offline::enabled() {
        return offline_checks().await;
    }
    let configured = platform_configured(creds);
    let credential = boot_credential(creds);
    run_boot_checks_with(creds, endpoints, configured, credential).await
}

/// The check set for hard offline mode: local only, and it says why.
async fn offline_checks() -> Vec<boot::BootCheck> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default();
    let mut checks = vec![boot::BootCheck {
        name: "Platform".into(),
        // Not a failure: the operator asked for this.
        result: "offline mode — platform checks skipped".into(),
        ok: true,
        dots: 8,
        delay_ms: 30,
    }];
    push_local_checks(&client, &mut checks).await;
    checks
}

/// The credential the boot checks should present, in the same precedence the
/// real resolver uses (`auth::resolve_platform_auth`): stored session first,
/// then the environment.
///
/// Before this existed the checks derived their header from `creds` alone and
/// sent `Bearer ` — an EMPTY bearer — whenever the only credential was an env
/// var. `platform_configured` said "configured", step 1 fired anyway, the
/// platform answered 401, and the row read "<host> unreachable". The host was
/// fine; we simply never sent a credential. That is a lying check, and it hit
/// exactly the headless/agent install this module documents as supported.
fn boot_credential(creds: Option<&StoredCredentials>) -> Option<PlatformAuth> {
    if let Some(token) = creds
        .map(|c| c.access_token.trim())
        .filter(|t| !t.is_empty())
    {
        return Some(PlatformAuth::Bearer(token.to_string()));
    }
    // An API key is a distinct wire shape (`X-API-Key`), so classify rather
    // than assuming Bearer. `PlatformAuth::classify` is the same rule the
    // resolver applies; it is not re-derived here.
    if let Some(key) = PlatformVar::API_KEY.get() {
        return Some(PlatformAuth::classify(&key));
    }
    PlatformVar::TOKEN
        .get()
        .or_else(|| PlatformVar::API_TOKEN.get())
        .map(|value| PlatformAuth::classify(&value))
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
    credential: Option<PlatformAuth>,
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

    // A stored session still drives the "is this a session or an env key?"
    // distinction in the Auth row below; `credential` drives the wire header.
    let session_token = creds.map(|c| c.access_token.trim()).unwrap_or("");
    let api = &endpoints.api_base;

    // 1. Platform connection — use /agent/capabilities (always 200 with auth)
    let Some(credential) = credential else {
        // Configured, but nothing to authenticate with. Say that, rather than
        // firing an unauthenticated request and blaming the host for the 401.
        checks.push(boot::BootCheck {
            name: "Platform".into(),
            result: "configured, but no usable credential — checks skipped".into(),
            ok: false,
            dots: 8,
            delay_ms: 30,
        });
        push_local_checks(&client, &mut checks).await;
        return checks;
    };
    let platform_ok = credential
        .apply(client.get(format!("{api}/agent/capabilities")))
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
    //    Only a stored SESSION can expire, so this branch stays keyed on the
    //    session token; an env key takes the placeholder row below.
    if !session_token.is_empty() {
        let user_resp = credential
            .apply(client.get(format!("{api}/users/me")))
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
    {
        let stats = credential
            .apply(client.get(format!("{api}/knowledge/graph/stats")))
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
    {
        let project_id = creds.and_then(|c| c.project_id.as_deref()).unwrap_or("");
        if !project_id.is_empty() {
            let models = credential
                .apply(client.get(format!("{api}/projects/{project_id}/llm/models")))
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
    {
        let gpus = credential
            .apply(client.get(format!("{api}/compute/gpus")))
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
    {
        let mkt = credential
            .apply(client.get(format!("{api}/marketplace/resources")))
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

    /// Hard offline mode must skip every platform check, from ANY command.
    /// `Commands::Setup` and `Commands::Resume` had no `cli.offline` guard, so
    /// before this the boot checks ran under `PRISM_OFFLINE=1` and — once they
    /// started carrying a real credential — would have sent it to a remote host
    /// the operator explicitly asked not to contact.
    #[tokio::test]
    async fn offline_mode_skips_every_platform_check() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_env();
        // A credential IS present: offline must win over "configured".
        unsafe {
            std::env::set_var("PRISM_API_KEY", "m27_real");
            std::env::set_var(prism_runtime::offline::ENV, "1");
        }
        // An address that would cost a full 5s timeout if it were ever dialled.
        let endpoints = PlatformEndpoints {
            api_base: "http://127.0.0.1:1/api/v1".to_string(),
            node_ws: "ws://127.0.0.1:1/api/v1/nodes/connect".to_string(),
        };
        let checks = run_boot_checks(None, &endpoints).await;
        unsafe { std::env::remove_var(prism_runtime::offline::ENV) };
        clear_platform_env();

        let platform = checks
            .iter()
            .find(|c| c.name == "Platform")
            .expect("offline still reports a Platform row");
        assert!(
            platform.result.contains("offline mode"),
            "must say why it skipped: {}",
            platform.result
        );
        assert!(platform.ok, "offline is a choice, not a failure");
        // None of the credentialed steps may appear.
        for banned in [
            "Auth",
            "Knowledge Graph",
            "LLM Models",
            "Compute",
            "Marketplace",
        ] {
            assert!(
                !checks.iter().any(|c| c.name == banned),
                "{banned} row must not exist in offline mode"
            );
        }
    }

    /// The lying check this change exists to kill: "configured" but with no
    /// usable credential must NOT fire an unauthenticated request and then
    /// report the host as unreachable. The host is fine; we had nothing to
    /// send.
    #[tokio::test]
    async fn configured_without_a_credential_says_so_instead_of_blaming_the_host() {
        let endpoints = PlatformEndpoints {
            api_base: "http://127.0.0.1:1/api/v1".to_string(),
            node_ws: "ws://127.0.0.1:1/api/v1/nodes/connect".to_string(),
        };
        let checks = run_boot_checks_with(None, &endpoints, true, None).await;
        let platform = checks
            .iter()
            .find(|c| c.name == "Platform")
            .expect("a configured platform always reports a Platform row");
        assert!(
            platform.result.contains("no usable credential"),
            "must name the real condition: {}",
            platform.result
        );
        assert!(
            !platform.result.contains("unreachable"),
            "never blame the host for a credential we did not send: {}",
            platform.result
        );
    }

    /// A stored session outranks the environment, and an `m27_` key is an
    /// API key (X-API-Key), not a Bearer token. Getting the second one wrong
    /// sends the key on the wrong header and every platform check 401s.
    #[test]
    fn boot_credential_prefers_the_session_then_classifies_the_env_key() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_env();

        // Session wins over a present env key.
        unsafe { std::env::set_var("PRISM_API_KEY", "m27_env") };
        assert_eq!(
            boot_credential(Some(&creds_with("session-jwt"))),
            Some(PlatformAuth::Bearer("session-jwt".to_string()))
        );

        // No session: the env key is classified by shape, not assumed Bearer.
        assert_eq!(
            boot_credential(None),
            Some(PlatformAuth::ApiKey("m27_env".to_string()))
        );

        // A non-m27 value under the token name is a rotating credential.
        clear_platform_env();
        unsafe { std::env::set_var("MARC27_TOKEN", "jwt-shaped") };
        assert_eq!(
            boot_credential(None),
            Some(PlatformAuth::Bearer("jwt-shaped".to_string()))
        );

        clear_platform_env();
        assert_eq!(boot_credential(None), None);
    }

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
        let checks = run_boot_checks_with(None, &endpoints, false, None).await;

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
        let checks = run_boot_checks_with(
            None,
            &endpoints,
            true,
            Some(PlatformAuth::ApiKey("m27_test".into())),
        )
        .await;

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
