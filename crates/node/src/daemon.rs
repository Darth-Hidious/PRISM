//! WebSocket daemon: register, heartbeat, job dispatch, reconnection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use prism_proto::{NodeCapabilities, NodeMessage, PlatformMessage};
use prism_runtime::{PlatformEndpoints, PrismPaths, StoredCredentials};
use serde::Serialize;
use sysinfo::System;
use tokio::signal;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::executor::{self, ContainerJobSpec, ContainerRuntime};
use crate::state::{self, ActiveJobRecord};

/// Options for the node daemon.
#[derive(Debug, Clone)]
pub struct DaemonOptions {
    pub name: String,
    pub visibility: String,
    pub price_per_hour_usd: Option<f64>,
    pub no_compute: bool,
    pub no_storage: bool,
    pub ssh: Option<SshCapability>,
    /// Broadcast this node for discovery (mDNS + platform). Without this, the node is private.
    pub broadcast: bool,
    /// Platform client for REST heartbeat + deregistration. Set when `--broadcast` is active.
    pub platform_client: Option<prism_client::PlatformClient>,
    /// Node ID returned by platform registration. Used for heartbeat + deregistration.
    pub platform_node_id: Option<String>,
    /// Path to the RBAC SQLite database for role sync.
    pub rbac_db_path: Option<PathBuf>,
    /// Organisation ID for role fetching.
    pub org_id: Option<String>,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            name: hostname(),
            visibility: "private".to_string(),
            price_per_hour_usd: None,
            no_compute: false,
            no_storage: false,
            ssh: None,
            broadcast: false,
            platform_client: None,
            platform_node_id: None,
            rbac_db_path: None,
            org_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshCapability {
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
}

impl SshCapability {
    fn endpoint(&self) -> String {
        match self.user.as_deref().filter(|value| !value.is_empty()) {
            Some(user) => format!("ssh://{user}@{}:{}", self.host, self.port),
            None => format!("ssh://{}:{}", self.host, self.port),
        }
    }
}

#[derive(Debug)]
struct RunningJobHandle {
    runtime: ContainerRuntime,
    handle: String,
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
    workspace_dir: PathBuf,
}

type RunningJobs = Arc<Mutex<HashMap<Uuid, RunningJobHandle>>>;

#[derive(Debug, Serialize)]
struct SignedServiceClaim<'a> {
    version: u8,
    service_kind: &'a str,
    owner_user_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    org_id: Option<&'a str>,
    node_name: &'a str,
    endpoint: String,
    issued_at: String,
}

/// Run the node daemon with reconnection logic.
pub async fn run_daemon(
    endpoints: &PlatformEndpoints,
    paths: &PrismPaths,
    options: DaemonOptions,
) -> Result<()> {
    let cli_state = paths.load_cli_state()?;
    let stored_credentials = cli_state.credentials.as_ref();

    let mut capabilities = crate::detect::probe_local_capabilities_async().await;
    capabilities.visibility = options.visibility.clone();
    capabilities.price_per_hour_usd = options.price_per_hour_usd;

    if options.no_compute {
        capabilities.services.retain(|s| s.kind != "compute");
    }
    if options.no_storage {
        capabilities.services.retain(|s| s.kind != "storage");
    }
    let (_node_secret, node_public) = crate::crypto::load_or_generate_key(&paths.state_dir)
        .context("failed to load/generate node keypair")?;
    let (node_signing_secret, node_signing_public) =
        crate::crypto::load_or_generate_signing_key(&paths.state_dir)
            .context("failed to load/generate node signing keypair")?;

    capabilities.labels.insert(
        "identity.signing_public_key".to_string(),
        crate::crypto::encode_signing_public_key(&node_signing_public),
    );

    let owner_user_id = stored_credentials
        .and_then(|creds| creds.user_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(ssh) = &options.ssh {
        let owner_user_id = owner_user_id.context(
            "SSH capability requires a logged-in PRISM user with a persisted user id. Run `prism login` first.",
        )?;
        let owner_org_id = stored_credentials
            .and_then(|creds| creds.org_id.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        advertise_ssh_service(
            &mut capabilities,
            ssh,
            owner_user_id,
            owner_org_id,
            &options.name,
            &node_signing_secret,
        )?;
    }

    let mut wire_capabilities = strip_paths_for_wire(&capabilities);
    wire_capabilities.public_key = Some(crate::crypto::encode_public_key(&node_public));

    let org_id = stored_credentials
        .and_then(|creds| creds.org_id.clone())
        .and_then(|id| Uuid::parse_str(&id).ok());

    tracing::info!(
        name = %options.name,
        cpu = wire_capabilities.cpu_cores,
        ram_gb = wire_capabilities.ram_gb,
        datasets = wire_capabilities.datasets.len(),
        models = wire_capabilities.models.len(),
        services = wire_capabilities.services.len(),
        "node probe complete"
    );

    state::clear_shutdown_request(&paths.state_dir);
    cleanup_orphaned_jobs(&paths.state_dir).await?;

    let pid_path = pid_file_path(paths);
    write_pid_file(&pid_path)?;
    let _pid_guard = PidFileGuard(pid_path);

    let platform_client = options.platform_client.clone();
    let platform_node_id = options.platform_node_id.clone();
    let rbac_db_path = options.rbac_db_path.clone();
    let rbac_org_id = options.org_id.clone();

    let mut delay_secs: u64 = 1;

    loop {
        let token = load_access_token(paths, endpoints).await?;

        match connect_and_run(
            endpoints,
            paths,
            &token,
            &options.name,
            org_id,
            &wire_capabilities,
            platform_client.as_ref(),
            platform_node_id.as_deref(),
            rbac_db_path.as_deref(),
            rbac_org_id.as_deref(),
        )
        .await
        {
            Ok(ShutdownReason::Graceful) => {
                // Deregister from platform on clean shutdown.
                if let (Some(client), Some(nid)) = (&platform_client, &platform_node_id) {
                    let registry = prism_client::node_registry::NodeRegistryClient::new(client);
                    if let Err(e) = registry.deregister_node(nid).await {
                        tracing::warn!(error = %e, "platform deregistration failed (non-fatal)");
                    } else {
                        tracing::info!("deregistered from MARC27 platform");
                    }
                }
                tracing::info!("node shut down gracefully");
                return Ok(());
            }
            Ok(ShutdownReason::TokenExpired) => {
                tracing::info!("token expired, refreshing and reconnecting");
                delay_secs = 1;
            }
            Err(e) => {
                tracing::warn!(error = %e, delay_secs, "disconnected, reconnecting");
                tokio::time::sleep(Duration::from_secs(delay_secs)).await;
                delay_secs = (delay_secs * 2).min(300);
            }
        }
    }
}

/// Stop a running node daemon in a cross-platform way.
pub fn stop_daemon(paths: &PrismPaths) -> Result<()> {
    let pid_path = pid_file_path(paths);
    if !pid_path.exists() {
        bail!(
            "no running node found (PID file not present at {})",
            pid_path.display()
        );
    }

    state::write_shutdown_request(&paths.state_dir)?;

    #[cfg(unix)]
    {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                let _ = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            }
        }
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if !pid_path.exists() {
            println!("Node stopped.");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    println!("Shutdown requested. Node is still draining or shutting down.");
    Ok(())
}

enum ShutdownReason {
    Graceful,
    TokenExpired,
}

/// Write data to a file with restricted permissions (0600 on Unix).
fn write_restricted_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(data)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)?;
    }
    Ok(())
}

/// Fetch managed LLM keys from the platform and write to local state.
///
/// Writes to `{state_dir}/llm_keys.json`. Non-fatal — logs warnings on failure.
async fn sync_llm_keys(client: &prism_client::PlatformClient, org_id: &str, state_dir: &Path) {
    let keys = match client.fetch_llm_keys(org_id).await {
        Ok(k) => k,
        Err(e) => {
            tracing::debug!(error = %e, "LLM key sync: platform endpoint unavailable");
            return;
        }
    };

    let keys_path = state_dir.join("llm_keys.json");
    match serde_json::to_string_pretty(&keys) {
        Ok(json) => {
            if let Err(e) = write_restricted_file(&keys_path, json.as_bytes()) {
                tracing::warn!(error = %e, path = %keys_path.display(), "LLM key sync: failed to write keys file");
            } else {
                tracing::info!(count = keys.len(), "LLM key sync complete");
            }
        }
        Err(e) => tracing::warn!(error = %e, "LLM key sync: failed to serialize keys"),
    }
}

/// Sync organisation roles from the platform into the local RBAC database.
///
/// Fetches member roles from the platform API and writes them to the local
/// SQLite RBAC engine. Non-fatal — logs warnings on failure.
async fn sync_roles_from_platform(
    client: &prism_client::PlatformClient,
    org_id: &str,
    rbac_db_path: &Path,
) {
    let members = match client.fetch_org_roles(org_id).await {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!(error = %e, "role sync: platform endpoint unavailable");
            return;
        }
    };

    let engine = match prism_core::rbac::RbacEngine::new(rbac_db_path) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "role sync: failed to open RBAC database");
            return;
        }
    };

    let mut synced = 0u32;
    for member in &members {
        let platform_role = match prism_core::rbac::PlatformRole::from_api_str(&member.role) {
            Some(r) => r,
            None => {
                tracing::debug!(user_id = %member.user_id, role = %member.role, "role sync: unknown platform role, skipping");
                continue;
            }
        };
        let local_role = platform_role.to_local_role();
        if let Err(e) = engine.assign_role(&member.user_id, local_role) {
            tracing::warn!(user_id = %member.user_id, error = %e, "role sync: failed to assign role");
        } else {
            synced += 1;
        }
    }
    // Revoke roles for users no longer on the platform
    let platform_ids: std::collections::HashSet<&str> =
        members.iter().map(|m| m.user_id.as_str()).collect();
    let mut revoked = 0u32;
    if let Ok(local_users) = engine.list_users() {
        for (uid, _role) in &local_users {
            if !platform_ids.contains(uid.as_str()) {
                if let Err(e) = engine.remove_role(uid) {
                    tracing::warn!(user_id = %uid, error = %e, "role sync: failed to revoke stale role");
                } else {
                    revoked += 1;
                }
            }
        }
    }

    tracing::info!(synced, revoked, total = members.len(), "role sync complete");
}

#[allow(clippy::too_many_arguments)]
async fn connect_and_run(
    endpoints: &PlatformEndpoints,
    paths: &PrismPaths,
    token: &str,
    name: &str,
    org_id: Option<Uuid>,
    capabilities: &NodeCapabilities,
    platform_client: Option<&prism_client::PlatformClient>,
    platform_node_id: Option<&str>,
    rbac_db_path: Option<&Path>,
    rbac_org_id: Option<&str>,
) -> Result<ShutdownReason> {
    let url = format!("{}?token={}", endpoints.node_ws, token);
    tracing::info!(url = %endpoints.node_ws, "connecting to platform");

    let (ws, _response) = tokio_tungstenite::connect_async(&url)
        .await
        .context("WebSocket connection failed")?;

    let (mut sink, mut stream) = ws.split();

    let register = NodeMessage::Register {
        name: name.to_string(),
        org_id,
        capabilities: Box::new(capabilities.clone()),
    };
    sink.send(Message::Text(serde_json::to_string(&register)?))
        .await?;

    tracing::info!("register message sent, waiting for confirmation");

    let mut node_id: Option<Uuid> = None;
    let mut heartbeat_interval = Duration::from_secs(30);

    if let Some(msg) = stream.next().await {
        let msg = msg.context("failed to read registration response")?;
        if let Message::Text(text) = msg {
            match serde_json::from_str::<PlatformMessage>(&text) {
                Ok(PlatformMessage::Registered {
                    node_id: id,
                    heartbeat_interval_secs,
                }) => {
                    node_id = Some(id);
                    heartbeat_interval = Duration::from_secs(heartbeat_interval_secs as u64);
                    println!("Node registered: {id}");
                    println!("  Heartbeat interval: {heartbeat_interval_secs}s");
                }
                Ok(PlatformMessage::Error { code, message }) => {
                    if code == "token_expired" {
                        return Ok(ShutdownReason::TokenExpired);
                    }
                    bail!("platform error: [{code}] {message}");
                }
                Ok(other) => tracing::warn!(?other, "unexpected first message from platform"),
                Err(e) => {
                    tracing::warn!(error = %e, raw = %text, "failed to parse platform message")
                }
            }
        }
    }

    let _node_id = node_id.context("did not receive Registered message")?;

    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<String>(128);
    let active_jobs = Arc::new(AtomicU32::new(0));
    let running_jobs: RunningJobs = Arc::new(Mutex::new(HashMap::new()));

    let mut heartbeat_timer = tokio::time::interval(heartbeat_interval);
    heartbeat_timer.tick().await;
    let mut shutdown_timer = tokio::time::interval(Duration::from_secs(1));
    shutdown_timer.tick().await;
    // REST heartbeat to platform (every 60s, independent of WS heartbeat).
    let mut rest_heartbeat_timer = tokio::time::interval(Duration::from_secs(60));
    rest_heartbeat_timer.tick().await;
    // Role sync from platform (every 5 min).
    let mut role_sync_timer = tokio::time::interval(Duration::from_secs(300));
    role_sync_timer.tick().await;

    // Initial sync on startup: roles + LLM keys.
    if let (Some(client), Some(oid)) = (platform_client, rbac_org_id) {
        if let Some(db_path) = rbac_db_path {
            sync_roles_from_platform(client, oid, db_path).await;
        }
        sync_llm_keys(client, oid, &paths.state_dir).await;
    }

    let mut system = System::new_all();

    loop {
        tokio::select! {
            _ = heartbeat_timer.tick() => {
                system.refresh_all();
                let cpu_load = system.global_cpu_usage() as f64;
                let used_mem = system.used_memory() as f64;
                let total_mem = system.total_memory() as f64;
                let memory_usage = if total_mem > 0.0 { used_mem / total_mem } else { 0.0 };
                let total_gpus: u32 = capabilities.gpus.iter().map(|g| g.count).sum();
                let active = active_jobs.load(Ordering::Relaxed);
                let gpus_in_use = if total_gpus > 0 { active.min(total_gpus) } else { 0 };
                let hb = NodeMessage::Heartbeat {
                    cpu_load,
                    memory_usage,
                    gpus_free: total_gpus.saturating_sub(gpus_in_use),
                    active_jobs: active,
                };
                sink.send(Message::Text(serde_json::to_string(&hb)?)).await?;
            }
            _ = rest_heartbeat_timer.tick() => {
                // REST heartbeat complements the WS heartbeat above.
                // The platform may not support this endpoint yet (see docs/marc27-api-discrepancies.md).
                // Failures are silently ignored — the WS heartbeat is authoritative.
                if let (Some(client), Some(nid)) = (platform_client, platform_node_id) {
                    let active = active_jobs.load(Ordering::Relaxed);
                    let registry = prism_client::node_registry::NodeRegistryClient::new(client);
                    if let Err(e) = registry.heartbeat(nid, "online", active).await {
                        tracing::debug!(error = %e, "REST heartbeat unavailable (WS heartbeat active)");
                    }
                }
            }
            _ = role_sync_timer.tick() => {
                if let (Some(client), Some(oid)) = (platform_client, rbac_org_id) {
                    if let Some(db_path) = rbac_db_path {
                        sync_roles_from_platform(client, oid, db_path).await;
                    }
                    sync_llm_keys(client, oid, &paths.state_dir).await;
                }
            }
            _ = shutdown_timer.tick() => {
                if state::shutdown_requested(&paths.state_dir) {
                    tracing::info!("shutdown request detected");
                    request_stop_all_jobs(&running_jobs).await;
                    sink.send(Message::Close(None)).await.ok();
                    return Ok(ShutdownReason::Graceful);
                }
            }
            Some(outgoing) = outgoing_rx.recv() => {
                sink.send(Message::Text(outgoing)).await?;
            }
            msg = stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        handle_platform_message(
                            paths,
                            &text,
                            &outgoing_tx,
                            &active_jobs,
                            &running_jobs,
                        ).await;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        sink.send(Message::Pong(data)).await?;
                    }
                    Some(Ok(Message::Close(_))) => {
                        tracing::info!("server closed connection");
                        return Err(anyhow!("server closed connection"));
                    }
                    Some(Err(e)) => return Err(e.into()),
                    None => return Err(anyhow!("WebSocket stream ended")),
                    _ => {}
                }
            }
            _ = signal::ctrl_c() => {
                tracing::info!("Ctrl-C received, shutting down");
                state::write_shutdown_request(&paths.state_dir).ok();
                request_stop_all_jobs(&running_jobs).await;
                sink.send(Message::Close(None)).await.ok();
                return Ok(ShutdownReason::Graceful);
            }
        }
    }
}

async fn handle_platform_message(
    paths: &PrismPaths,
    text: &str,
    outgoing_tx: &mpsc::Sender<String>,
    active_jobs: &Arc<AtomicU32>,
    running_jobs: &RunningJobs,
) {
    let msg: PlatformMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, raw = %text, "failed to parse platform message");
            return;
        }
    };

    match msg {
        PlatformMessage::Ping => tracing::debug!("received platform ping"),
        PlatformMessage::Registered { .. } => {}
        PlatformMessage::Error { code, message } => {
            tracing::error!(code = %code, message = %message, "platform error");
        }
        PlatformMessage::CancelJob { job_id } => {
            tracing::info!(%job_id, "cancel requested");
            let mut jobs = running_jobs.lock().await;
            if let Some(job) = jobs.get_mut(&job_id) {
                if let Some(cancel_tx) = job.cancel_tx.take() {
                    let _ = cancel_tx.send(());
                }
            }
        }
        PlatformMessage::SubmitJob {
            job_id,
            image,
            inputs,
            env_vars,
            gpu_type,
            timeout_secs,
        } => {
            tracing::info!(%job_id, %image, timeout = timeout_secs, "received job");

            let Some(runtime) = executor::resolve_container_runtime(
                std::env::var("PRISM_NODE_CONTAINER_RUNTIME")
                    .ok()
                    .as_deref(),
            ) else {
                send_msg(
                    outgoing_tx,
                    &NodeMessage::JobFailed {
                        job_id,
                        error: "no supported container runtime available (docker or podman)"
                            .to_string(),
                        output: None,
                        duration_secs: 0,
                    },
                )
                .await;
                return;
            };

            let workspace_dir = match prepare_job_workspace(
                &paths.state_dir,
                job_id,
                &image,
                &inputs,
                &env_vars,
                gpu_type.as_deref(),
                timeout_secs,
            ) {
                Ok(path) => path,
                Err(e) => {
                    send_msg(
                        outgoing_tx,
                        &NodeMessage::JobFailed {
                            job_id,
                            error: format!("failed to prepare job workspace: {e}"),
                            output: None,
                            duration_secs: 0,
                        },
                    )
                    .await;
                    return;
                }
            };

            let handle = executor::runtime_handle(job_id);
            if let Err(e) = state::register_active_job(
                &paths.state_dir,
                ActiveJobRecord {
                    job_id,
                    runtime: runtime.as_str().to_string(),
                    handle: handle.clone(),
                    workspace_dir: workspace_dir.display().to_string(),
                    image: image.clone(),
                    started_at: chrono::Utc::now(),
                },
            ) {
                send_msg(
                    outgoing_tx,
                    &NodeMessage::JobFailed {
                        job_id,
                        error: format!("failed to persist active job state: {e}"),
                        output: None,
                        duration_secs: 0,
                    },
                )
                .await;
                return;
            }

            let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
            running_jobs.lock().await.insert(
                job_id,
                RunningJobHandle {
                    runtime,
                    handle: handle.clone(),
                    cancel_tx: Some(cancel_tx),
                    workspace_dir: workspace_dir.clone(),
                },
            );

            let tx = outgoing_tx.clone();
            let jobs = active_jobs.clone();
            let running_jobs = running_jobs.clone();
            let state_dir = paths.state_dir.clone();
            jobs.fetch_add(1, Ordering::Relaxed);

            tokio::spawn(async move {
                send_msg(
                    &tx,
                    &NodeMessage::JobUpdate {
                        job_id,
                        progress: 0.0,
                        message: Some("Starting...".to_string()),
                    },
                )
                .await;

                let spec = ContainerJobSpec {
                    job_id,
                    image: image.clone(),
                    env_vars: env_vars.clone().into_iter().collect(),
                    gpu_type: gpu_type.clone(),
                    timeout_secs,
                    allow_network: false,
                    workspace_dir: workspace_dir.clone(),
                    memory_limit: None, // auto-detect from system RAM
                };

                let execute = executor::execute_container_job(runtime, &spec, |progress, msg| {
                    let tx = tx.clone();
                    let update = NodeMessage::JobUpdate {
                        job_id,
                        progress,
                        message: Some(msg.to_string()),
                    };
                    if let Ok(json) = serde_json::to_string(&update) {
                        tx.try_send(json).ok();
                    }
                });
                tokio::pin!(execute);

                let cancelled = tokio::select! {
                    _ = &mut cancel_rx => {
                        tracing::info!(%job_id, handle = %handle, "cancelling running job");
                        executor::cancel_container_job(runtime, &handle).await;
                        true
                    }
                    result = &mut execute => {
                        match result {
                            Ok(output) => {
                                if !output.log_lines.is_empty() {
                                    send_msg(&tx, &NodeMessage::JobLogs {
                                        job_id,
                                        lines: output.log_lines.clone(),
                                    }).await;
                                }
                                let result_json = serde_json::json!({
                                    "stdout_preview": output.stdout_preview,
                                    "stderr_preview": output.stderr_preview,
                                    "exit_code": output.exit_code,
                                });
                                send_msg(&tx, &NodeMessage::JobComplete {
                                    job_id,
                                    output: result_json,
                                    output_path: Some(output.output_path.display().to_string()),
                                    duration_secs: output.duration_secs,
                                }).await;
                            }
                            Err(e) => {
                                send_msg(&tx, &NodeMessage::JobFailed {
                                    job_id,
                                    error: e.to_string(),
                                    output: None,
                                    duration_secs: 0,
                                }).await;
                            }
                        }
                        false
                    }
                };

                if cancelled {
                    tracing::info!(%job_id, "job cancelled locally");
                }

                let mut jobs_map = running_jobs.lock().await;
                if let Some(job) = jobs_map.remove(&job_id) {
                    tracing::debug!(%job_id, workspace = %job.workspace_dir.display(), "job handle removed");
                }
                drop(jobs_map);

                if let Err(e) = state::remove_active_job(&state_dir, job_id) {
                    tracing::warn!(%job_id, error = %e, "failed to remove active job state");
                }
                jobs.fetch_sub(1, Ordering::Relaxed);
            });
        }
    }
}

async fn send_msg(tx: &mpsc::Sender<String>, msg: &NodeMessage) {
    if let Ok(json) = serde_json::to_string(msg) {
        tx.send(json).await.ok();
    }
}

async fn request_stop_all_jobs(running_jobs: &RunningJobs) {
    let mut jobs = running_jobs.lock().await;
    for (job_id, handle) in jobs.iter_mut() {
        tracing::info!(
            %job_id,
            runtime = handle.runtime.as_str(),
            handle = %handle.handle,
            "requesting job shutdown"
        );
        if let Some(cancel_tx) = handle.cancel_tx.take() {
            let _ = cancel_tx.send(());
        }
    }
}

fn prepare_job_workspace(
    state_dir: &Path,
    job_id: Uuid,
    image: &str,
    inputs: &serde_json::Value,
    env_vars: &std::collections::BTreeMap<String, String>,
    gpu_type: Option<&str>,
    timeout_secs: u64,
) -> Result<PathBuf> {
    let workspace = state::ensure_workspace(state_dir, job_id)?;
    let metadata = serde_json::json!({
        "job_id": job_id,
        "image": image,
        "gpu_type": gpu_type,
        "timeout_secs": timeout_secs,
        "created_at": chrono::Utc::now(),
    });
    std::fs::write(
        state::inputs_path(&workspace),
        serde_json::to_vec_pretty(inputs)?,
    )
    .context("failed to write inputs.json")?;
    std::fs::write(
        state::metadata_path(&workspace),
        serde_json::to_vec_pretty(&serde_json::json!({
            "job": metadata,
            "env_keys": env_vars.keys().collect::<Vec<_>>(),
        }))?,
    )
    .context("failed to write metadata.json")?;
    Ok(workspace)
}

async fn cleanup_orphaned_jobs(state_dir: &Path) -> Result<()> {
    for record in state::active_jobs(state_dir)? {
        tracing::warn!(
            job_id = %record.job_id,
            runtime = %record.runtime,
            handle = %record.handle,
            "cleaning up orphaned job from previous node session"
        );
        executor::cleanup_orphaned_job(&record.runtime, &record.handle).await;
        state::remove_active_job(state_dir, record.job_id).ok();
    }
    Ok(())
}

async fn load_access_token(paths: &PrismPaths, endpoints: &PlatformEndpoints) -> Result<String> {
    let state = paths.load_cli_state()?;
    let creds = state
        .credentials
        .as_ref()
        .context("not logged in — run `prism login` first")?;

    if let Some(expires_at) = creds.expires_at {
        if chrono::Utc::now() >= expires_at {
            tracing::info!("access token expired, refreshing");
            return refresh_token(paths, endpoints, creds).await;
        }
    }

    Ok(creds.access_token.clone())
}

async fn refresh_token(
    paths: &PrismPaths,
    endpoints: &PlatformEndpoints,
    creds: &StoredCredentials,
) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let resp = client
        .post(format!("{}/auth/refresh", endpoints.api_base))
        .json(&serde_json::json!({ "refresh_token": creds.refresh_token }))
        .send()
        .await
        .context("failed to refresh token")?
        .error_for_status()
        .context("token refresh returned error")?;

    #[derive(serde::Deserialize)]
    struct RefreshResponse {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: Option<u64>,
    }

    let refreshed: RefreshResponse = resp.json().await?;
    let mut state = paths.load_cli_state()?;
    if let Some(stored) = state.credentials.as_mut() {
        stored.access_token = refreshed.access_token.clone();
        if let Some(rt) = refreshed.refresh_token {
            stored.refresh_token = rt;
        }
        stored.expires_at = refreshed.expires_in.and_then(|secs| {
            chrono::Utc::now().checked_add_signed(chrono::Duration::seconds(secs as i64))
        });
        paths.save_cli_state(&state)?;
    }

    Ok(refreshed.access_token)
}

fn hostname() -> String {
    System::host_name().unwrap_or_else(|| "prism-node".to_string())
}

fn pid_file_path(paths: &PrismPaths) -> PathBuf {
    paths.state_dir.join("node.pid")
}

fn write_pid_file(path: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(path.parent().unwrap_or(path))?;
    let pid = std::process::id();
    std::fs::write(path, pid.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

struct PidFileGuard(PathBuf);

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

fn strip_paths_for_wire(caps: &NodeCapabilities) -> NodeCapabilities {
    let mut wire = caps.clone();

    for ds in &mut wire.datasets {
        ds.path = ds.name.clone();
    }
    for model in &mut wire.models {
        model.path = model.name.clone();
    }
    for svc in &mut wire.services {
        if let Some(ep) = &svc.endpoint {
            if !ep.starts_with("http://")
                && !ep.starts_with("https://")
                && !ep.starts_with("ssh://")
            {
                svc.endpoint = None;
            }
        }
    }

    wire
}

fn advertise_ssh_service(
    caps: &mut NodeCapabilities,
    ssh: &SshCapability,
    owner_user_id: &str,
    org_id: Option<&str>,
    node_name: &str,
    signing_key: &ed25519_dalek::SigningKey,
) -> Result<()> {
    caps.services.retain(|service| service.kind != "ssh");
    caps.labels
        .insert("ssh.enabled".to_string(), "true".to_string());
    caps.labels.insert(
        "ssh.consent_mode".to_string(),
        "owner_user_id_required".to_string(),
    );
    caps.labels
        .insert("ssh.owner_user_id".to_string(), owner_user_id.to_string());
    let claim = SignedServiceClaim {
        version: 1,
        service_kind: "ssh",
        owner_user_id,
        org_id,
        node_name,
        endpoint: ssh.endpoint(),
        issued_at: chrono::Utc::now().to_rfc3339(),
    };
    let claim_payload = serde_json::to_vec(&claim).context("failed to serialize ssh claim")?;
    let claim_signature = crate::crypto::sign_bytes(signing_key, &claim_payload);
    caps.labels.insert(
        "ssh.claim_payload".to_string(),
        base64::engine::general_purpose::STANDARD.encode(claim_payload),
    );
    caps.labels
        .insert("ssh.claim_signature".to_string(), claim_signature);
    caps.labels
        .insert("ssh.claim_algorithm".to_string(), "ed25519".to_string());
    caps.labels
        .insert("ssh.claim_version".to_string(), "1".to_string());
    caps.services.push(prism_proto::NodeService {
        kind: "ssh".to_string(),
        name: "SSH Access (owner consent required)".to_string(),
        status: "ready".to_string(),
        endpoint: Some(ssh.endpoint()),
        model: None,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use tempfile::TempDir;

    #[test]
    fn strip_paths_for_wire_redacts_dataset_and_model_paths() {
        let caps = NodeCapabilities {
            gpus: vec![],
            cpu_cores: 8,
            ram_gb: 32,
            disk_gb: 100,
            software: vec![],
            container_runtime: Some("docker".to_string()),
            docker: true,
            scheduler: None,
            labels: std::collections::BTreeMap::new(),
            storage_available_gb: 80,
            datasets: vec![prism_proto::DatasetInfo {
                name: "dataset".to_string(),
                path: "/secret/data.csv".to_string(),
                size_gb: 1.0,
                entries: None,
                format: Some("csv".to_string()),
            }],
            models: vec![prism_proto::ModelInfo {
                name: "model".to_string(),
                path: "/secret/model.onnx".to_string(),
                format: Some("onnx".to_string()),
                size_gb: Some(2.0),
            }],
            services: vec![
                prism_proto::NodeService {
                    kind: "llm".to_string(),
                    name: "Ollama".to_string(),
                    status: "ready".to_string(),
                    endpoint: Some("/private/socket".to_string()),
                    model: None,
                },
                prism_proto::NodeService {
                    kind: "ssh".to_string(),
                    name: "SSH Access".to_string(),
                    status: "ready".to_string(),
                    endpoint: Some("ssh://sid@node.example.com:2222".to_string()),
                    model: None,
                },
            ],
            visibility: "private".to_string(),
            price_per_hour_usd: None,
            public_key: None,
        };

        let wire = strip_paths_for_wire(&caps);
        assert_eq!(wire.datasets[0].path, "dataset");
        assert_eq!(wire.models[0].path, "model");
        assert_eq!(wire.services[0].endpoint, None);
        assert_eq!(
            wire.services[1].endpoint.as_deref(),
            Some("ssh://sid@node.example.com:2222")
        );
    }

    #[test]
    fn prepare_job_workspace_writes_inputs_and_metadata() {
        let tmp = TempDir::new().unwrap();
        let job_id = Uuid::new_v4();
        let workspace = prepare_job_workspace(
            tmp.path(),
            job_id,
            "marc27/test:latest",
            &serde_json::json!({"x": 1}),
            &std::collections::BTreeMap::new(),
            Some("A100-80GB"),
            60,
        )
        .unwrap();

        assert!(state::inputs_path(&workspace).exists());
        assert!(state::metadata_path(&workspace).exists());
    }

    #[test]
    fn advertise_ssh_service_adds_endpoint() {
        let mut caps = NodeCapabilities {
            gpus: vec![],
            cpu_cores: 8,
            ram_gb: 32,
            disk_gb: 100,
            software: vec![],
            container_runtime: Some("docker".to_string()),
            docker: true,
            scheduler: None,
            labels: std::collections::BTreeMap::new(),
            storage_available_gb: 80,
            datasets: vec![],
            models: vec![],
            services: vec![],
            visibility: "private".to_string(),
            price_per_hour_usd: None,
            public_key: None,
        };

        let signing_tmp = TempDir::new().unwrap();
        let (signing_key, signing_public) =
            crate::crypto::load_or_generate_signing_key(signing_tmp.path()).unwrap();

        advertise_ssh_service(
            &mut caps,
            &SshCapability {
                host: "node.example.com".to_string(),
                port: 2222,
                user: Some("sid".to_string()),
            },
            "user_123",
            Some("00000000-0000-4000-b000-000000000001"),
            "test-node",
            &signing_key,
        )
        .unwrap();

        let ssh = caps
            .services
            .iter()
            .find(|service| service.kind == "ssh")
            .unwrap();
        assert_eq!(
            ssh.endpoint.as_deref(),
            Some("ssh://sid@node.example.com:2222")
        );
        assert_eq!(ssh.name, "SSH Access (owner consent required)");
        assert_eq!(
            caps.labels.get("ssh.enabled").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            caps.labels.get("ssh.consent_mode").map(String::as_str),
            Some("owner_user_id_required")
        );
        assert_eq!(
            caps.labels.get("ssh.owner_user_id").map(String::as_str),
            Some("user_123")
        );
        assert_eq!(
            caps.labels.get("ssh.claim_algorithm").map(String::as_str),
            Some("ed25519")
        );
        let claim_payload = base64::engine::general_purpose::STANDARD
            .decode(caps.labels.get("ssh.claim_payload").unwrap())
            .unwrap();
        let signature = caps.labels.get("ssh.claim_signature").unwrap();
        crate::crypto::verify_signature(&signing_public, &claim_payload, signature).unwrap();
    }
}
