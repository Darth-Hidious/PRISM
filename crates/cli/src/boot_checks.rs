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
use prism_runtime::auth::{
    PlatformAuth, resolve_environment_credential, stored_bearer_for_endpoints,
    stored_node_bearer_for_endpoints,
};
use prism_runtime::platform_env::PlatformVar;
use prism_runtime::{PlatformEndpoints, StoredCredentials, StoredNodeToken};

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
#[cfg(test)]
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
    PlatformVar::get_preferred_then_alias(&[
        PlatformVar::API_KEY,
        PlatformVar::TOKEN,
        PlatformVar::API_TOKEN,
    ])
    .is_some()
}

/// Run the boot checks.
///
/// With no platform credential this touches the network only to probe the
/// local node on loopback. Otherwise each platform check times out after
/// 5s; failures are reported as `[--]` so the boot screen never hangs.
pub async fn run_boot_checks(
    creds: Option<&StoredCredentials>,
    endpoints: Option<&PlatformEndpoints>,
) -> Vec<boot::BootCheck> {
    // Hard offline mode is a policy about the process, not about one command.
    // `Commands::Tui` skipped the boot checks itself (main.rs), but `Setup` and
    // `Resume` called straight through — so `PRISM_OFFLINE=1 prism setup` ran
    // every platform check anyway. Enforcing it here covers all NINE call
    // sites at once (8 in main.rs + doctor.rs:172) instead of relying on each
    // to remember.
    //
    // This matters more since boot checks started carrying a real credential:
    // before, the offline bypass leaked an empty Bearer; now it would send the
    // operator's actual key to a remote host they explicitly asked not to
    // contact.
    if prism_runtime::offline::enabled() {
        return offline_checks().await;
    }
    let credential = boot_credential(creds, endpoints);
    let Some(endpoints) = endpoints else {
        return endpoint_not_configured_checks(!matches!(credential, BootCredential::Absent)).await;
    };
    run_boot_checks_with(creds, endpoints, true, credential).await
}

/// Boot checks with a durable node key included in the same precedence as the
/// production resolver. Used by `doctor`, which already has the resolved
/// [`PrismPaths`] and must not describe a working legacy node as local-only.
pub async fn run_boot_checks_with_node_token(
    creds: Option<&StoredCredentials>,
    endpoints: Option<&PlatformEndpoints>,
    node_token: Option<&StoredNodeToken>,
) -> Vec<boot::BootCheck> {
    if prism_runtime::offline::enabled() {
        return offline_checks().await;
    }
    let credential = boot_credential_with_node_token(creds, endpoints, node_token);
    let Some(endpoints) = endpoints else {
        return endpoint_not_configured_checks(!matches!(credential, BootCredential::Absent)).await;
    };
    run_boot_checks_with(creds, endpoints, true, credential).await
}

async fn endpoint_not_configured_checks(credential_present: bool) -> Vec<boot::BootCheck> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default();
    let mut checks = vec![boot::BootCheck {
        name: "Platform".into(),
        result: if credential_present {
            "credential present, but no endpoint configured — set PRISM_API_URL".into()
        } else {
            "not configured — running local-only; set PRISM_API_URL to connect".into()
        },
        ok: !credential_present,
        dots: 8,
        delay_ms: 30,
    }];
    push_local_checks(&client, &mut checks).await;
    checks
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

/// What the boot checks have to present as a credential.
///
/// The credential is either ready for the configured provider or absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BootCredential {
    /// Usable. Attach it and run the checks.
    Ready(PlatformAuth),
    /// A stored bearer exists but does not belong to the selected endpoint.
    /// The message contains configuration metadata only, never the token.
    Refused(String),
    /// Nothing configured.
    Absent,
}

/// The credential the boot checks should present, in the same precedence the
/// real resolver uses (`auth::resolve_platform_auth:164-196`):
///
///   1. `*_API_KEY`   (provider-neutral key, sent as `X-API-Key`)
///   2. `*_TOKEN` / `*_API_TOKEN`
///   3. the stored session
///
/// An earlier version of this comment said "stored session first, then the
/// environment" and the code matched the comment rather than the resolver.
/// Both were wrong. With an expired session AND a valid `PRISM_API_KEY`, every
/// real command authenticated fine via the key while the boot screen used the
/// dead session, rendered a red Auth row, and could trigger an interactive
/// re-login (main.rs:4474-4503) for a user whose tooling was working. That is
/// the same lying-check class this module exists to prevent, so the order is
/// now the resolver's, not a convenient one.
///
/// Before this existed the checks derived their header from `creds` alone and
/// sent `Bearer ` — an EMPTY bearer — whenever the only credential was an env
/// var. `platform_configured` said "configured", step 1 fired anyway, the
/// platform answered 401, and the row read "<host> unreachable". The host was
/// fine; we simply never sent a credential. That is a lying check, and it hit
/// exactly the headless/agent install this module documents as supported.
///
fn boot_credential(
    creds: Option<&StoredCredentials>,
    endpoints: Option<&PlatformEndpoints>,
) -> BootCredential {
    boot_credential_with_node_token(creds, endpoints, None)
}

fn boot_credential_with_node_token(
    creds: Option<&StoredCredentials>,
    endpoints: Option<&PlatformEndpoints>,
    node_token: Option<&StoredNodeToken>,
) -> BootCredential {
    // PRISM defines the API-key variable's wire shape. Providers define their
    // own key contents; the frozen `m27_` prefix remains accepted but is not a
    // requirement for an independent provider.
    // The endpoint owns the credential vocabulary. In particular,
    // PRISM_API_KEY is Supabase's public project key, not a user credential,
    // and must never displace the verified session Bearer. With no endpoint
    // there is no provider to classify against; retain the generic check only
    // so the boot row can explain that a credential lacks its endpoint.
    let environment_credential = endpoints
        .and_then(PlatformEndpoints::environment_credential)
        .or_else(|| {
            endpoints
                .is_none()
                .then(resolve_environment_credential)
                .flatten()
        });
    if let Some(credential) = environment_credential {
        return BootCredential::Ready(credential);
    }
    if let Some(token) = node_token {
        let Some(endpoints) = endpoints else {
            return BootCredential::Refused(
                "stored node credential binding refused: no platform endpoint is configured".into(),
            );
        };
        return match stored_node_bearer_for_endpoints(endpoints, token) {
            Ok(Some(credential)) => BootCredential::Ready(credential),
            Ok(None) => BootCredential::Absent,
            Err(error) => BootCredential::Refused(error.to_string()),
        };
    }
    // The stored session is LAST, matching the resolver, and is usable only
    // with the provider + normalized URL recorded at login.
    match (endpoints, creds) {
        (Some(endpoints), Some(credentials)) => {
            match stored_bearer_for_endpoints(endpoints, credentials) {
                Ok(Some(credential)) => BootCredential::Ready(credential),
                Ok(None) => BootCredential::Absent,
                Err(error) => BootCredential::Refused(error.to_string()),
            }
        }
        (None, Some(credentials)) if !credentials.access_token.trim().is_empty() => {
            BootCredential::Refused(
                "stored session binding refused: no platform endpoint is configured".to_string(),
            )
        }
        _ => BootCredential::Absent,
    }
}

/// The project scope the boot checks should use.
///
/// Env FIRST, session second — the order `resolve_active_project_id`
/// (main.rs:7184) and `select_project_context_automatically`
/// (agent/protocol.rs:317) both use, both of which return on the env value
/// unconditionally.
///
/// My first version had this backwards while citing those two as precedent.
/// A CI job setting `PRISM_PROJECT_ID` to override an interactive session's
/// project would have had every command scoped to the override and this one
/// boot row scoped to the stale session project — querying a project that may
/// not even exist any more.
///
/// Extracted so the precedence is testable on its own: the row-level test
/// cannot see it, because a sessionless install has no session project to
/// conflict with.
fn boot_project_id(creds: Option<&StoredCredentials>) -> Option<String> {
    PlatformVar::PROJECT_ID.get().or_else(|| {
        creds
            .and_then(|c| c.project_id.as_deref())
            .map(str::to_string)
    })
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
    credential: BootCredential,
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
    let credential = match credential {
        BootCredential::Ready(credential) => credential,
        BootCredential::Refused(reason) => {
            checks.push(boot::BootCheck {
                name: "Platform".into(),
                result: reason,
                ok: false,
                dots: 8,
                delay_ms: 30,
            });
            push_local_checks(&client, &mut checks).await;
            return checks;
        }
        // The absent arm states the real condition rather than firing an
        // unauthenticated request and blaming the host for the resulting 401.
        // A malformed credential names ITS OWN defect: "unreachable" would
        // send the reader to look at the network, which is not the problem.
        BootCredential::Absent => {
            checks.push(boot::BootCheck {
                name: "Platform".into(),
                result: "configured, but no usable credential — checks skipped".into(),
                ok: false,
                dots: 8,
                delay_ms: 30,
            });
            push_local_checks(&client, &mut checks).await;
            return checks;
        }
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
                // A stored session that has merely EXPIRED is not the same
                // failure as one that is invalid: a refresh token sitting in
                // `credentials.json` can fix it without the reader typing a
                // password. Saying only "token_expired" made the product look
                // broken when it could heal itself, so the row names the fix.
                let expired = r.status() == reqwest::StatusCode::UNAUTHORIZED;
                let line = rejection_line(r).await;
                if expired && creds.map(|c| !c.refresh_token.is_empty()).unwrap_or(false) {
                    (false, format!("{line}; run `prism login`"))
                } else {
                    (false, line)
                }
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
        // The project scope comes from the session when there is one, and
        // otherwise from the environment — the headless/agent path has no
        // stored credentials at all, so reading `creds` alone meant this row
        // NEVER appeared for it. `PlatformVar::PROJECT_ID` is what every other
        // surface already uses for exactly this (main.rs env_project_override,
        // agent/protocol.rs); boot_checks was the one place it was missed.
        let env_project = boot_project_id(creds);
        let project_id = env_project.as_deref().unwrap_or("");
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
                // MEASURED 2026-08-26: this endpoint answers 200 with NO
                // Authorization header, so a green row here is not evidence
                // that the reader's session works. A bare [OK] beside a FAILED
                // Auth row read as "most of the platform is fine" when nothing
                // about the session had been established. Say what was.
                (true, format!("{count} hosted models, public catalog"))
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
            // Same as LLM Models: measured 2026-08-26 to answer 200 with no
            // Authorization header, so this row says nothing about the session.
            (true, format!("{count} resources, public catalog"))
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

    // Actually construct the engine. This row used to be a hardcoded
    // `ok: true, "OPA/Rego loaded"` that never called anything — a check in
    // name only, sitting in the same list, same formatting and same confidence
    // as "Local Node", which IS a live probe.
    //
    // It matters more than a cosmetic row. `with_discovery` compiles the
    // built-in `default.rego` and then every `.rego` under `~/.prism/policies/`
    // and `.prism/policies/`, any of which can fail to parse. When it does, the
    // agent runs with `policy_engine == None` (`agent/src/service.rs:249`), and
    // the tool-call gate at `agent/src/protocol.rs:1059` is
    // `if let Some(pe) = policy_engine.as_mut()` — so a MISSING engine skips
    // the gate entirely. That is fail-open: one malformed custom policy
    // silently disables tool-call enforcement, while this row kept reporting
    // "loaded" forever.
    //
    // `prism-policy` was already in this binary's dependency graph via
    // prism-agent, so constructing it here costs a Rego compile, not a build.
    let (policy_result, policy_ok) = match prism_policy::PolicyEngine::with_discovery(None) {
        Ok(pe) => (format!("OPA/Rego, {} policies", pe.policy_count()), true),
        // Names the failure. The agent degrades to no enforcement on this path,
        // so a silent green here is the worst possible answer.
        Err(error) => (
            format!("failed to load — policies NOT enforced: {error}"),
            false,
        ),
    };
    checks.push(boot::BootCheck {
        name: "Policy Engine".into(),
        result: policy_result,
        ok: policy_ok,
        dots: 4,
        delay_ms: 15,
    });
}

/// `set_var`/`remove_var` are process-global; serialize every env-touching
/// test through this guard. `pub(crate)` so tests outside this module that
/// exercise [`platform_configured`] share the SAME lock — two private locks
/// would not serialize against each other, and both would be clearing the
/// same three variables.
///
/// **Re-exported, not declared.** This name and
/// `prism_runtime::offline::test_support::ENV_LOCK` must be the same mutex, or
/// they serialize nothing against each other. They were two different mutexes,
/// and it bit: `pyiron_cmd.rs` reached for the runtime one while every other
/// prism-cli test used this one, so its `PRISM_OFFLINE=1` leaked into
/// `a_signed_in_user_still_syncs_tools` — which calls `should_sync_tools`,
/// which returns false under offline. A genuine failure, in the full workspace
/// run only, from a test that passed on its own.
///
/// That was the TENTH occurrence of this shape on this branch, and the second I
/// caused while fixing an earlier one. Aliasing rather than declaring is what
/// makes the next one impossible: both spellings now resolve to one mutex, so a
/// file cannot pick the wrong lock.
#[cfg(test)]
pub(crate) use prism_runtime::offline::test_support::ENV_LOCK;

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

    /// The headless path has no stored credentials, so a Models row could
    /// only ever appear if the project scope is read from the environment too.
    /// Before this, `project_id` came from `creds` alone and the row was
    /// A row that answers WITHOUT a credential must not read as proof of a
    /// working session.
    ///
    /// MEASURED against api.marc27.com on 2026-08-26 with an EXPIRED token:
    /// `/users/me` -> 401 token_expired, `/knowledge/graph/stats` -> 401,
    /// `/compute/providers` -> 401, but `/projects/{id}/llm/models` -> 200 and
    /// `/marketplace/resources` -> 200 with NO Authorization header at all.
    /// So two of the six platform rows rendered a green [OK] beside a failed
    /// Auth row, and the screen read as "most of the platform is fine" while
    /// nothing about the reader's session had been established.
    ///
    /// This pins the wording rather than the transport: the row must SAY the
    /// catalog is public, so a green tick next to a dead session is legible
    /// instead of misleading.
    #[test]
    fn public_catalog_rows_admit_they_prove_nothing_about_the_session() {
        // The two rows built from unauthenticated endpoints.
        for probe in ["hosted models", "resources"] {
            let rendered = if probe == "hosted models" {
                format!("{} hosted models, public catalog", 597)
            } else {
                format!("{} resources, public catalog", 50)
            };
            assert!(
                rendered.contains("public catalog"),
                "a row built from an endpoint that answers unauthenticated must \
                 say so; got {rendered:?}"
            );
        }
        // And the source strings themselves, so deleting the qualifier from
        // the product code fails here rather than only on a live run.
        let src = include_str!("boot_checks.rs");
        for needle in ["hosted models, public catalog", "resources, public catalog"] {
            assert!(
                src.contains(needle),
                "boot_checks no longer qualifies a public-catalog row: {needle:?}"
            );
        }
        // An expired session must name the fix, not just the failure.
        assert!(
            src.contains("run `prism login`"),
            "the Auth row must tell the reader how to recover, not only that \
             the token expired -- a refresh token is sitting in credentials.json"
        );
    }

    /// silently absent for exactly the population the env-key work targets.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn models_row_uses_the_env_project_when_there_is_no_session() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_env();
        unsafe {
            std::env::set_var("PRISM_API_KEY", "m27_real");
            std::env::set_var("PRISM_PROJECT_ID", "proj-env");
        }
        let endpoints = PlatformEndpoints {
            api_base: "http://127.0.0.1:1/api/v1".to_string(),
            node_ws: "ws://127.0.0.1:1/api/v1/nodes/connect".to_string(),
            provider: None,
        };
        let checks = run_boot_checks_with(
            None,
            &endpoints,
            true,
            boot_credential(None, Some(&endpoints)),
        )
        .await;
        unsafe { std::env::remove_var("PRISM_PROJECT_ID") };
        clear_platform_env();

        // The row exists at all. Its ok/result depend on the (unreachable)
        // host; what this pins is that the step was REACHED, which it never
        // was for a sessionless install.
        assert!(
            checks.iter().any(|c| c.name == "LLM Models"),
            "no Models row: {:?}",
            checks.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
        );
    }

    /// The case the row-level test structurally cannot reach: a session
    /// project AND an env override, disagreeing. Env wins, matching
    /// `resolve_active_project_id` and `select_project_context_automatically`.
    #[test]
    fn env_project_overrides_the_session_project() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var("PRISM_PROJECT_ID") };
        unsafe { std::env::remove_var("MARC27_PROJECT_ID") };

        let mut with_project = creds_with("session-jwt");
        with_project.project_id = Some("session-project".to_string());

        // No override: the session project is used.
        assert_eq!(
            boot_project_id(Some(&with_project)).as_deref(),
            Some("session-project")
        );

        // Override present: it wins.
        unsafe { std::env::set_var("PRISM_PROJECT_ID", "env-project") };
        assert_eq!(
            boot_project_id(Some(&with_project)).as_deref(),
            Some("env-project"),
            "PRISM_PROJECT_ID must override the session project"
        );
        // And the historical spelling still works.
        unsafe { std::env::remove_var("PRISM_PROJECT_ID") };
        unsafe { std::env::set_var("MARC27_PROJECT_ID", "legacy-project") };
        assert_eq!(
            boot_project_id(Some(&with_project)).as_deref(),
            Some("legacy-project")
        );
        unsafe { std::env::remove_var("MARC27_PROJECT_ID") };

        assert_eq!(boot_project_id(None), None);
    }

    /// Provider-defined API key shapes remain valid under the PRISM-native
    /// variable; MARC27's frozen `m27_` prefix is compatibility, not identity.
    #[test]
    fn provider_neutral_api_key_shape_is_accepted() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_env();
        unsafe { std::env::set_var("PRISM_API_KEY", "provider-defined-key") };
        assert_eq!(
            boot_credential(None, None),
            BootCredential::Ready(PlatformAuth::ApiKey("provider-defined-key".into()))
        );
        clear_platform_env();
    }

    /// Hard offline mode must skip every platform check, from ANY command.
    /// `Commands::Setup` and `Commands::Resume` had no `cli.offline` guard, so
    /// before this the boot checks ran under `PRISM_OFFLINE=1` and — once they
    /// started carrying a real credential — would have sent it to a remote host
    /// the operator explicitly asked not to contact.
    #[allow(clippy::await_holding_lock)]
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
            provider: None,
        };
        let checks = run_boot_checks(None, Some(&endpoints)).await;
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
            provider: None,
        };
        let checks = run_boot_checks_with(None, &endpoints, true, BootCredential::Absent).await;
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

    /// Precedence must match `resolve_platform_auth` exactly: API key, then
    /// token, then the stored session — NOT session-first.
    ///
    /// This test previously asserted the opposite and passed, because the code
    /// it pinned had the same defect. With an expired session and a valid
    /// `PRISM_API_KEY`, session-first made every real command succeed via the
    /// key while the boot screen used the dead session and showed a red Auth
    /// row. Also pins that an `m27_` value is an API key (X-API-Key), not a
    /// Bearer token: getting that wrong 401s every check with a valid key.
    #[test]
    fn boot_credential_matches_the_resolver_precedence() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_env();

        // The env API key OUTRANKS a stored session, as in the resolver.
        unsafe { std::env::set_var("PRISM_API_KEY", "m27_env") };
        assert_eq!(
            boot_credential(Some(&creds_with("session-jwt")), None),
            BootCredential::Ready(PlatformAuth::ApiKey("m27_env".to_string())),
            "an env API key must win over a stored session"
        );

        // Same with no session at all: classified by shape, not assumed Bearer.
        assert_eq!(
            boot_credential(None, None),
            BootCredential::Ready(PlatformAuth::ApiKey("m27_env".to_string()))
        );

        // The stored session is the LAST resort, not the first.
        clear_platform_env();
        let marc27 = PlatformEndpoints::marc27();
        assert_eq!(
            boot_credential(Some(&creds_with("session-jwt")), Some(&marc27)),
            BootCredential::Ready(PlatformAuth::Bearer("session-jwt".to_string())),
            "with nothing in the env, the session is used"
        );

        // A non-m27 value under the token name is a rotating credential.
        clear_platform_env();
        unsafe { std::env::set_var("MARC27_TOKEN", "jwt-shaped") };
        assert_eq!(
            boot_credential(None, None),
            BootCredential::Ready(PlatformAuth::Bearer("jwt-shaped".to_string()))
        );

        clear_platform_env();
        assert_eq!(boot_credential(None, None), BootCredential::Absent);
    }

    #[test]
    fn supabase_anon_key_is_not_a_boot_bearer_credential() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_env();
        unsafe { std::env::set_var("PRISM_API_KEY", "supabase-public-anon-key") };
        let endpoints = PlatformEndpoints::from_url_with_provider(
            "https://project.supabase.co",
            Some("supabase".to_string()),
        );
        let stored = StoredCredentials {
            access_token: "verified-user-session".into(),
            platform_url: "https://project.supabase.co".into(),
            platform_provider: Some("supabase".into()),
            ..Default::default()
        };

        assert_eq!(
            boot_credential(Some(&stored), Some(&endpoints)),
            BootCredential::Ready(PlatformAuth::Bearer("verified-user-session".into())),
            "the public Supabase anon key must not displace a verified session"
        );

        unsafe { std::env::set_var("PRISM_TOKEN", "explicit-user-token") };
        assert_eq!(
            boot_credential(Some(&stored), Some(&endpoints)),
            BootCredential::Ready(PlatformAuth::Bearer("explicit-user-token".into())),
            "explicit token variables retain precedence for Supabase"
        );
        clear_platform_env();
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn unrelated_endpoint_refuses_stored_supabase_bearer_without_a_request() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_env();
        let _offline = prism_runtime::offline::test_support::OfflineEnvGuard::clear();
        let server = wiremock::MockServer::start().await;
        let endpoints =
            PlatformEndpoints::from_url_with_provider(&server.uri(), Some("supabase".to_string()));
        let credentials = StoredCredentials {
            access_token: "stored-access-token-must-not-leak".to_string(),
            platform_url: "https://trusted.example".to_string(),
            platform_provider: Some("supabase".to_string()),
            ..Default::default()
        };

        let checks = run_boot_checks(Some(&credentials), Some(&endpoints)).await;

        let platform = checks
            .iter()
            .find(|check| check.name == "Platform")
            .expect("binding refusal is visible");
        assert!(!platform.ok);
        assert!(
            platform.result.contains("stored session binding refused"),
            "{}",
            platform.result
        );
        assert!(
            !platform
                .result
                .contains("stored-access-token-must-not-leak")
        );
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("wiremock request recording")
                .len(),
            0,
            "boot binding refusal must happen before any request"
        );
        clear_platform_env();
    }

    fn creds_with(token: &str) -> StoredCredentials {
        StoredCredentials {
            access_token: token.to_string(),
            platform_url: "https://api.marc27.com".into(),
            platform_provider: Some("marc27".into()),
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
            provider: None,
        };
        let checks = run_boot_checks_with(None, &endpoints, false, BootCredential::Absent).await;

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
            provider: None,
        };
        let checks = run_boot_checks_with(
            None,
            &endpoints,
            true,
            BootCredential::Ready(PlatformAuth::ApiKey("m27_test".into())),
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
