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

#[derive(Debug, Clone)]
enum DeploymentBackend {
    Runtime {
        runtime_url: String,
    },
    Container {
        runtime: ContainerRuntime,
        handle: String,
    },
}

#[derive(Debug)]
struct RunningDeploymentHandle {
    backend: DeploymentBackend,
    stop_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

type RunningDeployments = Arc<Mutex<HashMap<Uuid, RunningDeploymentHandle>>>;

#[derive(Debug, Clone)]
struct DeploymentLaunchConfig {
    port: u16,
    health_path: String,
    startup_timeout_secs: u64,
    framework: String,
    command: Option<Vec<String>>,
    endpoint_url: Option<String>,
    public_base_url: Option<String>,
    public_host: Option<String>,
}

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
    cleanup_orphaned_deployments(&paths.state_dir).await?;

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
    let running_deployments: RunningDeployments = Arc::new(Mutex::new(HashMap::new()));

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
                // REST heartbeat complements the WS heartbeat above so the DB-backed node
                // registry stays fresh even if downstream consumers are reading REST state.
                // Failures remain non-fatal because the WS heartbeat is still authoritative.
                if let (Some(client), Some(nid)) = (platform_client, platform_node_id) {
                    let active = active_jobs.load(Ordering::Relaxed);
                    let registry = prism_client::node_registry::NodeRegistryClient::new(client);
                    if let Err(e) = registry.heartbeat(nid, "online", active).await {
                        tracing::debug!(error = %e, "REST heartbeat failed (WS heartbeat still active)");
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
                    request_stop_all_deployments(&running_deployments).await;
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
                            &running_deployments,
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
                request_stop_all_deployments(&running_deployments).await;
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
    running_deployments: &RunningDeployments,
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
        PlatformMessage::DeployModel {
            deployment_id,
            image,
            env_vars,
            gpu_type,
            deploy_config,
        } => {
            tracing::info!(%deployment_id, image = %image, "received deployment request");
            if let Err(error) = start_deployment(
                paths,
                deployment_id,
                image,
                env_vars,
                gpu_type,
                deploy_config,
                outgoing_tx,
                running_deployments,
            )
            .await
            {
                tracing::error!(%deployment_id, error = %error, "deployment start failed");
                send_msg(
                    outgoing_tx,
                    &NodeMessage::DeploymentStopped {
                        deployment_id,
                        reason: error.to_string(),
                    },
                )
                .await;
            }
        }
        PlatformMessage::StopDeployment { deployment_id } => {
            tracing::info!(%deployment_id, "deployment stop requested");
            if let Err(error) = stop_deployment(
                paths,
                deployment_id,
                "user_request",
                outgoing_tx,
                running_deployments,
            )
            .await
            {
                tracing::warn!(%deployment_id, error = %error, "deployment stop failed");
                send_msg(
                    outgoing_tx,
                    &NodeMessage::DeploymentStopped {
                        deployment_id,
                        reason: format!("stop_failed: {error}"),
                    },
                )
                .await;
            }
        }
    }
}

async fn send_msg(tx: &mpsc::Sender<String>, msg: &NodeMessage) {
    if let Ok(json) = serde_json::to_string(msg) {
        tx.send(json).await.ok();
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_deployment(
    paths: &PrismPaths,
    deployment_id: Uuid,
    image: String,
    env_vars: std::collections::BTreeMap<String, String>,
    gpu_type: Option<String>,
    deploy_config: serde_json::Value,
    outgoing_tx: &mpsc::Sender<String>,
    running_deployments: &RunningDeployments,
) -> Result<()> {
    let config = parse_deployment_launch_config(&deploy_config);
    let endpoint_url = resolve_public_endpoint_url(config.port, &config);
    let local_health_url = format!("http://127.0.0.1:{}{}", config.port, config.health_path);

    let backend = if looks_like_weights_source(&image) {
        let runtime_url = deployment_runtime_url();
        start_runtime_deployment(
            &runtime_url,
            deployment_id,
            &image,
            &env_vars,
            gpu_type.is_some(),
            &config,
        )
        .await?;
        DeploymentBackend::Runtime { runtime_url }
    } else {
        let runtime = executor::resolve_container_runtime(
            std::env::var("PRISM_NODE_CONTAINER_RUNTIME")
                .ok()
                .as_deref(),
        )
        .context("no supported container runtime available for deployment")?;
        let handle = executor::start_container_deployment(
            runtime,
            &executor::ContainerDeploymentSpec {
                deployment_id,
                image: image.clone(),
                env_vars: env_vars.clone(),
                gpu_type: gpu_type.clone(),
                port: config.port,
                command: config.command.clone(),
                memory_limit: None,
            },
        )
        .await?;
        DeploymentBackend::Container { runtime, handle }
    };

    let (backend_kind, handle, runtime_url) = describe_backend_for_state(&backend, deployment_id);
    state::register_active_deployment(
        &paths.state_dir,
        state::ActiveDeploymentRecord {
            deployment_id,
            backend: backend_kind,
            handle,
            runtime_url,
            endpoint_url: endpoint_url.clone(),
            local_health_url: local_health_url.clone(),
            started_at: chrono::Utc::now(),
        },
    )?;

    // Wait for local readiness before telling the platform the service is usable.
    if let Err(error) = wait_for_deployment_ready(
        &backend,
        deployment_id,
        &local_health_url,
        config.startup_timeout_secs,
    )
    .await
    {
        stop_deployment_backend(&backend, deployment_id).await.ok();
        state::remove_active_deployment(&paths.state_dir, deployment_id).ok();
        return Err(error);
    }

    send_msg(
        outgoing_tx,
        &NodeMessage::DeploymentReady {
            deployment_id,
            endpoint_url: endpoint_url.clone(),
        },
    )
    .await;

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    running_deployments.lock().await.insert(
        deployment_id,
        RunningDeploymentHandle {
            backend: backend.clone(),
            stop_tx: Some(stop_tx),
        },
    );
    spawn_deployment_monitor(
        paths.state_dir.clone(),
        deployment_id,
        backend,
        local_health_url,
        outgoing_tx.clone(),
        running_deployments.clone(),
        stop_rx,
    );

    Ok(())
}

async fn stop_deployment(
    paths: &PrismPaths,
    deployment_id: Uuid,
    reason: &str,
    outgoing_tx: &mpsc::Sender<String>,
    running_deployments: &RunningDeployments,
) -> Result<()> {
    let running = {
        let mut deployments = running_deployments.lock().await;
        deployments.remove(&deployment_id)
    };

    let record = state::remove_active_deployment(&paths.state_dir, deployment_id)?;
    let backend = match (running, record.as_ref()) {
        (Some(mut handle), _) => {
            if let Some(stop_tx) = handle.stop_tx.take() {
                let _ = stop_tx.send(());
            }
            Some(handle.backend)
        }
        (None, Some(record)) => Some(backend_from_record(record)?),
        (None, None) => None,
    };

    if let Some(backend) = backend {
        stop_deployment_backend(&backend, deployment_id).await?;
    }

    send_msg(
        outgoing_tx,
        &NodeMessage::DeploymentStopped {
            deployment_id,
            reason: reason.to_string(),
        },
    )
    .await;
    Ok(())
}

fn spawn_deployment_monitor(
    state_dir: PathBuf,
    deployment_id: Uuid,
    backend: DeploymentBackend,
    local_health_url: String,
    outgoing_tx: mpsc::Sender<String>,
    running_deployments: RunningDeployments,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
) {
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!(%deployment_id, error = %error, "failed to build deployment monitor client");
                return;
            }
        };
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.tick().await;

        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                _ = interval.tick() => {
                    match deployment_backend_status(&backend, deployment_id).await {
                        Ok((true, _)) => {
                            let healthy = http_health_ok(&client, &local_health_url).await;
                            let message = if healthy {
                                None
                            } else {
                                Some(format!("health check failed for {local_health_url}"))
                            };
                            send_msg(
                                &outgoing_tx,
                                &NodeMessage::DeploymentHealthUpdate {
                                    deployment_id,
                                    healthy,
                                    message,
                                },
                            ).await;
                        }
                        Ok((false, reason)) => {
                            running_deployments.lock().await.remove(&deployment_id);
                            state::remove_active_deployment(&state_dir, deployment_id).ok();
                            send_msg(
                                &outgoing_tx,
                                &NodeMessage::DeploymentStopped {
                                    deployment_id,
                                    reason: reason.unwrap_or_else(|| "stopped".to_string()),
                                },
                            ).await;
                            break;
                        }
                        Err(error) => {
                            running_deployments.lock().await.remove(&deployment_id);
                            state::remove_active_deployment(&state_dir, deployment_id).ok();
                            send_msg(
                                &outgoing_tx,
                                &NodeMessage::DeploymentStopped {
                                    deployment_id,
                                    reason: format!("monitor_failed: {error}"),
                                },
                            ).await;
                            break;
                        }
                    }
                }
            }
        }
    });
}

fn parse_deployment_launch_config(value: &serde_json::Value) -> DeploymentLaunchConfig {
    let port = value
        .get("port")
        .and_then(|item| item.as_u64())
        .and_then(|item| u16::try_from(item).ok())
        .unwrap_or(8080);
    let health_path = value
        .get("health_path")
        .and_then(|item| item.as_str())
        .map(normalize_health_path)
        .unwrap_or_else(|| "/health".to_string());
    let startup_timeout_secs = value
        .get("startup_timeout_secs")
        .and_then(|item| item.as_u64())
        .or_else(|| value.get("startup_timeout").and_then(|item| item.as_u64()))
        .unwrap_or(120);
    let framework = value
        .get("framework")
        .and_then(|item| item.as_str())
        .unwrap_or("auto")
        .to_string();
    let command = value.get("command").and_then(parse_command_override);
    DeploymentLaunchConfig {
        port,
        health_path,
        startup_timeout_secs,
        framework,
        command,
        endpoint_url: value
            .get("endpoint_url")
            .and_then(|item| item.as_str())
            .map(|item| item.to_string()),
        public_base_url: value
            .get("public_base_url")
            .and_then(|item| item.as_str())
            .map(|item| item.to_string()),
        public_host: value
            .get("public_host")
            .and_then(|item| item.as_str())
            .map(|item| item.to_string()),
    }
}

fn parse_command_override(value: &serde_json::Value) -> Option<Vec<String>> {
    if let Some(array) = value.as_array() {
        let command = array
            .iter()
            .filter_map(|item| item.as_str().map(|item| item.to_string()))
            .collect::<Vec<_>>();
        if command.is_empty() {
            None
        } else {
            Some(command)
        }
    } else {
        value.as_str().map(|item| vec![item.to_string()])
    }
}

fn normalize_health_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

fn looks_like_weights_source(image: &str) -> bool {
    image.starts_with("hf://")
        || image.starts_with("r2://")
        || image.starts_with("http://")
        || image.starts_with("https://")
        || image.starts_with('/')
        || Path::new(image).exists()
}

fn deployment_runtime_url() -> String {
    std::env::var("PRISM_RUNTIME_URL").unwrap_or_else(|_| "http://127.0.0.1:8090".to_string())
}

fn resolve_public_endpoint_url(port: u16, config: &DeploymentLaunchConfig) -> String {
    if let Some(endpoint_url) = &config.endpoint_url {
        return endpoint_url.clone();
    }

    if let Some(base) = config
        .public_base_url
        .clone()
        .or_else(|| std::env::var("PRISM_NODE_PUBLIC_BASE_URL").ok())
    {
        let trimmed = base.trim_end_matches('/').to_string();
        if let Ok(mut url) = reqwest::Url::parse(&trimmed) {
            if url.port().is_none() {
                let _ = url.set_port(Some(port));
            }
            return url.to_string().trim_end_matches('/').to_string();
        }
        return format!("{trimmed}:{port}");
    }

    let host = config
        .public_host
        .clone()
        .or_else(|| std::env::var("PRISM_NODE_PUBLIC_HOST").ok())
        .or_else(detect_local_ip)
        .unwrap_or_else(|| "127.0.0.1".to_string());
    format!("http://{host}:{port}")
}

fn detect_local_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

fn describe_backend_for_state(
    backend: &DeploymentBackend,
    deployment_id: Uuid,
) -> (String, String, Option<String>) {
    match backend {
        DeploymentBackend::Runtime { runtime_url } => (
            "runtime".to_string(),
            deployment_id.to_string(),
            Some(runtime_url.clone()),
        ),
        DeploymentBackend::Container { runtime, handle } => {
            (runtime.as_str().to_string(), handle.clone(), None)
        }
    }
}

fn backend_from_record(record: &state::ActiveDeploymentRecord) -> Result<DeploymentBackend> {
    if record.backend == "runtime" {
        return Ok(DeploymentBackend::Runtime {
            runtime_url: record
                .runtime_url
                .clone()
                .unwrap_or_else(deployment_runtime_url),
        });
    }

    let runtime = executor::resolve_container_runtime(Some(&record.backend))
        .with_context(|| format!("unsupported deployment backend {}", record.backend))?;
    Ok(DeploymentBackend::Container {
        runtime,
        handle: record.handle.clone(),
    })
}

async fn start_runtime_deployment(
    runtime_url: &str,
    deployment_id: Uuid,
    image: &str,
    env_vars: &std::collections::BTreeMap<String, String>,
    gpu: bool,
    config: &DeploymentLaunchConfig,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?;
    client
        .post(format!("{}/deploy", runtime_url.trim_end_matches('/')))
        .json(&serde_json::json!({
            "deployment_id": deployment_id.to_string(),
            "weights_source": image,
            "framework": config.framework.clone(),
            "port": config.port,
            "health_path": config.health_path.clone(),
            "gpu": gpu,
            "env": env_vars,
            "command": config.command.clone(),
        }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

async fn stop_deployment_backend(backend: &DeploymentBackend, deployment_id: Uuid) -> Result<()> {
    match backend {
        DeploymentBackend::Runtime { runtime_url } => {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?;
            let response = client
                .delete(format!(
                    "{}/deploy/{}",
                    runtime_url.trim_end_matches('/'),
                    deployment_id
                ))
                .send()
                .await?;
            if !response.status().is_success()
                && response.status() != reqwest::StatusCode::NOT_FOUND
            {
                response.error_for_status()?;
            }
            Ok(())
        }
        DeploymentBackend::Container { runtime, handle } => {
            executor::stop_container_handle(*runtime, handle).await;
            Ok(())
        }
    }
}

async fn deployment_backend_status(
    backend: &DeploymentBackend,
    deployment_id: Uuid,
) -> Result<(bool, Option<String>)> {
    match backend {
        DeploymentBackend::Runtime { runtime_url } => {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()?;
            let response = client
                .get(format!(
                    "{}/deploy/{}",
                    runtime_url.trim_end_matches('/'),
                    deployment_id
                ))
                .send()
                .await?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok((false, Some("deployment_not_found".to_string())));
            }
            let response = response.error_for_status()?;
            let value: serde_json::Value = response.json().await?;
            let status = value
                .get("status")
                .and_then(|item| item.as_str())
                .unwrap_or("unknown");
            let exit_code = value.get("exit_code").and_then(|item| item.as_i64());
            let running = matches!(status, "starting" | "running");
            let reason = if running {
                None
            } else {
                Some(match exit_code {
                    Some(code) => format!("runtime_stopped:{status}:exit_code={code}"),
                    None => format!("runtime_stopped:{status}"),
                })
            };
            Ok((running, reason))
        }
        DeploymentBackend::Container { runtime, handle } => {
            let (status, exit_code) = executor::inspect_container_handle(*runtime, handle).await?;
            let running = matches!(status.as_str(), "running" | "created" | "restarting");
            let reason = if running {
                None
            } else {
                Some(format!("container_stopped:{status}:exit_code={exit_code}"))
            };
            Ok((running, reason))
        }
    }
}

async fn wait_for_deployment_ready(
    backend: &DeploymentBackend,
    deployment_id: Uuid,
    local_health_url: &str,
    timeout_secs: u64,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs.max(5));

    loop {
        if http_health_ok(&client, local_health_url).await {
            return Ok(());
        }

        let (running, reason) = deployment_backend_status(backend, deployment_id).await?;
        if !running {
            bail!(
                "{}",
                reason.unwrap_or_else(|| "deployment stopped before becoming healthy".to_string())
            );
        }
        if std::time::Instant::now() >= deadline {
            bail!("deployment did not become healthy within {timeout_secs}s");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn http_health_ok(client: &reqwest::Client, url: &str) -> bool {
    client
        .get(url)
        .send()
        .await
        .map(|response| response.status().is_success())
        .unwrap_or(false)
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

async fn request_stop_all_deployments(running_deployments: &RunningDeployments) {
    let mut deployments = running_deployments.lock().await;
    for (deployment_id, handle) in deployments.iter_mut() {
        tracing::info!(%deployment_id, "requesting deployment shutdown");
        if let Some(stop_tx) = handle.stop_tx.take() {
            let _ = stop_tx.send(());
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

async fn cleanup_orphaned_deployments(state_dir: &Path) -> Result<()> {
    for record in state::active_deployments(state_dir)? {
        tracing::warn!(
            deployment_id = %record.deployment_id,
            backend = %record.backend,
            handle = %record.handle,
            "cleaning up orphaned deployment from previous node session"
        );
        let backend = backend_from_record(&record)?;
        stop_deployment_backend(&backend, record.deployment_id)
            .await
            .ok();
        state::remove_active_deployment(state_dir, record.deployment_id).ok();
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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
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

    #[test]
    fn deployment_config_parses_optional_fields() {
        let config = parse_deployment_launch_config(&serde_json::json!({
            "port": 9001,
            "health_path": "ready",
            "startup_timeout_secs": 45,
            "framework": "vllm",
            "command": ["python", "serve.py"],
            "public_host": "node.example.com",
        }));

        assert_eq!(config.port, 9001);
        assert_eq!(config.health_path, "/ready");
        assert_eq!(config.startup_timeout_secs, 45);
        assert_eq!(config.framework, "vllm");
        assert_eq!(
            config.command,
            Some(vec!["python".to_string(), "serve.py".to_string()])
        );
        assert_eq!(config.public_host.as_deref(), Some("node.example.com"));
    }

    #[test]
    fn public_endpoint_url_uses_explicit_base_url_port() {
        let config = DeploymentLaunchConfig {
            port: 9001,
            health_path: "/health".to_string(),
            startup_timeout_secs: 120,
            framework: "auto".to_string(),
            command: None,
            endpoint_url: None,
            public_base_url: Some("https://node.example.com".to_string()),
            public_host: None,
        };

        let endpoint = resolve_public_endpoint_url(9001, &config);
        assert_eq!(endpoint, "https://node.example.com:9001");
    }

    #[allow(clippy::type_complexity)]
    fn spawn_stub_http_server(
        max_requests: usize,
        responder: Arc<dyn Fn(&str) -> (u16, String, &'static str) + Send + Sync>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for _ in 0..max_requests {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0u8; 8192];
                let read = stream.read(&mut buffer).unwrap();
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let (status, body, content_type) = responder(&request);
                let response = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (format!("http://{}", addr), handle)
    }

    #[tokio::test]
    async fn runtime_backed_deployment_emits_ready_and_stopped() {
        let deployment_id = Uuid::parse_str("00000000-0000-4000-8000-000000000321").unwrap();
        let (health_base, health_server) = spawn_stub_http_server(
            1,
            Arc::new(|request| {
                assert!(request.starts_with("GET /health "));
                (200, "ok".to_string(), "text/plain")
            }),
        );
        let health_port = reqwest::Url::parse(&health_base).unwrap().port().unwrap();
        let runtime_requests = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let runtime_requests_clone = runtime_requests.clone();
        let (runtime_url, runtime_server) = spawn_stub_http_server(
            2,
            Arc::new(move |request| {
                runtime_requests_clone
                    .lock()
                    .unwrap()
                    .push(request.lines().next().unwrap_or("").to_string());
                if request.starts_with("POST /deploy ") {
                    (
                        200,
                        serde_json::json!({
                            "deployment_id": deployment_id.to_string(),
                            "status": "starting",
                            "port": health_port,
                            "pid": 20,
                        })
                        .to_string(),
                        "application/json",
                    )
                } else {
                    assert!(request.starts_with(&format!("DELETE /deploy/{} ", deployment_id)));
                    (
                        200,
                        serde_json::json!({
                            "deployment_id": deployment_id.to_string(),
                            "status": "stopped",
                        })
                        .to_string(),
                        "application/json",
                    )
                }
            }),
        );

        std::env::set_var("PRISM_RUNTIME_URL", runtime_url);
        let tmp = TempDir::new().unwrap();
        let paths = PrismPaths {
            config_dir: tmp.path().join("config"),
            cache_dir: tmp.path().join("cache"),
            data_dir: tmp.path().join("data"),
            state_dir: tmp.path().join("state"),
        };
        let (tx, mut rx) = mpsc::channel::<String>(8);
        let running_deployments: RunningDeployments = Arc::new(Mutex::new(HashMap::new()));

        start_deployment(
            &paths,
            deployment_id,
            "hf://sentence-transformers/paraphrase-MiniLM-L3-v2".to_string(),
            std::collections::BTreeMap::new(),
            Some("A100-80GB".to_string()),
            serde_json::json!({
                "port": health_port,
                "health_path": "/health",
                "public_host": "node.example.com",
            }),
            &tx,
            &running_deployments,
        )
        .await
        .unwrap();

        let ready: NodeMessage = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        assert!(matches!(
            ready,
            NodeMessage::DeploymentReady {
                deployment_id: id,
                endpoint_url,
            } if id == deployment_id && endpoint_url == format!("http://node.example.com:{health_port}")
        ));

        stop_deployment(
            &paths,
            deployment_id,
            "user_request",
            &tx,
            &running_deployments,
        )
        .await
        .unwrap();

        let stopped: NodeMessage = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
        assert!(matches!(
            stopped,
            NodeMessage::DeploymentStopped {
                deployment_id: id,
                reason,
            } if id == deployment_id && reason == "user_request"
        ));
        assert!(state::active_deployments(&paths.state_dir)
            .unwrap()
            .is_empty());

        health_server.join().unwrap();
        runtime_server.join().unwrap();
        std::env::remove_var("PRISM_RUNTIME_URL");

        let requests = runtime_requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("POST /deploy "));
        assert!(requests[1].starts_with("DELETE /deploy/"));
    }
}
