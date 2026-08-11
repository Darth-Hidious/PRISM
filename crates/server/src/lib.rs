// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Embedded Axum HTTP/WebSocket server for PRISM nodes.
//!
//! Exposes a REST API and WebSocket endpoint for:
//!
//! - Node status and health monitoring.
//! - Dataset management and ingestion triggers.
//! - Graph and semantic queries.
//! - Tool execution and listing.
//! - Mesh discovery and subscription management.
//! - User management and audit log access.
//!
//! All routes are role-gated via the [`middleware`] layer. The server also hosts
//! the embedded web dashboard SPA (future).

pub mod handlers;
pub mod middleware;
pub mod router;
pub mod ws;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;
use uuid::Uuid;

use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tracing::{error, info};

/// Events broadcast to connected WebSocket clients for live dashboard updates.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", content = "data")]
pub enum WsEvent {
    /// Periodic node status snapshot (uptime, service health).
    NodeStatusUpdate {
        uptime_secs: u64,
        services: Vec<ServiceSnapshot>,
    },
    /// A mesh peer was added or removed.
    MeshPeerChange {
        action: &'static str,
        node_id: String,
        name: String,
    },
    /// A new audit log entry was recorded.
    AuditEntry {
        timestamp: String,
        user: String,
        action: String,
    },
}

/// Snapshot of a service for WebSocket broadcast.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServiceSnapshot {
    pub name: String,
    pub port: u16,
    pub healthy: bool,
}

const OFFLINE_SESSION_TTL_SECS: i64 = 24 * 60 * 60;
const OFFLINE_SESSION_STORE_FILE: &str = "offline_sessions.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct OfflineSessionRecord {
    expires_at_unix: i64,
}

#[derive(Default)]
struct OfflineSessionStore {
    records: BTreeMap<String, OfflineSessionRecord>,
    loaded_path: Option<PathBuf>,
}

pub(crate) struct OfflineSessionCapability {
    pub token: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// Shared state for the PRISM node HTTP server.
pub struct NodeState {
    pub node_name: String,
    pub started_at: Instant,
    pub services: Mutex<Vec<ServiceEntry>>,
    /// Path to the audit log SQLite database.
    pub audit_db_path: Option<PathBuf>,
    /// Path to the RBAC SQLite database.
    pub rbac_db_path: Option<PathBuf>,
    /// Path to the session SQLite database.
    pub session_db_path: Option<PathBuf>,
    /// Path to the bundled Turso provenance store served by `/api/query`.
    /// `None` means the production default (`~/.prism/provenance.db`);
    /// tests running several nodes in one process give each its own store.
    pub provenance_db_path: Option<PathBuf>,
    /// Server-issued bearer capabilities for standalone mode. Active tokens
    /// are persisted beside the server's other state so a solo session can
    /// resume after restart; each record carries its real expiry and logout
    /// removes it durably.
    offline_session_tokens: RwLock<OfflineSessionStore>,
    /// In-memory tool registry (populated by scanning tool directories).
    pub tool_registry: RwLock<prism_core::registry::ToolRegistry>,
    /// Mesh handle for peer discovery.
    pub mesh: RwLock<prism_mesh::MeshHandle>,
    /// Subscription manager for pub/sub.
    pub subscriptions: Arc<RwLock<prism_mesh::subscription::SubscriptionManager>>,
    /// LLM config for the embedded chat/agent service (Ollama or compatible).
    pub llm: Option<prism_ingest::LlmConfig>,
    /// Platform API client (set when node is registered with MARC27 platform).
    pub platform_client: Option<prism_client::PlatformClient>,
    /// Explicit verifier for bearer credentials presented when minting a
    /// remote PRISM session. `None` fails closed; it never implies MARC27.
    /// The configuration's custom `Debug` implementation redacts provider
    /// keys.
    pub identity_verifier: Option<prism_client::auth::IdentityVerifierConfig>,
    /// Identity returned by the linked platform credential's `/users/me`.
    /// Cached after the first owner-authorized request; it is never supplied
    /// by an HTTP caller.
    pub platform_owner_id: OnceLock<String>,
    /// Broadcast channel for live WebSocket updates to the dashboard.
    pub ws_broadcast: broadcast::Sender<String>,
    /// Current number of active WebSocket connections (used to enforce concurrency limit).
    pub ws_connections: AtomicUsize,
    /// Kafka producer for publishing mesh messages to other nodes (set once after init).
    pub kafka_producer: OnceLock<Arc<prism_mesh::kafka::MeshKafkaProducer>>,
    /// This node's unique ID on the mesh (set once after mesh init).
    pub node_id: OnceLock<Uuid>,
    /// Federated query client for dispatching queries to mesh peers.
    pub federation: OnceLock<prism_mesh::federated_query::FederatedQuery>,
    /// Conversational agent service backing `POST /api/chat` — the SAME
    /// agent loop the TUI backend runs, exposed over HTTP. Set once after
    /// boot when an LLM is configured and the Python tool server spawns;
    /// unset ⇒ the chat endpoints return 503.
    pub chat: OnceLock<Arc<prism_agent::service::ChatService>>,
    /// Signed cross-org audit envelopes (F5). Set at boot when the node
    /// has an identity + signing key; `None` (e.g. tests) disables
    /// emission. Shared with the platform-relay handler so both cross-org
    /// receive paths write to one identity + one append-only log.
    pub federation_audit: Option<Arc<prism_audit::AuditEmitter>>,
}

/// A running service tracked by the server.
pub struct ServiceEntry {
    pub name: String,
    pub port: u16,
    pub healthy: bool,
}

impl NodeState {
    pub fn new(node_name: String) -> Self {
        let (ws_broadcast, _) = broadcast::channel(256);
        Self {
            node_name,
            started_at: Instant::now(),
            services: Mutex::new(Vec::new()),
            audit_db_path: None,
            rbac_db_path: None,
            session_db_path: None,
            provenance_db_path: None,
            offline_session_tokens: RwLock::new(OfflineSessionStore::default()),
            tool_registry: RwLock::new(prism_core::registry::ToolRegistry::new()),
            mesh: RwLock::new(prism_mesh::MeshHandle::Offline),
            subscriptions: Arc::new(RwLock::new(
                prism_mesh::subscription::SubscriptionManager::new(),
            )),
            llm: None,
            platform_client: None,
            identity_verifier: None,
            platform_owner_id: OnceLock::new(),
            ws_broadcast,
            ws_connections: AtomicUsize::new(0),
            kafka_producer: OnceLock::new(),
            node_id: OnceLock::new(),
            federation: OnceLock::new(),
            chat: OnceLock::new(),
            federation_audit: None,
        }
    }

    fn offline_session_store_path(&self) -> Option<PathBuf> {
        self.audit_db_path
            .as_deref()
            .and_then(Path::parent)
            .map(|dir| dir.join(OFFLINE_SESSION_STORE_FILE))
    }

    fn hydrate_offline_sessions(&self, store: &mut OfflineSessionStore) -> std::io::Result<()> {
        let Some(path) = self.offline_session_store_path() else {
            return Ok(());
        };
        if store.loaded_path.as_ref() == Some(&path) {
            return Ok(());
        }

        let records = match std::fs::read(&path) {
            Ok(data) => serde_json::from_slice::<BTreeMap<String, OfflineSessionRecord>>(&data)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error),
        };
        store.records.extend(records);
        store.loaded_path = Some(path);
        Ok(())
    }

    fn persist_offline_sessions(&self, store: &OfflineSessionStore) -> std::io::Result<()> {
        let Some(path) = self.offline_session_store_path() else {
            return Ok(());
        };
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(
            ".{OFFLINE_SESSION_STORE_FILE}.{}.tmp",
            Uuid::new_v4()
        ));
        let data = serde_json::to_vec_pretty(&store.records)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        if let Err(error) = (|| {
            file.write_all(&data)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &path)
        })() {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn mint_offline_session(&self) -> std::io::Result<OfflineSessionCapability> {
        let token = Uuid::new_v4().to_string();
        let expires_at_unix = chrono::Utc::now().timestamp() + OFFLINE_SESSION_TTL_SECS;
        let expires_at = chrono::DateTime::from_timestamp(expires_at_unix, 0)
            .expect("24-hour session expiry is in range");
        let mut store = self
            .offline_session_tokens
            .write()
            .unwrap_or_else(|error| error.into_inner());
        self.hydrate_offline_sessions(&mut store)?;
        let now = chrono::Utc::now().timestamp();
        store
            .records
            .retain(|_, record| record.expires_at_unix > now);
        store
            .records
            .insert(token.clone(), OfflineSessionRecord { expires_at_unix });
        if let Err(error) = self.persist_offline_sessions(&store) {
            store.records.remove(&token);
            return Err(error);
        }
        Ok(OfflineSessionCapability { token, expires_at })
    }

    pub(crate) fn is_valid_offline_session_token(&self, token: &str) -> bool {
        let mut store = self
            .offline_session_tokens
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if let Err(error) = self.hydrate_offline_sessions(&mut store) {
            tracing::error!(error = %error, "failed to load standalone session capabilities");
            return false;
        }

        let now = chrono::Utc::now().timestamp();
        let before = store.records.len();
        store
            .records
            .retain(|_, record| record.expires_at_unix > now);
        if store.records.len() != before
            && let Err(error) = self.persist_offline_sessions(&store)
        {
            tracing::warn!(error = %error, "failed to prune expired standalone sessions");
        }
        store.records.contains_key(token)
    }

    pub(crate) fn revoke_offline_session_token(&self, token: &str) -> std::io::Result<bool> {
        let mut store = self
            .offline_session_tokens
            .write()
            .unwrap_or_else(|error| error.into_inner());
        self.hydrate_offline_sessions(&mut store)?;
        let Some(record) = store.records.remove(token) else {
            return Ok(false);
        };
        if let Err(error) = self.persist_offline_sessions(&store) {
            store.records.insert(token.to_string(), record);
            return Err(error);
        }
        Ok(true)
    }

    /// Broadcast a [`WsEvent`] to all connected WebSocket clients.
    pub fn broadcast(&self, event: &WsEvent) {
        if let Ok(json) = serde_json::to_string(event) {
            // Ignore error — means no active receivers.
            let _ = self.ws_broadcast.send(json);
        }
    }

    /// Write an audit entry AND broadcast it to WebSocket clients.
    pub fn audit_and_broadcast(&self, entry: &prism_core::audit::AuditEntry) {
        // Write to SQLite
        if let Some(ref db_path) = self.audit_db_path
            && let Ok(log) = prism_core::audit::AuditLog::new(db_path)
            && let Err(e) = log.log(entry)
        {
            tracing::warn!(error = %e, "failed to write audit entry");
        }
        // Broadcast to WebSocket clients
        self.broadcast(&WsEvent::AuditEntry {
            timestamp: entry.timestamp.to_rfc3339(),
            user: entry.user_id.clone(),
            action: format!("{}", entry.action),
        });
    }

    /// Update service list from orchestrator handles.
    pub fn update_services(&self, entries: Vec<ServiceEntry>) {
        *self.services.lock().unwrap_or_else(|e| e.into_inner()) = entries;
    }
}

/// Start the Axum HTTP server on the given port. Returns the actual bound address.
///
/// Also spawns a background task that broadcasts [`WsEvent::NodeStatusUpdate`]
/// every 5 seconds to keep the dashboard live.
pub async fn start_server(
    state: Arc<NodeState>,
    port: u16,
) -> anyhow::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let app = router::build_router(state.clone());
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    let addr = listener.local_addr()?;
    info!(%addr, "Dashboard server listening");

    // Periodic status broadcaster
    let ticker_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            let uptime_secs = ticker_state.started_at.elapsed().as_secs();
            let services = ticker_state
                .services
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .map(|s| ServiceSnapshot {
                    name: s.name.clone(),
                    port: s.port,
                    healthy: s.healthy,
                })
                .collect();
            ticker_state.broadcast(&WsEvent::NodeStatusUpdate {
                uptime_secs,
                services,
            });
        }
    });

    let handle = tokio::spawn(async move {
        // ConnectInfo lets handlers distinguish loopback from remote callers
        // (the session-mint gate treats them differently).
        if let Err(e) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        {
            error!(%e, "Server exited with error");
        }
    });

    Ok((addr, handle))
}

/// Unit-test binary: point provenance writes at a scratch store BEFORE any
/// test (or thread) starts. The chat-handler tests drive the REAL agent
/// loop, which otherwise opens the user's live `~/.prism/provenance.db` —
/// prism-agent's `test-guard` (armed for this crate's tests via
/// dev-dependencies) aborts on that. Integration binaries do the same
/// through `tests/common/mod.rs`.
// SAFETY (ctor): pre-main; the body only touches the environment, which is
// exactly what must happen before any thread can exist.
#[cfg(test)]
#[ctor::ctor(unsafe)]
fn isolate_provenance_store_for_unit_tests() {
    prism_agent::testsupport::isolate_provenance_store_pre_main();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_session_capability_survives_server_restart() {
        let state_dir = tempfile::tempdir().expect("state directory");
        let mut first_server = NodeState::new("offline-node".into());
        first_server.audit_db_path = Some(state_dir.path().join("audit.db"));
        let capability = first_server
            .mint_offline_session()
            .expect("mint standalone session");
        assert!(first_server.is_valid_offline_session_token(&capability.token));
        drop(first_server);

        let mut restarted_server = NodeState::new("offline-node".into());
        restarted_server.audit_db_path = Some(state_dir.path().join("audit.db"));
        assert!(
            restarted_server.is_valid_offline_session_token(&capability.token),
            "a standalone bearer must remain valid across a server restart"
        );
    }

    #[test]
    fn offline_session_capability_expires() {
        let state = NodeState::new("offline-node".into());
        state
            .offline_session_tokens
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .records
            .insert(
                "expired-token".into(),
                OfflineSessionRecord {
                    expires_at_unix: chrono::Utc::now().timestamp() - 1,
                },
            );

        assert!(
            !state.is_valid_offline_session_token("expired-token"),
            "expired standalone bearers must be rejected"
        );
    }

    #[test]
    fn node_state_update_services() {
        let state = NodeState::new("test-node".into());
        assert!(state.services.lock().unwrap().is_empty());
        state.update_services(vec![ServiceEntry {
            name: "kafka".into(),
            port: 9092,
            healthy: true,
        }]);
        assert_eq!(state.services.lock().unwrap().len(), 1);
    }
}
