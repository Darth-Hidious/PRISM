// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Shared NATIVE driver for PRISM frontends (the Ratatui TUI and the GPUI
//! desktop app). One in-process path onto [`prism_agent`]: no subprocess,
//! no stdio pipes, no JSON-RPC transport — request lines go in on a
//! channel, protocol values come out on a channel, using the exact same
//! message contract the stdio backend emits so frontends stay
//! transport-agnostic.

use std::path::Path;

use anyhow::{Context, Result};
use sha2::Digest;

#[derive(Default)]
struct RefreshAttemptRegistry {
    attempted: std::sync::Mutex<std::collections::HashSet<[u8; 32]>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshAttemptClaim {
    Granted,
    AlreadyAttempted,
    Unavailable,
}

impl RefreshAttemptRegistry {
    fn claim(&self, refresh_token: &str) -> RefreshAttemptClaim {
        let fingerprint: [u8; 32] = sha2::Sha256::digest(refresh_token.as_bytes()).into();
        let Ok(mut attempted) = self.attempted.lock() else {
            return RefreshAttemptClaim::Unavailable;
        };
        if attempted.insert(fingerprint) {
            RefreshAttemptClaim::Granted
        } else {
            RefreshAttemptClaim::AlreadyAttempted
        }
    }
}

static REFRESH_ATTEMPTS: std::sync::OnceLock<RefreshAttemptRegistry> = std::sync::OnceLock::new();

fn claim_refresh_attempt(refresh_token: &str) -> RefreshAttemptClaim {
    REFRESH_ATTEMPTS
        .get_or_init(RefreshAttemptRegistry::default)
        .claim(refresh_token)
}

#[derive(Clone, PartialEq, Eq)]
struct RefreshProviderConfig {
    adapter: prism_client::IdentityProviderAdapter,
    url: String,
    key: Option<String>,
}

impl std::fmt::Debug for RefreshProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshProviderConfig")
            .field("adapter", &self.adapter)
            .field("url", &self.url)
            .field("key", &self.key.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

fn resolve_refresh_provider(
    credentials: &prism_runtime::StoredCredentials,
) -> Result<RefreshProviderConfig> {
    let provider_name = credentials.platform_provider.as_deref().ok_or_else(|| {
        anyhow::anyhow!("credential refresh refused: identity provider is missing")
    })?;
    let adapter = prism_client::identity_provider_for(Some(provider_name)).ok_or_else(|| {
        anyhow::anyhow!(
            "credential refresh refused: unrecognised identity provider `{provider_name}`"
        )
    })?;

    match adapter {
        prism_client::IdentityProviderAdapter::Marc27 => {
            // A refresh token is bound to the provider URL stored at login.
            // Ordinary environment overrides must not redirect that secret to
            // a different host, even when the override is labelled MARC27.
            let stored_url = credentials.platform_url.trim();
            if stored_url.is_empty() {
                anyhow::bail!("credential refresh refused: stored MARC27 platform URL is missing");
            }
            let endpoints = prism_runtime::PlatformEndpoints::from_url_with_provider(
                stored_url,
                Some(prism_client::auth::MARC27_IDENTITY_PROVIDER.to_string()),
            );
            Ok(RefreshProviderConfig {
                adapter,
                url: endpoints.api_base,
                key: None,
            })
        }
        // Same shape, same handling: both bind the refresh to the issuer URL
        // and key stored WITH the credential, so an environment override
        // cannot redirect the refresh secret to another host. The provider
        // name in the error is taken from the adapter, not hard-coded, so a
        // Mirdyne failure never reports itself as Supabase.
        prism_client::IdentityProviderAdapter::Supabase
        | prism_client::IdentityProviderAdapter::Mirdyne => {
            let url = credentials
                .identity_provider_url
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} is not configured: missing identity provider project URL",
                        adapter.as_str()
                    )
                })?;
            let key = credentials
                .identity_provider_key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} is not configured: missing identity provider key",
                        adapter.as_str()
                    )
                })?;
            Ok(RefreshProviderConfig {
                adapter,
                url: url.to_owned(),
                key: Some(key.to_owned()),
            })
        }
    }
}

fn ensure_supabase_refresh_principal(
    credentials: &prism_runtime::StoredCredentials,
    issuer: &str,
    subject: &str,
) -> Result<String> {
    let refreshed_principal =
        prism_node::provider_roles::map_supabase_principal(issuer, subject)
            .ok_or_else(|| anyhow::anyhow!("Supabase refresh returned an invalid identity"))?;
    if credentials.user_id.as_deref() != Some(refreshed_principal.as_str()) {
        anyhow::bail!("Supabase refresh subject does not match the stored identity");
    }
    Ok(refreshed_principal)
}

/// A live in-process agent session.
pub struct NativeSession {
    tx: Option<std::sync::mpsc::Sender<String>>,
    rx: Option<std::sync::mpsc::Receiver<serde_json::Value>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NativeSession {
    /// Decompose into the raw channel ends. The session thread keeps
    /// running until the returned sender is dropped.
    pub fn into_parts(
        mut self,
    ) -> (
        std::sync::mpsc::Sender<String>,
        std::sync::mpsc::Receiver<serde_json::Value>,
    ) {
        self.thread = None;
        (
            self.tx.take().expect("session already decomposed"),
            self.rx.take().expect("session already decomposed"),
        )
    }
}

impl NativeSession {
    /// Send a JSON-RPC request line into the session.
    pub fn request(&self, method: &str, params: serde_json::Value) -> Result<()> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("native session decomposed"))?
            .send(serde_json::to_string(&req)?)
            .map_err(|_| anyhow::anyhow!("native session thread is gone"))?;
        Ok(())
    }

    pub fn init(&self) -> Result<()> {
        self.request(
            "init",
            serde_json::json!({"auto_approve": false, "resume": ""}),
        )
    }

    pub fn send_message(&self, text: &str) -> Result<()> {
        self.request("input.message", serde_json::json!({"text": text}))
    }

    pub fn send_approval(&self, response: &str, tool_name: &str) -> Result<()> {
        self.request(
            "input.prompt_response",
            serde_json::json!({"response": response, "tool_name": tool_name}),
        )
    }

    /// Closing the request channel ends the session loop; the thread
    /// then exits on its own.
    pub fn shutdown(&mut self) {
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for NativeSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Proactive credential refresh, mirroring the CLI startup policy:
/// signed-in with a refresh_token and within 5 minutes of expiry (or
/// already expired) → refresh through the credential's recorded identity
/// provider and persist the rotated pair to both stores (cli-state.json +
/// ~/.prism/credentials.json). Unknown providers fail closed before any
/// request is built. Each exact refresh token is attempted at most once per
/// process because providers may consume it even when local persistence fails.
/// A refresh failure leaves the old credential in place and the session
/// surfaces the honest 401.
pub fn ensure_fresh_credentials() {
    let paths = match prism_runtime::PrismPaths::discover() {
        Ok(p) => p,
        Err(_) => return,
    };
    let state = match paths.load_cli_state() {
        Ok(s) => s,
        Err(_) => return,
    };
    let creds = match state.credentials {
        Some(c) => c,
        None => return,
    };
    if creds.refresh_token.is_empty() {
        return;
    }
    let near_expiry = creds
        .expires_at
        .map(|e| e <= chrono::Utc::now() + chrono::Duration::minutes(5))
        .unwrap_or(false);
    if !near_expiry {
        return;
    }
    let provider = match resolve_refresh_provider(&creds) {
        Ok(provider) => provider,
        Err(error) => {
            tracing::warn!(%error, "credential refresh skipped");
            return;
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return,
    };
    match claim_refresh_attempt(&creds.refresh_token) {
        RefreshAttemptClaim::Granted => {}
        RefreshAttemptClaim::AlreadyAttempted => {
            tracing::warn!(
                provider = provider.adapter.as_str(),
                "credential refresh skipped because this refresh token was already attempted in this process; sign in again before starting another session"
            );
            return;
        }
        RefreshAttemptClaim::Unavailable => {
            tracing::warn!(
                provider = provider.adapter.as_str(),
                "credential refresh skipped because the one-attempt safety guard is unavailable; sign in again before starting another session"
            );
            return;
        }
    }
    let creds_clone = creds.clone();
    let result = rt.block_on(async {
        let client = prism_client::PlatformClient::new(&provider.url);
        let refreshed = provider
            .adapter
            .refresh_token(
                client.inner(),
                &provider.url,
                provider.key.as_deref(),
                &creds_clone.refresh_token,
            )
            .await?;
        if provider.adapter == prism_client::IdentityProviderAdapter::Supabase {
            let claims = refreshed.supabase_claims.as_ref().ok_or_else(|| {
                anyhow::anyhow!("verified Supabase refresh did not return identity claims")
            })?;
            let principal =
                ensure_supabase_refresh_principal(&creds_clone, &claims.iss, &claims.sub)?;
            std::fs::create_dir_all(&paths.state_dir)?;
            let engine = prism_core::rbac::RbacEngine::new(&paths.state_dir.join("rbac.db"))?;
            let role_sync = prism_node::provider_roles::sync_supabase_login_role(
                &engine,
                &claims.iss,
                &claims.sub,
                claims.role.as_deref().unwrap_or(""),
            )?;
            anyhow::ensure!(
                role_sync.principal_id == principal,
                "Supabase refresh role mapping changed the verified principal"
            );
        }
        let refreshed = refreshed.tokens;
        let mut new_creds = creds_clone.clone();
        new_creds.access_token = refreshed.access_token;
        new_creds.refresh_token = refreshed.refresh_token;
        new_creds.expires_at = refreshed.expires_in.and_then(|secs| {
            chrono::Utc::now().checked_add_signed(chrono::Duration::seconds(secs as i64))
        });
        paths.persist_credentials(&new_creds).context(
            "failed to commit the rotated credential pair; do not retry this refresh token, sign in again",
        )?;
        anyhow::Ok(())
    });
    match result {
        Ok(()) => tracing::info!(
            provider = provider.adapter.as_str(),
            "platform credential refreshed"
        ),
        Err(e) => tracing::warn!(
            provider = provider.adapter.as_str(),
            error = %e,
            "proactive credential refresh failed; sign in again before starting another session"
        ),
    }
}

/// Spawn the native agent session for `project_root`. Fails honestly when
/// the LLM endpoint cannot be resolved (not signed in and no local
/// endpoint configured).
pub fn spawn_native_session(project_root: &Path) -> Result<NativeSession> {
    spawn_native_session_with(project_root, None)
}

/// Spawn with an explicitly chosen chat target (provider picker path).
pub fn spawn_native_session_with(
    project_root: &Path,
    target: Option<prism_core::chat_config::ChatTarget>,
) -> Result<NativeSession> {
    ensure_fresh_credentials();
    let paths = prism_runtime::PrismPaths::discover()
        .map_err(|e| anyhow::anyhow!("prism paths unavailable: {e}"))?;
    let inputs = match target {
        Some(t) => prism_runtime::llm_resolve::native_session_inputs_with(
            project_root,
            prism_runtime::llm_resolve::resolve_python_bin(),
            &paths,
            Some(t),
        )?,
        None => prism_runtime::llm_resolve::native_session_inputs(
            project_root,
            prism_runtime::llm_resolve::resolve_python_bin(),
            &paths,
        )?,
    };
    let llm_config = prism_llm::LlmConfig {
        base_url: inputs.llm.base_url,
        model: inputs.llm.model,
        api_key: inputs.llm.api_key,
        credential_kind: inputs.llm.credential_kind.map(|kind| match kind {
            prism_runtime::llm_resolve::ResolvedCredentialKind::ApiKey => {
                prism_llm::LlmCredentialKind::ApiKey
            }
            prism_runtime::llm_resolve::ResolvedCredentialKind::Bearer => {
                prism_llm::LlmCredentialKind::Bearer
            }
        }),
        embedding_model: inputs.llm.embedding_model,
        context_window: inputs.llm.context_window,
        max_output_tokens: inputs.llm.max_output_tokens,
        ..Default::default()
    };
    let tool_server = prism_python_bridge::ToolServer {
        python_bin: inputs.python_bin,
        project_root: inputs.project_root,
        env: inputs.env,
    };
    let (in_tx, in_rx) = std::sync::mpsc::channel::<String>();
    let (out_tx, out_rx) = std::sync::mpsc::channel::<serde_json::Value>();
    let thread = std::thread::spawn(move || {
        let _ = prism_agent::protocol::run_server_native(llm_config, tool_server, in_rx, out_tx);
    });
    Ok(NativeSession {
        tx: Some(in_tx),
        rx: Some(out_rx),
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvironmentGuard {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvironmentGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(name);
            unsafe { std::env::set_var(name, value) };
            Self { name, previous }
        }
    }

    impl Drop for EnvironmentGuard {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(value) => std::env::set_var(self.name, value),
                    None => std::env::remove_var(self.name),
                }
            }
        }
    }

    fn stored_credentials(provider: Option<&str>) -> prism_runtime::StoredCredentials {
        prism_runtime::StoredCredentials {
            access_token: "access-token".to_string(),
            refresh_token: "refresh-token".to_string(),
            platform_url: "https://platform.example/api/v1".to_string(),
            platform_provider: provider.map(str::to_owned),
            identity_provider_url: Some("https://project.supabase.co".to_string()),
            identity_provider_key: Some("supabase-anon-key".to_string()),
            user_id: None,
            display_name: None,
            org_id: None,
            org_name: None,
            project_id: None,
            project_name: None,
            expires_at: None,
        }
    }

    fn unique_refresh_token_marker(label: &str) -> String {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!(
            "frontend-refresh-guard-test-{label}-{}-{sequence}",
            std::process::id()
        )
    }

    #[test]
    fn refresh_attempt_registry_blocks_the_same_token_after_one_attempt() {
        let token = unique_refresh_token_marker("same");

        assert_eq!(claim_refresh_attempt(&token), RefreshAttemptClaim::Granted);
        assert_eq!(
            claim_refresh_attempt(&token),
            RefreshAttemptClaim::AlreadyAttempted
        );
    }

    #[test]
    fn refresh_attempt_registry_allows_a_different_rotated_token() {
        let before_rotation = unique_refresh_token_marker("before-rotation");
        let after_rotation = unique_refresh_token_marker("after-rotation");

        assert_eq!(
            claim_refresh_attempt(&before_rotation),
            RefreshAttemptClaim::Granted
        );
        assert_eq!(
            claim_refresh_attempt(&after_rotation),
            RefreshAttemptClaim::Granted
        );
    }

    #[test]
    fn refresh_attempt_registry_fails_closed_when_its_lock_is_poisoned() {
        let registry = std::sync::Arc::new(RefreshAttemptRegistry::default());
        let poisoner = std::sync::Arc::clone(&registry);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.attempted.lock().expect("unpoisoned test lock");
            panic!("poison the refresh-attempt registry");
        })
        .join();

        assert_eq!(
            registry.claim("test-refresh-token-after-poison"),
            RefreshAttemptClaim::Unavailable
        );
    }

    #[test]
    fn supabase_refresh_uses_stored_identity_configuration() {
        let credentials = stored_credentials(Some("supabase"));

        let provider = resolve_refresh_provider(&credentials).expect("Supabase config");

        assert_eq!(
            provider.adapter,
            prism_client::IdentityProviderAdapter::Supabase
        );
        assert_eq!(provider.url, "https://project.supabase.co");
        assert_eq!(provider.key.as_deref(), Some("supabase-anon-key"));
    }

    #[test]
    fn marc27_refresh_ignores_an_unrelated_environment_endpoint() {
        let _lock = prism_runtime::offline::test_support::env_lock();
        let _environment =
            EnvironmentGuard::set("PRISM_API_URL", "https://unrelated-endpoint.example/api/v1");
        let credentials = stored_credentials(Some("marc27"));

        let provider = resolve_refresh_provider(&credentials).expect("stored MARC27 config");

        assert_eq!(
            provider.adapter,
            prism_client::IdentityProviderAdapter::Marc27
        );
        assert_eq!(provider.url, "https://platform.example/api/v1");
        assert_ne!(provider.url, "https://unrelated-endpoint.example/api/v1");
    }

    #[test]
    fn refresh_provider_debug_redacts_the_provider_key() {
        let provider = resolve_refresh_provider(&stored_credentials(Some("supabase")))
            .expect("Supabase config");

        let debug = format!("{provider:?}");
        assert!(debug.contains("[REDACTED]"), "{debug}");
        assert!(!debug.contains("supabase-anon-key"), "{debug}");
    }

    #[test]
    fn unknown_identity_provider_is_refused_before_refresh() {
        let credentials = stored_credentials(Some("not-a-provider"));

        let error = resolve_refresh_provider(&credentials)
            .expect_err("an unknown provider must fail closed")
            .to_string();

        assert!(error.contains("unrecognised identity provider"), "{error}");
        assert!(!error.contains("refresh-token"), "token leaked: {error}");
        assert!(!error.contains("supabase-anon-key"), "key leaked: {error}");
    }

    #[test]
    fn incomplete_supabase_configuration_is_refused() {
        let mut credentials = stored_credentials(Some("supabase"));
        credentials.identity_provider_key = None;

        let error = resolve_refresh_provider(&credentials)
            .expect_err("Supabase refresh requires its stored public key")
            .to_string();

        // Wording note: this used to assert "anon key". The message became
        // provider-neutral when Mirdyne joined the shared arm — "anon key" is
        // Supabase's own term and would be wrong for another issuer. What the
        // test actually guards is unchanged: an incomplete configuration is
        // REFUSED, and the error names WHICH provider was misconfigured.
        assert!(error.contains("missing identity provider key"), "{error}");
        assert!(
            error.contains("supabase"),
            "the error must name the provider that is misconfigured: {error}"
        );
    }

    /// The same refusal, for Mirdyne, through the same shared arm — proving
    /// the arm reports the provider it was actually given rather than the one
    /// whose code path it borrows.
    #[test]
    fn incomplete_mirdyne_configuration_is_refused_under_its_own_name() {
        let mut credentials = stored_credentials(Some("mirdyne"));
        credentials.identity_provider_key = None;

        let error = resolve_refresh_provider(&credentials)
            .expect_err("Mirdyne refresh requires its stored key")
            .to_string();

        assert!(error.contains("missing identity provider key"), "{error}");
        assert!(
            error.contains("mirdyne"),
            "a Mirdyne failure must not report itself as Supabase: {error}"
        );
    }

    #[test]
    fn verified_supabase_refresh_requires_the_stored_principal() {
        let mut credentials = stored_credentials(Some("supabase"));
        let issuer = "https://project.supabase.co/auth/v1";
        let subject = "user-123";
        let principal = prism_node::provider_roles::map_supabase_principal(issuer, subject)
            .expect("canonical principal");
        credentials.user_id = Some(principal.clone());

        assert_eq!(
            ensure_supabase_refresh_principal(&credentials, issuer, subject)
                .expect("same verified identity"),
            principal
        );
    }

    #[test]
    fn verified_supabase_refresh_rejects_subject_switching() {
        let mut credentials = stored_credentials(Some("supabase"));
        let issuer = "https://project.supabase.co/auth/v1";
        credentials.user_id = prism_node::provider_roles::map_supabase_principal(issuer, "user-a");

        let error = ensure_supabase_refresh_principal(&credentials, issuer, "user-b")
            .expect_err("a rotated session must retain its canonical subject")
            .to_string();

        assert!(
            error.contains("does not match the stored identity"),
            "{error}"
        );
        assert!(!error.contains("user-a"), "stored subject leaked: {error}");
        assert!(
            !error.contains("user-b"),
            "refreshed subject leaked: {error}"
        );
    }
}
