// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! F1c3 — federation lookup helpers.
//!
//! Two pieces wired to the trust primitives in [`super::federation`]:
//!
//! 1. [`PlatformPubkeyFetcher`] — fetches and caches the MARC27
//!    platform's root Ed25519 pubkey. The verifier in
//!    [`super::federation::verify_peer`] takes a `VerifyingKey`
//!    parameter; this is where it comes from in production.
//! 2. [`ActionRoleTable`] — maps action verbs to required roles.
//!    `verify_peer` takes `required_role: Option<&str>`; this is the
//!    lookup that produces it.
//!
//! These are *separate from `federation.rs` on purpose*: that module
//! is pure data + crypto with no I/O. Anything that touches network,
//! disk, or config — i.e. anything that could fail for non-crypto
//! reasons — lives here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use tokio::fs;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// ActionRoleTable
// ---------------------------------------------------------------------------

/// Maps action verbs to the role required to perform them cross-org.
///
/// The verifier resolves an action like `"inference.submit"` to a
/// required role like `"compute.invoke"`, then checks that the
/// peer's `PeerIdentity.roles` contains it.
///
/// `None` for an action means **no role required** (e.g. peer
/// liveness pings should not need a role gate).
///
/// Defaults baked into the binary cover the v1 cross-org actions.
/// Site operators override via
/// `~/.prism/federation/action-roles.toml`:
///
/// ```toml
/// [actions]
/// "inference.submit" = "ml.invoke"     # rename the role
/// "dataset.export"   = "data.read"     # new action
/// "peer.heartbeat"   = ""              # explicit "no role" (overrides absence)
/// ```
///
/// An empty-string role means "explicitly no role required" — useful
/// for shadowing a default.
#[derive(Debug, Clone, Default)]
pub struct ActionRoleTable {
    map: HashMap<String, Option<String>>,
}

impl ActionRoleTable {
    /// PRISM Fabric v1 default action → role mapping.
    ///
    /// Keep this list short. Each entry is something a remote node
    /// might genuinely ask another org's node to do today, with a
    /// role that mirrors the `prism-platform` role taxonomy. Adding
    /// an entry is a security-relevant change — it widens the set
    /// of cross-org calls that anyone with that role can make.
    pub fn defaults() -> Self {
        let mut map: HashMap<String, Option<String>> = HashMap::new();

        // Inference path (F6 demo: cross-site llama inference)
        map.insert("inference.submit".into(), Some("compute.invoke".into()));
        map.insert("inference.estimate".into(), Some("compute.invoke".into()));

        // Compute orchestration (F3: locality-aware placement)
        map.insert("compute.estimate".into(), Some("compute.invoke".into()));
        map.insert("compute.allocate".into(), Some("compute.invoke".into()));

        // Data
        map.insert("dataset.read".into(), Some("data.read".into()));
        map.insert("dataset.metadata".into(), Some("data.read".into()));

        // Workflow (cross-org coordination only)
        map.insert("workflow.execute".into(), Some("workflow.invoke".into()));

        // Liveness — explicit "no role required" so an override
        // can't accidentally tighten it without intent.
        map.insert("peer.heartbeat".into(), None);

        Self { map }
    }

    /// Build from an empty (no defaults) state. Useful for tests
    /// that want full control over the mapping.
    pub fn empty() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Parse a TOML override and merge it on top of the receiver.
    /// Empty-string roles are treated as "no role required."
    pub fn merge_toml(&mut self, toml_text: &str) -> Result<()> {
        let parsed: ActionRolesFile =
            toml::from_str(toml_text).context("invalid action-roles.toml")?;
        for (action, role) in parsed.actions {
            let value = if role.is_empty() { None } else { Some(role) };
            self.map.insert(action, value);
        }
        Ok(())
    }

    /// Load `~/.prism/federation/action-roles.toml` if present and
    /// merge into `self`. Missing file is not an error.
    pub async fn merge_user_config(&mut self, home: &Path) -> Result<()> {
        let path = home.join(".prism/federation/action-roles.toml");
        match fs::read_to_string(&path).await {
            Ok(text) => self
                .merge_toml(&text)
                .with_context(|| format!("loading {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(anyhow::Error::new(e).context(format!("reading {}", path.display()))),
        }
    }

    /// The role required for `action`. Returns:
    ///   * `Some(Some(role))` — explicit role required,
    ///   * `Some(None)` — explicit "no role required,"
    ///   * `None` — action not in the table at all (caller decides
    ///     whether to default-deny or pass through).
    pub fn lookup(&self, action: &str) -> Option<Option<&str>> {
        self.map.get(action).map(|opt| opt.as_deref())
    }

    /// Convenience: the required role as a single `Option<&str>`,
    /// flattening "not in table" and "no role required" into the
    /// same "no role" output. Use this when you want default-allow
    /// (cross-org actions you didn't anticipate get through with no
    /// role check); use [`Self::lookup`] when you want default-deny.
    pub fn required_role(&self, action: &str) -> Option<&str> {
        self.lookup(action).flatten()
    }
}

#[derive(Deserialize)]
struct ActionRolesFile {
    #[serde(default)]
    actions: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// PlatformPubkeyFetcher
// ---------------------------------------------------------------------------

/// Pluggable transport for the platform pubkey HTTP fetch. The
/// production impl wraps `prism_client::PlatformClient`; tests inject
/// a mock so we can test cache + refresh logic without network.
#[async_trait]
pub trait PlatformPubkeySource: Send + Sync {
    /// Return the platform's Ed25519 root pubkey as 32 raw bytes.
    async fn fetch_pubkey(&self) -> Result<[u8; 32]>;
}

/// Caches the MARC27 platform root pubkey on disk and in memory.
///
/// The pubkey changes only on platform-key rotation, which is rare
/// and externally signaled. We cache locally so the verifier doesn't
/// take a 50–200ms HTTP hit on every cross-org request.
///
/// Use [`Self::current`] for normal reads; use [`Self::refresh`] to
/// force a re-fetch (e.g. after a rotation announcement).
pub struct PlatformPubkeyFetcher {
    cache_path: PathBuf,
    source: Box<dyn PlatformPubkeySource>,
    in_memory: Mutex<Option<VerifyingKey>>,
}

impl PlatformPubkeyFetcher {
    /// Create with a custom transport. Production callers use
    /// [`Self::with_platform_client`].
    pub fn with_source(cache_path: PathBuf, source: Box<dyn PlatformPubkeySource>) -> Self {
        Self {
            cache_path,
            source,
            in_memory: Mutex::new(None),
        }
    }

    /// Default cache path: `~/.prism/federation/platform_pubkey.bin`.
    pub fn default_cache_path(home: &Path) -> PathBuf {
        home.join(".prism/federation/platform_pubkey.bin")
    }

    /// Return the cached pubkey if present, else fetch + cache.
    ///
    /// Read order:
    ///   1. In-memory (set on first successful read of this process)
    ///   2. Disk cache at `cache_path`
    ///   3. HTTP fetch via `source` (writes through to both layers)
    pub async fn current(&self) -> Result<VerifyingKey> {
        // Layer 1: memory
        if let Some(key) = *self.in_memory.lock().await {
            return Ok(key);
        }

        // Layer 2: disk cache
        if let Ok(bytes) = fs::read(&self.cache_path).await
            && bytes.len() == 32
        {
            let arr: [u8; 32] = bytes.as_slice().try_into().expect("len-checked above");
            if let Ok(key) = VerifyingKey::from_bytes(&arr) {
                *self.in_memory.lock().await = Some(key);
                return Ok(key);
            }
            // Bad cache file — fall through to refetch and overwrite.
            tracing::warn!(
                cache = %self.cache_path.display(),
                "platform pubkey cache file is corrupt; refetching"
            );
        }

        // Layer 3: network
        self.refresh().await
    }

    /// Force a network fetch and update both cache layers.
    pub async fn refresh(&self) -> Result<VerifyingKey> {
        let bytes = self
            .source
            .fetch_pubkey()
            .await
            .context("fetching platform pubkey")?;
        let key = VerifyingKey::from_bytes(&bytes)
            .map_err(|e| anyhow!("platform returned invalid Ed25519 pubkey: {e}"))?;

        // Persist to disk (best-effort: cache write failures don't
        // break verification, just degrade to network-on-every-boot).
        if let Some(parent) = self.cache_path.parent()
            && let Err(e) = fs::create_dir_all(parent).await
        {
            tracing::warn!(
                cache = %self.cache_path.display(),
                error = %e,
                "failed to create platform pubkey cache dir"
            );
        }
        if let Err(e) = fs::write(&self.cache_path, bytes).await {
            tracing::warn!(
                cache = %self.cache_path.display(),
                error = %e,
                "failed to persist platform pubkey to disk cache"
            );
        }

        *self.in_memory.lock().await = Some(key);
        Ok(key)
    }
}

// ---------------------------------------------------------------------------
// PlatformClient adapter (production transport)
// ---------------------------------------------------------------------------

/// Production transport: wraps `prism_client::PlatformClient` to
/// hit `/federation/platform-pubkey` and return the 32-byte key.
///
/// The platform endpoint returns `{"pubkey_hex": "<64 hex chars>"}`.
pub struct PlatformClientPubkeySource {
    client: prism_client::PlatformClient,
}

impl PlatformClientPubkeySource {
    pub fn new(client: prism_client::PlatformClient) -> Self {
        Self { client }
    }
}

#[derive(Deserialize)]
struct PubkeyResponse {
    pubkey_hex: String,
}

#[async_trait]
impl PlatformPubkeySource for PlatformClientPubkeySource {
    async fn fetch_pubkey(&self) -> Result<[u8; 32]> {
        let resp: PubkeyResponse = self
            .client
            .get("/federation/platform-pubkey")
            .await
            .context("GET /federation/platform-pubkey")?;
        let bytes = hex::decode(&resp.pubkey_hex).with_context(|| {
            format!(
                "invalid hex in platform pubkey response: {}",
                resp.pubkey_hex
            )
        })?;
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("expected 32-byte pubkey, got {} bytes", bytes.len()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    // ── ActionRoleTable ──────────────────────────────────────────────

    #[test]
    fn defaults_cover_v1_cross_org_actions() {
        let t = ActionRoleTable::defaults();
        assert_eq!(t.required_role("inference.submit"), Some("compute.invoke"));
        assert_eq!(t.required_role("dataset.read"), Some("data.read"));
        assert_eq!(t.required_role("workflow.execute"), Some("workflow.invoke"));
    }

    #[test]
    fn heartbeat_is_no_role_required_not_missing() {
        let t = ActionRoleTable::defaults();
        // Distinguishes "in table, no role" from "not in table".
        assert_eq!(t.lookup("peer.heartbeat"), Some(None));
        assert_eq!(t.lookup("totally.unknown.action"), None);
        // Both flatten to None for `required_role`.
        assert_eq!(t.required_role("peer.heartbeat"), None);
        assert_eq!(t.required_role("totally.unknown.action"), None);
    }

    #[test]
    fn merge_toml_adds_and_overrides() {
        let mut t = ActionRoleTable::defaults();
        t.merge_toml(
            r#"
            [actions]
            "dataset.export"   = "data.read"
            "inference.submit" = "ml.invoke"
            "peer.heartbeat"   = "liveness.ping"
            "#,
        )
        .unwrap();
        assert_eq!(t.required_role("dataset.export"), Some("data.read"));
        assert_eq!(t.required_role("inference.submit"), Some("ml.invoke"));
        assert_eq!(t.required_role("peer.heartbeat"), Some("liveness.ping"));
    }

    #[test]
    fn merge_toml_empty_string_means_no_role_required() {
        let mut t = ActionRoleTable::defaults();
        t.merge_toml(
            r#"
            [actions]
            "inference.submit" = ""
            "#,
        )
        .unwrap();
        // Now in the table but with no role.
        assert_eq!(t.lookup("inference.submit"), Some(None));
        assert_eq!(t.required_role("inference.submit"), None);
    }

    #[test]
    fn merge_toml_rejects_garbage() {
        let mut t = ActionRoleTable::defaults();
        let err = t.merge_toml("this is not toml [[ broken").unwrap_err();
        assert!(err.to_string().contains("action-roles.toml"));
    }

    #[tokio::test]
    async fn merge_user_config_silent_on_missing_file() {
        let home = TempDir::new().unwrap();
        let mut t = ActionRoleTable::defaults();
        // No file present — should be a no-op success, not an error.
        t.merge_user_config(home.path()).await.unwrap();
        // Defaults still intact.
        assert_eq!(t.required_role("inference.submit"), Some("compute.invoke"));
    }

    #[tokio::test]
    async fn merge_user_config_loads_when_file_exists() {
        let home = TempDir::new().unwrap();
        let cfg_dir = home.path().join(".prism/federation");
        fs::create_dir_all(&cfg_dir).await.unwrap();
        fs::write(
            cfg_dir.join("action-roles.toml"),
            r#"
            [actions]
            "dataset.export" = "data.read"
            "#,
        )
        .await
        .unwrap();

        let mut t = ActionRoleTable::defaults();
        t.merge_user_config(home.path()).await.unwrap();
        assert_eq!(t.required_role("dataset.export"), Some("data.read"));
    }

    // ── PlatformPubkeyFetcher ─────────────────────────────────────────

    /// Test source that records how many times it was hit and returns
    /// a fixed key. Lets us prove the cache is doing its job.
    struct CountingSource {
        key_bytes: [u8; 32],
        hits: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl PlatformPubkeySource for CountingSource {
        async fn fetch_pubkey(&self) -> Result<[u8; 32]> {
            self.hits.fetch_add(1, Ordering::SeqCst);
            Ok(self.key_bytes)
        }
    }

    fn make_key() -> ([u8; 32], VerifyingKey) {
        let signing = SigningKey::generate(&mut OsRng);
        let v = signing.verifying_key();
        (v.to_bytes(), v)
    }

    #[tokio::test]
    async fn current_fetches_once_then_uses_memory_cache() {
        let tmp = TempDir::new().unwrap();
        let (bytes, expected) = make_key();
        let hits = Arc::new(AtomicUsize::new(0));
        let fetcher = PlatformPubkeyFetcher::with_source(
            tmp.path().join("pk.bin"),
            Box::new(CountingSource {
                key_bytes: bytes,
                hits: hits.clone(),
            }),
        );

        // Two reads → one network fetch.
        let k1 = fetcher.current().await.unwrap();
        let k2 = fetcher.current().await.unwrap();
        assert_eq!(k1.to_bytes(), expected.to_bytes());
        assert_eq!(k2.to_bytes(), expected.to_bytes());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn current_uses_disk_cache_across_fetcher_instances() {
        let tmp = TempDir::new().unwrap();
        let cache_path = tmp.path().join("pk.bin");
        let (bytes, expected) = make_key();

        // First fetcher writes the cache.
        let hits1 = Arc::new(AtomicUsize::new(0));
        let f1 = PlatformPubkeyFetcher::with_source(
            cache_path.clone(),
            Box::new(CountingSource {
                key_bytes: bytes,
                hits: hits1.clone(),
            }),
        );
        f1.current().await.unwrap();
        assert_eq!(hits1.load(Ordering::SeqCst), 1);

        // Second fetcher (fresh in-memory state) hits disk, not network.
        let hits2 = Arc::new(AtomicUsize::new(0));
        let f2 = PlatformPubkeyFetcher::with_source(
            cache_path,
            Box::new(CountingSource {
                key_bytes: bytes,
                hits: hits2.clone(),
            }),
        );
        let k = f2.current().await.unwrap();
        assert_eq!(k.to_bytes(), expected.to_bytes());
        assert_eq!(
            hits2.load(Ordering::SeqCst),
            0,
            "second fetcher should hit disk cache, not network"
        );
    }

    #[tokio::test]
    async fn refresh_always_hits_network_and_updates_cache() {
        let tmp = TempDir::new().unwrap();
        let (bytes, _) = make_key();
        let hits = Arc::new(AtomicUsize::new(0));
        let fetcher = PlatformPubkeyFetcher::with_source(
            tmp.path().join("pk.bin"),
            Box::new(CountingSource {
                key_bytes: bytes,
                hits: hits.clone(),
            }),
        );

        fetcher.current().await.unwrap(); // 1
        fetcher.refresh().await.unwrap(); // 2
        fetcher.refresh().await.unwrap(); // 3
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn corrupt_disk_cache_falls_back_to_network() {
        let tmp = TempDir::new().unwrap();
        let cache_path = tmp.path().join("pk.bin");
        // Write a too-short file that won't deserialize as a key.
        fs::write(&cache_path, b"junk").await.unwrap();

        let (bytes, expected) = make_key();
        let hits = Arc::new(AtomicUsize::new(0));
        let fetcher = PlatformPubkeyFetcher::with_source(
            cache_path,
            Box::new(CountingSource {
                key_bytes: bytes,
                hits: hits.clone(),
            }),
        );

        let k = fetcher.current().await.unwrap();
        assert_eq!(k.to_bytes(), expected.to_bytes());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "corrupt cache should trigger one network fetch"
        );
    }
}
