// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Shared runtime primitives for PRISM Rust binaries.
//!
//! Provides [`PrismPaths`] (XDG-based directory discovery), [`PrismCliState`]
//! (credential persistence), [`PlatformEndpoints`] (URL derivation for the
//! platform API, WebSocket, and dashboard), and [`retry`] — the shared
//! "is this failure worth another attempt?" policy every network path uses.
//!
//! # Naming: this is *not* a process/execution runtime
//!
//! The crate name `runtime` is historical. This crate owns configuration,
//! paths, credential resolution, offline policy, and retry policy; it does not
//! own an event loop, scheduler, process supervision, or task executor.

pub mod auth;
pub mod llm_resolve;
pub mod offline;
pub mod platform_env;
pub mod retry;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::platform_env::PlatformVar;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("could not resolve PRISM project directories")]
    ProjectDirsUnavailable,
    #[error("failed to read state file {path}: {source}")]
    ReadState {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write state file {path}: {source}")]
    WriteState {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse state file {path}: {source}")]
    ParseState {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error(
        "credential persistence failed ({persist}); restoring the previous state also failed ({rollback})"
    )]
    CredentialRollback {
        persist: Box<RuntimeError>,
        rollback: Box<RuntimeError>,
    },
}

/// Replace one credential-bearing file through a same-directory temporary.
///
/// Each individual file replacement is atomic and the temporary is owner-only
/// before any secret bytes are written. Coordination across the two credential
/// stores is handled separately by [`PrismPaths::persist_credentials`].
fn write_restricted_file(path: &Path, contents: &[u8]) -> Result<(), RuntimeError> {
    use std::io::Write as _;

    let parent = path.parent().ok_or_else(|| RuntimeError::WriteState {
        path: path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "credential path has no parent directory",
        ),
    })?;
    fs::create_dir_all(parent).map_err(|source| RuntimeError::WriteState {
        path: parent.to_path_buf(),
        source,
    })?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| RuntimeError::WriteState {
            path: path.to_path_buf(),
            source,
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| RuntimeError::WriteState {
                path: path.to_path_buf(),
                source,
            })?;
    }
    temporary
        .write_all(contents)
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| RuntimeError::WriteState {
            path: path.to_path_buf(),
            source,
        })?;
    temporary
        .persist(path)
        .map(|_| ())
        .map_err(|error| RuntimeError::WriteState {
            path: path.to_path_buf(),
            source: error.error,
        })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrismPaths {
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
}

/// The on-disk qualifier triple. The PRODUCT's name, not a company's: this
/// string is visible to every user as the path (`~/Library/Application
/// Support/dev.prism.prism` on macOS) and `prism status` prints it. A
/// company name here made PRISM look like a client for someone else's
/// platform.
const BUNDLE_ID: [&str; 3] = ["dev", "prism", "prism"];

/// The qualifier every shipped release up to and including v2.7.1 used.
/// Those installs are real — they hold `cli-state.json` (the login),
/// `node_key` / `node_signing_key` (the node's identity), and the audit,
/// session, rbac and subscription databases — so [`PrismPaths::discover`]
/// moves them across once rather than orphaning them. Frozen: changing it
/// strands everyone who has not upgraded yet.
const LEGACY_BUNDLE_ID: [&str; 3] = ["com", "marc27", "prism"];

impl PrismPaths {
    pub fn discover() -> Result<Self, RuntimeError> {
        let dirs = Self::for_bundle_id(BUNDLE_ID).ok_or(RuntimeError::ProjectDirsUnavailable)?;
        // Releases v1.0.0…v2.7.1 wrote under `com.marc27.prism`. Renaming the
        // bundle id without moving the data would silently log those installs
        // out, mint a second node identity on the next `node up`, and hide
        // their audit/rbac/subscription history. Best-effort by design — see
        // `migrate_dir`.
        dirs.migrate_legacy_install();
        Ok(dirs)
    }

    /// The four directories `directories` derives for one qualifier triple.
    /// `None` only when no home directory can be resolved at all.
    fn for_bundle_id(id: [&str; 3]) -> Option<Self> {
        let dirs = ProjectDirs::from(id[0], id[1], id[2])?;
        Some(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            state_dir: dirs
                .state_dir()
                .unwrap_or_else(|| dirs.data_local_dir())
                .to_path_buf(),
        })
    }

    /// One-time move of a pre-rename install into these directories.
    ///
    /// On platforms where the qualifier is not part of the path (Linux/XDG
    /// derives both from the application name alone) the old and new paths
    /// are identical and every pair is a no-op.
    fn migrate_legacy_install(&self) {
        let Some(legacy) = Self::for_bundle_id(LEGACY_BUNDLE_ID) else {
            return;
        };
        for (from, to) in [
            (&legacy.config_dir, &self.config_dir),
            (&legacy.cache_dir, &self.cache_dir),
            (&legacy.data_dir, &self.data_dir),
            (&legacy.state_dir, &self.state_dir),
        ] {
            migrate_dir(from, to);
        }
    }
}

/// Move one pre-rename directory to its new home, exactly once.
///
/// Skips unless the old directory exists and the new one does not, which is
/// what makes it both idempotent and non-destructive: a user who already
/// launched a renamed build and signed in again keeps that state, and a
/// second run after a successful move finds nothing left to do.
///
/// `rename` rather than copy: old and new are siblings under the same
/// parent, so it cannot cross a filesystem, it is atomic, and the 0600 modes
/// on `cli-state.json`, `node_key` and `node_signing_key` survive untouched
/// because the inodes never move.
///
/// Failure is reported and swallowed. A migration that cannot complete must
/// leave PRISM starting on an empty new directory — recoverable, and the old
/// data is still there — instead of refusing to start at all.
fn migrate_dir(from: &Path, to: &Path) {
    if from == to || to.exists() || !from.is_dir() {
        return;
    }
    if let Some(parent) = to.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        eprintln!("[prism] could not prepare {}: {e}", parent.display());
        return;
    }
    match fs::rename(from, to) {
        Ok(()) => eprintln!(
            "[prism] moved your PRISM data from {} to {}",
            from.display(),
            to.display()
        ),
        Err(e) => eprintln!(
            "[prism] could not move {} to {} ({e}); starting fresh at the new \
             location — your old data is untouched.",
            from.display(),
            to.display()
        ),
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct StoredCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub platform_url: String,
    /// Adapter that issued these credentials. Old credential files predate
    /// this field and came exclusively from MARC27, so their missing value is
    /// migrated to that provider. Newly written provider-neutral credentials
    /// serialize an explicit `null` and remain provider-neutral on reload.
    #[serde(default = "legacy_stored_platform_provider")]
    pub platform_provider: Option<String>,
    /// Auth-project root when identity is issued by a service separate from
    /// the PRISM-compatible platform API (for example Supabase Auth).
    #[serde(default)]
    pub identity_provider_url: Option<String>,
    /// Public provider client key needed for login and refresh. It is kept in
    /// the same restricted credential stores and redacted from `Debug`.
    #[serde(default)]
    pub identity_provider_key: Option<String>,
    pub user_id: Option<String>,
    pub display_name: Option<String>,
    pub org_id: Option<String>,
    pub org_name: Option<String>,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for StoredCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredCredentials")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("platform_url", &self.platform_url)
            .field("platform_provider", &self.platform_provider)
            .field("identity_provider_url", &self.identity_provider_url)
            .field(
                "identity_provider_key",
                &self.identity_provider_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("user_id", &self.user_id)
            .field("display_name", &self.display_name)
            .field("org_id", &self.org_id)
            .field("org_name", &self.org_name)
            .field("project_id", &self.project_id)
            .field("project_name", &self.project_name)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PrismCliState {
    pub credentials: Option<StoredCredentials>,
    #[serde(default)]
    pub preferred_python: Option<String>,
}

/// A durable node token — a stable, non-rotating node-scoped API key minted
/// once and reused by `node up` so the daemon survives session refresh-token
/// rotation. Stored at [`PrismPaths::node_token_path`], separately from the
/// rotating [`StoredCredentials`].
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct StoredNodeToken {
    /// Full API key (`m27_…`). Sent as the WS auth token; never rotates.
    pub key: String,
    /// API-key row id (for revocation via `DELETE /api-keys/{id}`).
    pub id: String,
    /// Short prefix for display (`m27_abcd…`).
    pub prefix: String,
    /// Platform API endpoint this durable key was minted against.
    ///
    /// Durable credentials must remain paired with their destination; an
    /// environment/config override is not authority to send this key to a
    /// different host. Empty only for legacy files written before endpoint
    /// binding was introduced.
    #[serde(default)]
    pub platform_url: String,
    /// Provider adapter recorded when this durable key was minted.
    #[serde(default)]
    pub platform_provider: Option<String>,
}

impl std::fmt::Debug for StoredNodeToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredNodeToken")
            .field("key", &"[REDACTED]")
            .field("id", &self.id)
            .field("prefix", &self.prefix)
            .field("platform_url", &self.platform_url)
            .field("platform_provider", &self.platform_provider)
            .finish()
    }
}

impl PrismPaths {
    pub fn cli_state_path(&self) -> PathBuf {
        self.config_dir.join("cli-state.json")
    }

    pub fn load_cli_state(&self) -> Result<PrismCliState, RuntimeError> {
        let path = self.cli_state_path();
        if !path.exists() {
            return Ok(PrismCliState::default());
        }
        let text = fs::read_to_string(&path).map_err(|source| RuntimeError::ReadState {
            path: path.clone(),
            source,
        })?;
        serde_json::from_str(&text).map_err(|source| RuntimeError::ParseState { path, source })
    }

    pub fn save_cli_state(&self, state: &PrismCliState) -> Result<(), RuntimeError> {
        let path = self.cli_state_path();
        let text =
            serde_json::to_string_pretty(state).expect("serializing cli state should not fail");
        write_restricted_file(&path, format!("{text}\n").as_bytes())
    }

    /// Path to the SDK credential mirror (`~/.prism/credentials.json`) that the
    /// Python platform tools read. HOME-based (NOT the XDG `config_dir`) to
    /// match where `_platform_creds.py` looks.
    pub fn sdk_credentials_path() -> Option<PathBuf> {
        env::var_os("HOME").map(|home| PathBuf::from(home).join(".prism").join("credentials.json"))
    }

    /// Persist credentials to BOTH stores: the authoritative `cli-state.json`
    /// AND the `~/.prism/credentials.json` SDK mirror.
    ///
    /// EVERY refresh MUST go through this. Writing only cli-state (the old
    /// silent-refresh behavior) left the SDK mirror holding a refresh token
    /// that single-use rotation had since REVOKED — replaying it tripped the
    /// server's token-family invalidation and forced a device-flow re-login
    /// (the "re-login every ~24h" drift). Both writes report errors; callers
    /// must never claim that a rotated pair was persisted while the SDK
    /// credential file still contains its predecessor.
    ///
    /// The two paths may live on different filesystems, so this does not claim
    /// cross-file crash atomicity. For every error reported in-process, the
    /// previous CLI state is restored (or the newly created state is removed),
    /// and each individual file is replaced atomically.
    pub fn persist_credentials(&self, creds: &StoredCredentials) -> Result<(), RuntimeError> {
        let state_path = self.cli_state_path();
        let state_existed = state_path.exists();
        let previous_state = self.load_cli_state()?;
        let mut state = previous_state.clone();
        state.credentials = Some(creds.clone());
        self.save_cli_state(&state)?;
        if let Err(persist) = Self::save_sdk_credentials(creds) {
            let rollback = if state_existed {
                self.save_cli_state(&previous_state)
            } else {
                match fs::remove_file(&state_path) {
                    Ok(()) => Ok(()),
                    Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(source) => Err(RuntimeError::WriteState {
                        path: state_path,
                        source,
                    }),
                }
            };
            if let Err(rollback) = rollback {
                return Err(RuntimeError::CredentialRollback {
                    persist: Box::new(persist),
                    rollback: Box::new(rollback),
                });
            }
            return Err(persist);
        }
        Ok(())
    }

    /// Write the `~/.prism/credentials.json` SDK mirror (0600 on unix).
    /// The JSON shape MUST stay in sync with the Python `_platform_creds.py`.
    pub fn save_sdk_credentials(creds: &StoredCredentials) -> Result<(), RuntimeError> {
        let Some(path) = Self::sdk_credentials_path() else {
            return Ok(());
        };
        let mirror = serde_json::json!({
            "access_token": creds.access_token,
            "refresh_token": creds.refresh_token,
            "platform_url": creds.platform_url,
            "platform_provider": creds.platform_provider,
            "identity_provider_url": creds.identity_provider_url,
            "identity_provider_key": creds.identity_provider_key,
            "user_id": creds.user_id,
            "org_id": creds.org_id,
            "project_id": creds.project_id,
            // Persisted so a cli-state rebuilt from this mirror keeps the
            // expiry and the proactive (within-5-min) refresh fires. Without
            // it the mirror yields expires_at=None → every launch falls into
            // the reactive retry path. Extra field: ignored by readers that
            // don't know it (the Python _platform_creds.py reader), and
            // emitted as RFC 3339 so it deserializes back into DateTime<Utc>.
            "expires_at": creds.expires_at,
        });
        let json = serde_json::to_string_pretty(&mirror)
            .expect("serializing SDK credentials should not fail");
        write_restricted_file(&path, json.as_bytes())
    }

    /// Path to the durable node-token file (`{state_dir}/node-token`).
    pub fn node_token_path(&self) -> PathBuf {
        self.state_dir.join("node-token")
    }

    /// Load the durable node token, if one is stored. Returns `None` when the
    /// file is absent or unreadable (never panics — the daemon falls back to
    /// the rotating session token).
    pub fn load_node_token(&self) -> Option<StoredNodeToken> {
        let path = self.node_token_path();
        let text = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Persist a durable node token with 0600 permissions (contains a key).
    pub fn save_node_token(&self, token: &StoredNodeToken) -> Result<(), RuntimeError> {
        let path = self.node_token_path();
        fs::create_dir_all(&self.state_dir).map_err(|source| RuntimeError::WriteState {
            path: self.state_dir.clone(),
            source,
        })?;
        let text =
            serde_json::to_string_pretty(token).expect("serializing node token should not fail");
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .map_err(|source| RuntimeError::WriteState {
                    path: path.clone(),
                    source,
                })?;
            file.write_all(format!("{text}\n").as_bytes())
                .map_err(|source| RuntimeError::WriteState { path, source })
        }
        #[cfg(not(unix))]
        {
            fs::write(&path, format!("{text}\n"))
                .map_err(|source| RuntimeError::WriteState { path, source })
        }
    }

    /// Remove the durable node-token file (best-effort; returns whether it existed).
    pub fn clear_node_token(&self) -> bool {
        fs::remove_file(self.node_token_path()).is_ok()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlatformEndpoints {
    pub api_base: String,
    pub node_ws: String,
    /// External adapter identity. `None` is a valid provider-neutral endpoint
    /// and must never be guessed from a PRISM-native environment variable.
    #[serde(default)]
    pub provider: Option<String>,
}

impl PlatformEndpoints {
    /// Resolve an explicitly configured platform from the environment.
    ///
    /// `PRISM_API_URL` is canonical. `PRISM_PLATFORM_URL` remains supported,
    /// as do both historical `MARC27_*` aliases, but all PRISM-native names
    /// outrank all provider aliases. There is deliberately no hosted default:
    /// `None` means this PRISM install is local-only until an operator points
    /// it at a provider.
    pub fn from_env() -> Option<Self> {
        Self::resolve_with_provider(None, None, None)
    }

    /// Resolve endpoints from all durable configuration surfaces.
    ///
    /// Environment is the explicit process override, then `[platform].url`,
    /// then the endpoint stored with an existing login. The stored endpoint is
    /// what preserves already-installed sessions after removing the implicit
    /// MARC27 default.
    pub fn resolve(
        configured_url: Option<&str>,
        credentials: Option<&StoredCredentials>,
    ) -> Option<Self> {
        Self::resolve_with_provider(configured_url, None, credentials)
    }

    /// Resolve an endpoint together with the adapter that owns its external
    /// protocol. Endpoint and provider are deliberately separate: PRISM roles
    /// remain PRISM roles, and a provider merely supplies a mapping adapter.
    pub fn resolve_with_provider(
        configured_url: Option<&str>,
        configured_provider: Option<&str>,
        credentials: Option<&StoredCredentials>,
    ) -> Option<Self> {
        const URL_VARS: [PlatformVar; 2] = [PlatformVar::API_URL, PlatformVar::PLATFORM_URL];
        const CREDENTIAL_VARS: [PlatformVar; 3] = [
            PlatformVar::API_KEY,
            PlatformVar::TOKEN,
            PlatformVar::API_TOKEN,
        ];

        let env_url = PlatformVar::get_with_source_preferred_then_alias(&URL_VARS);
        let env_provider = PlatformVar::PROVIDER.get();
        let selected_credential =
            PlatformVar::get_with_source_preferred_then_alias(&CREDENTIAL_VARS);

        let url = env_url
            .as_ref()
            .map(|(value, _)| value.as_str())
            .or_else(|| configured_url.and_then(non_blank_value))
            .or_else(|| {
                credentials
                    .map(|value| value.platform_url.as_str())
                    .and_then(non_blank_value)
            });

        let configured_provider = configured_provider.and_then(non_blank_value);
        let stored_provider = credentials
            .and_then(|value| value.platform_provider.as_deref())
            .and_then(non_blank_value);
        let selected_url_matches_stored = url.is_some_and(|selected_url| {
            credentials
                .map(|value| value.platform_url.as_str())
                .and_then(non_blank_value)
                .is_some_and(|stored_url| same_platform_endpoint(selected_url, stored_url))
        });
        let stored_provider_for_selected_url = selected_url_matches_stored
            .then(|| stored_provider.map(normalize_provider))
            .flatten();
        let alias_url_selected = env_url
            .as_ref()
            .is_some_and(|(_, source)| source.starts_with("MARC27_"));
        let alias_credential_selected = selected_credential
            .as_ref()
            .is_some_and(|(_, source)| source.starts_with("MARC27_"));
        let explicit_provider = env_provider
            .as_deref()
            .and_then(non_blank_value)
            .or(configured_provider)
            .map(normalize_provider);
        let inferred_provider = if env_url.is_some() {
            // A PRISM-native URL is provider-neutral. A MARC27-named URL is
            // explicit legacy adapter evidence. Provider metadata from a
            // stored login travels only when the override resolves to that
            // exact endpoint; an unrelated process override stays neutral.
            alias_url_selected
                .then(|| MARC27_PROVIDER.to_string())
                .or(stored_provider_for_selected_url)
        } else if let Some(configured_url) = configured_url.and_then(non_blank_value) {
            // Pre-stage config had no provider field. Preserve an explicitly
            // configured historical host, but do not carry a stored MARC27
            // identity onto an unrelated configured endpoint.
            stored_provider_for_selected_url
                .or_else(|| is_marc27_endpoint(configured_url).then(|| MARC27_PROVIDER.to_string()))
        } else if url.is_some() {
            // The selected URL came from the stored login, so its provider
            // metadata travels with it.
            stored_provider.map(normalize_provider)
        } else if alias_credential_selected {
            // Key-only legacy installs explicitly selected MARC27 through the
            // company-scoped credential name.
            Some(MARC27_PROVIDER.to_string())
        } else {
            stored_provider.map(normalize_provider)
        };
        let provider = explicit_provider.or(inferred_provider);

        match url {
            Some(url) => Some(Self::from_url_with_provider(url, provider)),
            None if provider.as_deref() == Some(MARC27_PROVIDER) => Some(Self::marc27()),
            None => None,
        }
    }

    /// Resolve all configuration plus a durable node credential. A frozen
    /// `m27_` node key is explicit legacy-provider evidence, so old nodes keep
    /// connecting without restoring an unconditional global endpoint.
    pub fn resolve_for_paths(
        configured_url: Option<&str>,
        configured_provider: Option<&str>,
        credentials: Option<&StoredCredentials>,
        paths: &PrismPaths,
    ) -> Option<Self> {
        let endpoints =
            Self::resolve_with_provider(configured_url, configured_provider, credentials);
        let has_marc27_node_key = paths
            .load_node_token()
            .is_some_and(|token| token.key.starts_with("m27_"));
        match endpoints {
            Some(endpoints) => Some(endpoints),
            None if has_marc27_node_key => Some(Self::marc27()),
            None => None,
        }
    }

    /// Build the API and node-WebSocket endpoints from either a bare platform
    /// root or a full `/api/v1` API base.
    pub fn from_url(url: &str) -> Self {
        Self::from_url_with_provider(url, None)
    }

    /// Build endpoints while retaining an explicitly selected provider.
    pub fn from_url_with_provider(url: &str, provider: Option<String>) -> Self {
        let root = url.trim().trim_end_matches('/');
        let api_base = if root.ends_with("/api/v1") {
            root.to_string()
        } else {
            format!("{root}/api/v1")
        };

        let ws_root = if let Some(rest) = api_base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = api_base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            api_base.clone()
        };

        Self {
            api_base,
            node_ws: format!("{ws_root}/nodes/connect"),
            provider,
        }
    }

    /// Compatibility endpoint for an explicitly selected MARC27 provider.
    /// This is never a default: it is reachable only from provider metadata,
    /// a selected `MARC27_*` alias, or a frozen `m27_` durable node key.
    pub fn marc27() -> Self {
        Self::from_url_with_provider(MARC27_PROVIDER_API_BASE, Some(MARC27_PROVIDER.to_string()))
    }

    /// Resolve the process environment's credential for this platform.
    ///
    /// A Supabase project's public anon key may be supplied through
    /// `PRISM_API_KEY` for login configuration. It authenticates PRISM to the
    /// identity provider, not the user to the PRISM-compatible platform, so it
    /// must never displace the verified access token stored by login. Explicit
    /// token variables retain their normal native-first precedence.
    pub fn environment_credential(&self) -> Option<auth::PlatformAuth> {
        let variables: &[PlatformVar] = if self.provider.as_deref() == Some(SUPABASE_PROVIDER) {
            &[PlatformVar::TOKEN, PlatformVar::API_TOKEN]
        } else {
            &[
                PlatformVar::API_KEY,
                PlatformVar::TOKEN,
                PlatformVar::API_TOKEN,
            ]
        };
        let (value, source) = PlatformVar::get_with_source_preferred_then_alias(variables)?;
        if source == PlatformVar::API_KEY.preferred || source == PlatformVar::API_KEY.alias {
            Some(auth::PlatformAuth::ApiKey(value))
        } else {
            Some(auth::PlatformAuth::classify(&value))
        }
    }
}

/// Stable adapter id used at provider boundaries.
pub const MARC27_PROVIDER: &str = "marc27";
/// Stable adapter id for Supabase Auth boundaries.
pub const SUPABASE_PROVIDER: &str = "supabase";
/// Endpoint belonging to the optional MARC27 adapter. It is selected only by
/// explicit legacy/provider evidence; it is not PRISM's default endpoint.
pub const MARC27_PROVIDER_API_BASE: &str = "https://api.marc27.com/api/v1";

fn legacy_stored_platform_provider() -> Option<String> {
    Some(MARC27_PROVIDER.to_string())
}

fn normalize_provider(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn is_marc27_endpoint(value: &str) -> bool {
    let host = value
        .trim()
        .trim_end_matches('/')
        .strip_prefix("https://")
        .or_else(|| value.trim().trim_end_matches('/').strip_prefix("http://"))
        .unwrap_or_default()
        .split('/')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default();
    host == "marc27.com" || host.ends_with(".marc27.com")
}

fn same_platform_endpoint(left: &str, right: &str) -> bool {
    PlatformEndpoints::from_url(left).api_base == PlatformEndpoints::from_url(right).api_base
}

fn non_blank_value(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `env::set_var` mutates the process-global environment, which is not
    // thread-safe against concurrent env access on any variable. Serialize
    // every env-touching test through this guard.
    pub(crate) static ENV_GUARD: Mutex<()> = Mutex::new(());

    /// Clear the full platform surface so endpoint tests cannot accidentally
    /// inherit a credential/provider from the developer's shell.
    fn clear_platform_url() {
        unsafe {
            for var in PlatformVar::ALL {
                env::remove_var(var.preferred);
                env::remove_var(var.alias);
            }
        }
    }

    /// The historical `MARC27_*` name must keep working forever: every shipped
    /// client, deployed node and CI secret sets it.
    ///
    /// The URL here deliberately does NOT match the built-in default. An
    /// earlier version of this test used `https://api.marc27.com/`, which is
    /// byte-identical to `default_root` once the trailing slash is trimmed —
    /// so it passed whether the alias resolved or the lookup returned `None`
    /// and fell through to the default. It could not fail for the reason it
    /// was named after. A non-default host makes the two outcomes distinct.
    #[test]
    fn historical_platform_url_alias_still_resolves() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_url();
        unsafe {
            env::set_var("MARC27_PLATFORM_URL", "https://legacy.example.test/");
        }
        let endpoints = PlatformEndpoints::from_env().expect("legacy alias should configure it");
        assert_eq!(endpoints.api_base, "https://legacy.example.test/api/v1");
        assert_eq!(
            endpoints.node_ws,
            "wss://legacy.example.test/api/v1/nodes/connect"
        );
        clear_platform_url();
    }

    /// The provider-neutral name works at a real call site, not just in the
    /// resolver's own unit tests. This is what makes "PRISM can be pointed at
    /// another provider" a demonstrated property rather than a claim.
    #[test]
    fn neutral_platform_url_is_honoured() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_url();
        unsafe {
            env::set_var("PRISM_PLATFORM_URL", "https://self-hosted.example.test/");
        }
        let endpoints = PlatformEndpoints::from_env().expect("native URL should configure it");
        assert_eq!(
            endpoints.api_base,
            "https://self-hosted.example.test/api/v1"
        );
        assert_eq!(
            endpoints.node_ws,
            "wss://self-hosted.example.test/api/v1/nodes/connect"
        );
        clear_platform_url();
    }

    /// Precedence, proven where it matters: an operator adding the neutral
    /// name to a host that already carries the historical one gets the neutral
    /// one. Both values are non-default, so neither can be produced by a
    /// fallback.
    #[test]
    fn neutral_platform_url_wins_over_the_alias() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_url();
        unsafe {
            env::set_var("PRISM_PLATFORM_URL", "https://new.example.test/");
            env::set_var("MARC27_PLATFORM_URL", "https://old.example.test/");
        }
        let endpoints = PlatformEndpoints::from_env().expect("native URL should configure it");
        assert_eq!(endpoints.api_base, "https://new.example.test/api/v1");
        clear_platform_url();
    }

    /// With no explicit URL there is no provider. This must fail if a future
    /// change reintroduces any implicit hosted endpoint, MARC27 or otherwise.
    #[test]
    fn unset_platform_url_is_unconfigured() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_url();
        assert_eq!(PlatformEndpoints::from_env(), None);
    }

    /// A provider-neutral key carries no endpoint identity. This is the
    /// falsifiable guard against silently restoring another company's host.
    #[test]
    fn native_key_without_endpoint_or_provider_is_unconfigured() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_url();
        unsafe { env::set_var("PRISM_API_KEY", "provider-defined-shape") };
        let endpoints = PlatformEndpoints::from_env();
        assert_eq!(endpoints, None);
        clear_platform_url();
    }

    /// A historical company-scoped credential explicitly selects the legacy
    /// adapter. Existing key-only installs keep working, but no unconfigured
    /// install inherits this endpoint.
    #[test]
    fn legacy_key_only_selects_the_optional_marc27_provider() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_url();
        unsafe { env::set_var("MARC27_API_KEY", "m27_legacy") };
        let endpoints = PlatformEndpoints::from_env().expect("legacy key selects its provider");
        assert_eq!(endpoints.api_base, MARC27_PROVIDER_API_BASE);
        assert_eq!(endpoints.provider.as_deref(), Some(MARC27_PROVIDER));
        clear_platform_url();
    }

    #[test]
    fn native_credential_family_shadows_legacy_api_key_provider_evidence() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_url();
        unsafe {
            env::set_var("PRISM_TOKEN", "native-session");
            env::set_var("MARC27_API_KEY", "m27_shadowed");
        }
        assert_eq!(PlatformEndpoints::from_env(), None);
        clear_platform_url();
    }

    #[test]
    fn explicit_provider_can_supply_its_known_endpoint() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_url();
        unsafe {
            env::set_var("PRISM_PLATFORM_PROVIDER", "MARC27");
            env::set_var("PRISM_API_KEY", "provider-defined-shape");
        }
        let endpoints = PlatformEndpoints::from_env().expect("provider is explicit");
        assert_eq!(endpoints.api_base, MARC27_PROVIDER_API_BASE);
        assert_eq!(endpoints.provider.as_deref(), Some(MARC27_PROVIDER));
        clear_platform_url();
    }

    #[test]
    fn native_url_never_infers_provider_from_its_hostname() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_url();
        unsafe { env::set_var("PRISM_API_URL", "https://api.marc27.com") };
        let endpoints = PlatformEndpoints::from_env().expect("URL is configured");
        assert_eq!(endpoints.provider, None);
        clear_platform_url();
    }

    #[test]
    fn native_url_does_not_inherit_stale_stored_provider_identity() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_url();
        unsafe {
            env::set_var("PRISM_API_URL", "https://independent.example");
            env::set_var("PRISM_API_KEY", "independent-key");
        }
        let credentials = StoredCredentials {
            access_token: "old-session".into(),
            platform_url: "https://api.marc27.com".into(),
            platform_provider: Some(MARC27_PROVIDER.into()),
            ..Default::default()
        };

        let endpoints = PlatformEndpoints::resolve(None, Some(&credentials)).unwrap();
        assert_eq!(endpoints.api_base, "https://independent.example/api/v1");
        assert_eq!(endpoints.provider, None);
        clear_platform_url();
    }

    #[test]
    fn matching_native_url_retains_stored_supabase_provider_identity() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_url();
        unsafe {
            env::set_var("PRISM_API_URL", "https://project.supabase.co/");
        }
        let credentials = StoredCredentials {
            access_token: "verified-session".into(),
            platform_url: "https://project.supabase.co/api/v1".into(),
            platform_provider: Some(SUPABASE_PROVIDER.into()),
            identity_provider_url: Some("https://project.supabase.co".into()),
            identity_provider_key: Some("public-anon-key".into()),
            ..Default::default()
        };

        let endpoints = PlatformEndpoints::resolve(None, Some(&credentials)).unwrap();
        assert_eq!(endpoints.api_base, "https://project.supabase.co/api/v1");
        assert_eq!(endpoints.provider.as_deref(), Some(SUPABASE_PROVIDER));
        clear_platform_url();
    }

    #[test]
    fn supabase_anon_key_is_not_resolved_as_a_platform_credential() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_url();
        let endpoints = PlatformEndpoints::from_url_with_provider(
            "https://project.supabase.co",
            Some(SUPABASE_PROVIDER.into()),
        );
        unsafe {
            env::set_var("PRISM_API_KEY", "public-anon-key");
        }

        assert_eq!(endpoints.environment_credential(), None);

        // Native token spellings remain ahead of all deprecated aliases, even
        // when the alias appears earlier in the logical token family.
        unsafe {
            env::set_var("MARC27_TOKEN", "deprecated-session");
            env::set_var("PRISM_API_TOKEN", "native-api-session");
        }
        assert_eq!(
            endpoints.environment_credential(),
            Some(auth::PlatformAuth::Bearer("native-api-session".into()))
        );
        unsafe {
            env::set_var("PRISM_TOKEN", "native-primary-session");
        }
        assert_eq!(
            endpoints.environment_credential(),
            Some(auth::PlatformAuth::Bearer("native-primary-session".into()))
        );
        clear_platform_url();
    }

    #[test]
    fn native_url_is_not_relabelled_by_an_old_node_key() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_url();
        unsafe { env::set_var("PRISM_API_URL", "https://independent.example") };
        let dir = env::temp_dir().join(format!("prism-provider-source-{}", std::process::id()));
        let paths = PrismPaths {
            config_dir: dir.join("config"),
            cache_dir: dir.join("cache"),
            data_dir: dir.join("data"),
            state_dir: dir.join("state"),
        };
        paths
            .save_node_token(&StoredNodeToken {
                key: "m27_old-node-key".into(),
                id: "legacy".into(),
                prefix: "m27_old".into(),
                ..Default::default()
            })
            .unwrap();

        let endpoints = PlatformEndpoints::resolve_for_paths(None, None, None, &paths).unwrap();
        assert_eq!(endpoints.api_base, "https://independent.example/api/v1");
        assert_eq!(endpoints.provider, None);

        clear_platform_url();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn full_api_base_is_not_double_suffixed() {
        let endpoints = PlatformEndpoints::from_url("https://provider.test/api/v1/");
        assert_eq!(endpoints.api_base, "https://provider.test/api/v1");
        assert_eq!(
            endpoints.node_ws,
            "wss://provider.test/api/v1/nodes/connect"
        );
    }

    #[test]
    fn stored_login_preserves_existing_install_without_a_default() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        clear_platform_url();
        let credentials = StoredCredentials {
            platform_url: "https://existing-provider.test".into(),
            access_token: "session".into(),
            ..Default::default()
        };
        let endpoints = PlatformEndpoints::resolve(None, Some(&credentials))
            .expect("stored login endpoint should be retained");
        assert_eq!(endpoints.api_base, "https://existing-provider.test/api/v1");
    }

    #[test]
    fn old_stored_session_migrates_to_its_historical_provider() {
        let old_json = r#"{
            "access_token": "session",
            "refresh_token": "refresh",
            "platform_url": "https://api.marc27.com"
        }"#;
        let credentials: StoredCredentials = serde_json::from_str(old_json).unwrap();
        assert_eq!(
            credentials.platform_provider.as_deref(),
            Some(MARC27_PROVIDER)
        );

        let new_json = r#"{
            "access_token": "session",
            "refresh_token": "refresh",
            "platform_url": "https://provider.example",
            "platform_provider": null
        }"#;
        let credentials: StoredCredentials = serde_json::from_str(new_json).unwrap();
        assert_eq!(credentials.platform_provider, None);
    }

    // The regression guard for the "re-login every ~24h" dance: a refresh must
    // land in BOTH the authoritative `cli-state.json` AND the HOME-based
    // `~/.prism/credentials.json` SDK mirror, or the neglected store keeps a
    // server-revoked refresh token and the next start is forced back through
    // device flow.
    #[test]
    fn persist_credentials_writes_both_stores() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());

        let base = env::temp_dir().join(format!("prism-cred-test-{}", std::process::id()));
        let home = base.join("home");
        let cfg = base.join("config");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&cfg).unwrap();

        let prev_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", &home);
        }

        let paths = PrismPaths {
            config_dir: cfg.clone(),
            cache_dir: base.join("cache"),
            data_dir: base.join("data"),
            state_dir: base.join("state"),
        };
        let mirror_path = home.join(".prism").join("credentials.json");
        // Regression setup: `mode(0o600)` on OpenOptions does not change an
        // existing file. Start with permissive legacy files and prove the
        // credential writer repairs them before storing the new token pair.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::create_dir_all(mirror_path.parent().unwrap()).unwrap();
            fs::write(paths.cli_state_path(), "{}\n").unwrap();
            fs::write(&mirror_path, "old-readable-secret\n").unwrap();
            fs::set_permissions(paths.cli_state_path(), fs::Permissions::from_mode(0o644)).unwrap();
            fs::set_permissions(&mirror_path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        let creds = StoredCredentials {
            access_token: "at-new".into(),
            refresh_token: "rt-rotated".into(),
            platform_url: "https://api.marc27.com".into(),
            identity_provider_url: Some("https://project.supabase.co".into()),
            identity_provider_key: Some("public-anon-key".into()),
            user_id: Some("u1".into()),
            org_id: Some("o1".into()),
            project_id: Some("p1".into()),
            // expires_at MUST survive into the mirror: without it a cli-state
            // rebuilt from the mirror gets expires_at=None → the proactive
            // (within-5-min) refresh never fires → every launch falls into the
            // reactive retry path. (F0 review fix #3.)
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(24)),
            ..Default::default()
        };

        paths.persist_credentials(&creds).unwrap();

        // Store 1: cli-state.json round-trips the rotated tokens + expiry.
        let stored = paths
            .load_cli_state()
            .unwrap()
            .credentials
            .expect("cli-state must hold credentials");
        assert_eq!(stored.access_token, "at-new");
        assert_eq!(stored.refresh_token, "rt-rotated");
        assert!(
            stored.expires_at.is_some(),
            "cli-state must persist expires_at"
        );

        // Store 2: SDK mirror exists with the 6-field Python shape PLUS
        // expires_at (review fix #3).
        assert!(
            mirror_path.exists(),
            "SDK mirror must be written on refresh"
        );
        let mirror: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&mirror_path).unwrap()).unwrap();
        assert_eq!(mirror["access_token"], "at-new");
        assert_eq!(mirror["refresh_token"], "rt-rotated");
        assert_eq!(mirror["platform_url"], "https://api.marc27.com");
        assert_eq!(
            mirror["identity_provider_url"],
            "https://project.supabase.co"
        );
        assert_eq!(mirror["identity_provider_key"], "public-anon-key");
        assert_eq!(mirror["user_id"], "u1");
        assert_eq!(mirror["org_id"], "o1");
        assert_eq!(mirror["project_id"], "p1");
        assert!(
            !mirror["expires_at"].is_null(),
            "SDK mirror MUST carry expires_at (F0 review fix #3) — a mirror \
             without it yields expires_at=None on rebuild and the proactive \
             refresh never fires"
        );
        // And it must deserialize back into a DateTime<Utc> (the shape
        // StoredCredentials expects on reload).
        assert!(
            serde_json::from_value::<chrono::DateTime<chrono::Utc>>(mirror["expires_at"].clone())
                .is_ok(),
            "mirror expires_at must be a valid RFC 3339 timestamp"
        );

        // The mirror holds bearer + refresh tokens — must be owner-only (0600).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let cli_state_mode = fs::metadata(paths.cli_state_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let mode = fs::metadata(&mirror_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(cli_state_mode, 0o600, "CLI state must be repaired to 0600");
            assert_eq!(mode, 0o600, "SDK mirror must be 0600");
        }

        unsafe {
            match prev_home {
                Some(h) => env::set_var("HOME", h),
                None => env::remove_var("HOME"),
            }
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn stored_credentials_debug_redacts_every_credential() {
        let credentials = StoredCredentials {
            access_token: "access-do-not-log".into(),
            refresh_token: "refresh-do-not-log".into(),
            identity_provider_key: Some("provider-key-do-not-log".into()),
            ..Default::default()
        };

        let debug = format!("{credentials:?}");
        for secret in [
            "access-do-not-log",
            "refresh-do-not-log",
            "provider-key-do-not-log",
        ] {
            assert!(!debug.contains(secret), "Debug leaked {secret}: {debug}");
        }
        assert_eq!(debug.matches("[REDACTED]").count(), 3);
    }

    #[test]
    fn persist_credentials_reports_sdk_mirror_write_failure() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let directory = tempfile::tempdir().expect("isolated credential paths");
        let home = directory.path().join("home");
        let config = directory.path().join("config");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config).unwrap();
        // A file where the mirror directory must be makes create_dir_all fail
        // deterministically without relying on the test runner's uid.
        fs::write(home.join(".prism"), "not a directory").unwrap();
        let paths = PrismPaths {
            config_dir: config,
            cache_dir: directory.path().join("cache"),
            data_dir: directory.path().join("data"),
            state_dir: directory.path().join("state"),
        };
        let previous_home = env::var_os("HOME");
        unsafe { env::set_var("HOME", &home) };
        let result = paths.persist_credentials(&StoredCredentials {
            access_token: "new-access".into(),
            refresh_token: "new-refresh".into(),
            ..Default::default()
        });
        unsafe {
            match previous_home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }
        }

        let error = result
            .expect_err("mirror failure must be reported")
            .to_string();
        assert!(error.contains("failed to write state file"), "{error}");
        assert!(
            !paths.cli_state_path().exists(),
            "a failed first persistence must remove the newly created CLI state"
        );
    }

    #[test]
    fn persist_credentials_restores_previous_cli_state_when_mirror_fails() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let directory = tempfile::tempdir().expect("isolated credential paths");
        let home = directory.path().join("home");
        let config = directory.path().join("config");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::write(home.join(".prism"), "not a directory").unwrap();
        let paths = PrismPaths {
            config_dir: config,
            cache_dir: directory.path().join("cache"),
            data_dir: directory.path().join("data"),
            state_dir: directory.path().join("state"),
        };
        let previous_state = PrismCliState {
            credentials: Some(StoredCredentials {
                access_token: "old-access".into(),
                refresh_token: "old-refresh".into(),
                ..Default::default()
            }),
            preferred_python: Some("/existing/python".into()),
        };
        paths.save_cli_state(&previous_state).unwrap();

        let previous_home = env::var_os("HOME");
        unsafe { env::set_var("HOME", &home) };
        let result = paths.persist_credentials(&StoredCredentials {
            access_token: "new-access".into(),
            refresh_token: "new-refresh".into(),
            ..Default::default()
        });
        unsafe {
            match previous_home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }
        }

        result.expect_err("mirror failure must be reported");
        assert_eq!(
            paths.load_cli_state().unwrap(),
            previous_state,
            "the old credential pair and unrelated CLI state must be restored"
        );
    }

    #[test]
    fn persist_credentials_does_not_overwrite_malformed_cli_state() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let directory = tempfile::tempdir().expect("isolated credential paths");
        let paths = PrismPaths {
            config_dir: directory.path().join("config"),
            cache_dir: directory.path().join("cache"),
            data_dir: directory.path().join("data"),
            state_dir: directory.path().join("state"),
        };
        fs::create_dir_all(&paths.config_dir).unwrap();
        let malformed = b"{ definitely-not-valid-json\n";
        fs::write(paths.cli_state_path(), malformed).unwrap();

        let result = paths.persist_credentials(&StoredCredentials {
            access_token: "new-access".into(),
            refresh_token: "new-refresh".into(),
            ..Default::default()
        });

        assert!(matches!(result, Err(RuntimeError::ParseState { .. })));
        assert_eq!(fs::read(paths.cli_state_path()).unwrap(), malformed);
    }

    #[test]
    fn stored_node_token_roundtrips() {
        let token = StoredNodeToken {
            key: "m27_secret_key_value".into(),
            id: "00000000-0000-0000-0000-000000000001".into(),
            prefix: "m27_abcd".into(),
            platform_url: "https://provider.example/api/v1".into(),
            platform_provider: Some("marc27".into()),
        };
        let json = serde_json::to_string(&token).unwrap();
        let parsed: StoredNodeToken = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, token);
    }

    #[test]
    fn stored_node_token_debug_redacts_the_key() {
        let token = StoredNodeToken {
            key: "m27_DO_NOT_LEAK_THIS".into(),
            id: "id-1".into(),
            prefix: "m27_xy".into(),
            platform_url: "https://provider.example/api/v1".into(),
            platform_provider: Some("marc27".into()),
        };
        let dbg = format!("{token:?}");
        assert!(
            !dbg.contains("DO_NOT_LEAK_THIS"),
            "debug leaked the key: {dbg}"
        );
        assert!(dbg.contains("id-1") && dbg.contains("m27_xy"));
        assert!(dbg.contains("[REDACTED]"));
    }

    /// The durable-token path is a single source of truth shared by the daemon
    /// (load) and the CLI (mint/revoke) — they must agree.
    #[test]
    fn node_token_path_is_under_state_dir() {
        let paths = PrismPaths {
            config_dir: std::path::PathBuf::from("/tmp/prism-cfg"),
            cache_dir: std::path::PathBuf::from("/tmp/prism-cache"),
            data_dir: std::path::PathBuf::from("/tmp/prism-data"),
            state_dir: std::path::PathBuf::from("/tmp/prism-state"),
        };
        assert_eq!(
            paths.node_token_path(),
            std::path::PathBuf::from("/tmp/prism-state/node-token")
        );
    }

    /// save → load → clear round-trip in a temp dir: proves the daemon's
    /// preference for a stored node token actually reads back what mint wrote.
    #[test]
    fn save_load_clear_node_token_roundtrip() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "prism-node-token-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = PrismPaths {
            config_dir: dir.join("cfg"),
            cache_dir: dir.join("cache"),
            data_dir: dir.join("data"),
            state_dir: dir.clone(),
        };

        // Absent → None (daemon falls back to the rotating session token).
        assert!(paths.load_node_token().is_none());

        // Save + load returns exactly what was stored.
        let token = StoredNodeToken {
            key: "m27_roundtrip_key".into(),
            id: "id-42".into(),
            prefix: "m27_rt".into(),
            platform_url: "https://provider.example/api/v1".into(),
            platform_provider: Some("marc27".into()),
        };
        paths.save_node_token(&token).unwrap();
        assert_eq!(paths.load_node_token().as_ref(), Some(&token));

        // The file must be 0600 on unix (it contains a key).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(paths.node_token_path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "node-token file is not 0600");
        }

        // Clear removes it → back to None.
        assert!(paths.clear_node_token());
        assert!(paths.load_node_token().is_none());
        // Clear is idempotent once gone (returns false, no panic).
        assert!(!paths.clear_node_token());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Pre-rename install migration (`com.marc27.prism` → `dev.prism.prism`) ──

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Seed a directory shaped like a real pre-rename install: the login
    /// state and the node's identity keys at 0600, a world-readable database,
    /// and a nested directory.
    fn seed_legacy_install(dir: &Path, marker: &str) {
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("cli-state.json"), marker).unwrap();
        fs::write(dir.join("node_key"), b"raw-32-bytes").unwrap();
        fs::write(dir.join("node_signing_key"), b"raw-32-bytes").unwrap();
        fs::write(dir.join("audit.db"), b"sqlite").unwrap();
        fs::write(dir.join("nested").join("sessions.db"), b"sqlite").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for secret in ["cli-state.json", "node_key", "node_signing_key"] {
                fs::set_permissions(dir.join(secret), fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
    }

    /// The whole point of the migration: everything an install owns arrives
    /// at the new location, and the owner-only modes on the credential and
    /// node-identity files survive the move.
    #[test]
    fn migrate_dir_moves_a_legacy_install_preserving_modes() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("com.marc27.prism");
        let new = tmp.path().join("dev.prism.prism");
        seed_legacy_install(&legacy, "the-original-login");

        migrate_dir(&legacy, &new);

        assert_eq!(
            fs::read_to_string(new.join("cli-state.json")).unwrap(),
            "the-original-login",
            "the login must survive the bundle-id rename"
        );
        for carried in ["node_key", "node_signing_key", "audit.db"] {
            assert!(new.join(carried).exists(), "{carried} was left behind");
        }
        assert!(
            new.join("nested").join("sessions.db").exists(),
            "nested databases must come across too"
        );
        #[cfg(unix)]
        for secret in ["cli-state.json", "node_key", "node_signing_key"] {
            assert_eq!(
                mode_of(&new.join(secret)),
                0o600,
                "{secret} must stay owner-only after migration"
            );
        }
        assert!(!legacy.exists(), "the old directory should be gone");
    }

    /// Run it twice — the second launch of an upgraded build must be a
    /// no-op, not a second move that undoes the first.
    #[test]
    fn migrate_dir_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("com.marc27.prism");
        let new = tmp.path().join("dev.prism.prism");
        seed_legacy_install(&legacy, "the-original-login");

        migrate_dir(&legacy, &new);
        fs::write(new.join("cli-state.json"), "refreshed-after-upgrade").unwrap();
        migrate_dir(&legacy, &new);

        assert_eq!(
            fs::read_to_string(new.join("cli-state.json")).unwrap(),
            "refreshed-after-upgrade",
            "a second run must not resurrect the pre-migration state"
        );
    }

    /// Someone who already ran the un-migrated build and signed in again has
    /// live state at the new location. Migration must never overwrite it.
    #[test]
    fn migrate_dir_never_clobbers_the_new_location() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("com.marc27.prism");
        let new = tmp.path().join("dev.prism.prism");
        seed_legacy_install(&legacy, "stale-login");
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("cli-state.json"), "the-login-in-use").unwrap();

        migrate_dir(&legacy, &new);

        assert_eq!(
            fs::read_to_string(new.join("cli-state.json")).unwrap(),
            "the-login-in-use",
            "an existing new-location file must win"
        );
        assert!(
            legacy.exists(),
            "with nothing to do, the old directory is left for the user to inspect"
        );
    }

    /// A fresh install has no legacy directory. Migration must not conjure
    /// the new one either — `discover()` runs on every invocation, including
    /// ones that go on to touch no state at all.
    #[test]
    fn migrate_dir_does_nothing_without_a_legacy_install() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("com.marc27.prism");
        let new = tmp.path().join("dev.prism.prism");

        migrate_dir(&legacy, &new);

        assert!(!new.exists(), "no legacy install ⇒ nothing to create");
    }

    /// The wiring test: `discover()` itself must perform the migration.
    /// Releases v1.0.0…v2.7.1 wrote under `com.marc27.prism`; without this
    /// call an upgrade silently logs the user out.
    ///
    /// On platforms where the qualifier is not part of the path (Linux/XDG)
    /// the two locations coincide and this passes trivially — which is
    /// correct: there is nothing to migrate there.
    #[test]
    fn discover_adopts_a_pre_rename_install() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prev_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", tmp.path());
        }

        let legacy = PrismPaths::for_bundle_id(LEGACY_BUNDLE_ID).expect("legacy dirs under $HOME");
        fs::create_dir_all(&legacy.config_dir).unwrap();
        let state = PrismCliState {
            credentials: Some(StoredCredentials {
                access_token: "at-from-the-old-bundle-id".into(),
                ..Default::default()
            }),
            preferred_python: None,
        };
        legacy.save_cli_state(&state).unwrap();

        let paths = PrismPaths::discover().unwrap();
        let carried = paths
            .load_cli_state()
            .unwrap()
            .credentials
            .expect("an upgrade must not log the user out");

        unsafe {
            match prev_home {
                Some(h) => env::set_var("HOME", h),
                None => env::remove_var("HOME"),
            }
        }

        assert_eq!(carried.access_token, "at-from-the-old-bundle-id");
        #[cfg(unix)]
        assert_eq!(
            mode_of(&paths.cli_state_path()),
            0o600,
            "the migrated credential file must still be owner-only"
        );
    }
}
