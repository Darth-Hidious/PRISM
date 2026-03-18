//! WebSocket daemon: register, heartbeat, job dispatch, reconnection.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use prism_proto::{NodeCapabilities, NodeMessage, PlatformMessage};
use prism_runtime::{PlatformEndpoints, PrismPaths, StoredCredentials};
use sysinfo::System;
use tokio::signal;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

/// Options for the node daemon.
#[derive(Debug, Clone)]
pub struct DaemonOptions {
    pub name: String,
    pub visibility: String,
    pub price_per_hour_usd: Option<f64>,
    pub no_compute: bool,
    pub no_storage: bool,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            name: hostname(),
            visibility: "private".to_string(),
            price_per_hour_usd: None,
            no_compute: false,
            no_storage: false,
        }
    }
}

/// Run the node daemon with reconnection logic.
pub async fn run_daemon(
    endpoints: &PlatformEndpoints,
    paths: &PrismPaths,
    options: DaemonOptions,
) -> Result<()> {
    let mut capabilities = crate::detect::probe_local_capabilities_async().await;
    capabilities.visibility = options.visibility.clone();
    capabilities.price_per_hour_usd = options.price_per_hour_usd;

    if options.no_compute {
        capabilities.services.retain(|s| s.kind != "compute");
    }
    if options.no_storage {
        capabilities.services.retain(|s| s.kind != "storage");
    }

    // Strip absolute paths before sending over the wire — security requirement.
    let wire_capabilities = strip_paths_for_wire(&capabilities);

    // Load org_id from stored credentials
    let org_id = paths
        .load_cli_state()
        .ok()
        .and_then(|s| s.credentials)
        .and_then(|c| c.org_id)
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

    // Write PID file so `node down` can find us
    let pid_path = pid_file_path(paths);
    write_pid_file(&pid_path)?;

    // Ensure PID file is cleaned up on exit
    let _pid_guard = PidFileGuard(pid_path.clone());

    let mut delay_secs: u64 = 1;

    loop {
        let token = load_access_token(paths, endpoints).await?;

        match connect_and_run(endpoints, &token, &options.name, org_id, &wire_capabilities).await {
            Ok(ShutdownReason::Graceful) => {
                tracing::info!("node shut down gracefully");
                return Ok(());
            }
            Ok(ShutdownReason::TokenExpired) => {
                tracing::info!("token expired, refreshing and reconnecting");
                delay_secs = 1;
                continue;
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    delay_secs,
                    "disconnected, reconnecting"
                );
                tokio::time::sleep(Duration::from_secs(delay_secs)).await;
                delay_secs = (delay_secs * 2).min(300);
            }
        }
    }
}

/// Stop a running node daemon by sending SIGTERM to the PID in the PID file.
pub fn stop_daemon(paths: &PrismPaths) -> Result<()> {
    let pid_path = pid_file_path(paths);
    if !pid_path.exists() {
        bail!("no running node found (PID file not present at {})", pid_path.display());
    }

    let pid_str = std::fs::read_to_string(&pid_path)
        .context("failed to read PID file")?;
    let pid: u32 = pid_str
        .trim()
        .parse()
        .context("PID file contains invalid PID")?;

    // Check if process is alive
    let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
    if !alive {
        // Stale PID file — clean up
        std::fs::remove_file(&pid_path).ok();
        bail!("node process (PID {pid}) is not running (stale PID file removed)");
    }

    // Send SIGTERM
    let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if result != 0 {
        bail!("failed to send SIGTERM to PID {pid}: {}", std::io::Error::last_os_error());
    }

    // Wait briefly for process to exit, then clean up PID file
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(250));
        let still_alive = unsafe { libc::kill(pid as i32, 0) } == 0;
        if !still_alive {
            std::fs::remove_file(&pid_path).ok();
            println!("Node (PID {pid}) stopped.");
            return Ok(());
        }
    }

    println!("Sent SIGTERM to PID {pid}. Process may still be shutting down.");
    Ok(())
}

enum ShutdownReason {
    Graceful,
    TokenExpired,
}

async fn connect_and_run(
    endpoints: &PlatformEndpoints,
    token: &str,
    name: &str,
    org_id: Option<Uuid>,
    capabilities: &NodeCapabilities,
) -> Result<ShutdownReason> {
    let url = format!("{}?token={}", endpoints.node_ws, token);

    tracing::info!(url = %endpoints.node_ws, "connecting to platform");

    let (ws, _response) = tokio_tungstenite::connect_async(&url)
        .await
        .context("WebSocket connection failed")?;

    let (mut sink, mut stream) = ws.split();

    // Send Register message
    let register = NodeMessage::Register {
        name: name.to_string(),
        org_id,
        capabilities: Box::new(capabilities.clone()),
    };
    let register_json = serde_json::to_string(&register)?;
    sink.send(Message::Text(register_json)).await?;

    tracing::info!("register message sent, waiting for confirmation");

    // Wait for Registered response
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
                Ok(other) => {
                    tracing::warn!(?other, "unexpected first message from platform");
                }
                Err(e) => {
                    tracing::warn!(error = %e, raw = %text, "failed to parse platform message");
                }
            }
        }
    }

    let _node_id = node_id.context("did not receive Registered message")?;

    // Main loop: heartbeat + message handling + Ctrl-C
    let mut heartbeat_timer = tokio::time::interval(heartbeat_interval);
    heartbeat_timer.tick().await; // consume first immediate tick

    let mut system = System::new_all();

    loop {
        tokio::select! {
            _ = heartbeat_timer.tick() => {
                system.refresh_all();
                let cpu_load = system.global_cpu_usage() as f64;
                let used_mem = system.used_memory() as f64;
                let total_mem = system.total_memory() as f64;
                let memory_usage = if total_mem > 0.0 { used_mem / total_mem } else { 0.0 };

                let hb = NodeMessage::Heartbeat {
                    cpu_load,
                    memory_usage,
                    gpus_free: 0, // TODO: real GPU status
                    active_jobs: 0,
                };
                let hb_json = serde_json::to_string(&hb)?;
                sink.send(Message::Text(hb_json)).await?;
                tracing::debug!(cpu = cpu_load, mem = memory_usage, "heartbeat sent");
            }
            msg = stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        handle_platform_message(&text, &mut sink).await?;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        sink.send(Message::Pong(data)).await?;
                    }
                    Some(Ok(Message::Close(_))) => {
                        tracing::info!("server closed connection");
                        return Err(anyhow::anyhow!("server closed connection"));
                    }
                    Some(Err(e)) => {
                        return Err(e.into());
                    }
                    None => {
                        return Err(anyhow::anyhow!("WebSocket stream ended"));
                    }
                    _ => {}
                }
            }
            _ = signal::ctrl_c() => {
                tracing::info!("Ctrl-C received, shutting down");
                sink.send(Message::Close(None)).await.ok();
                return Ok(ShutdownReason::Graceful);
            }
        }
    }
}

async fn handle_platform_message(
    text: &str,
    sink: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
) -> Result<()> {
    let msg: PlatformMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, raw = %text, "failed to parse platform message");
            return Ok(());
        }
    };

    match msg {
        PlatformMessage::Ping => {
            // Platform-level ping (not WS ping)
            tracing::debug!("received platform ping");
        }
        PlatformMessage::SubmitJob {
            job_id,
            image,
            inputs: _,
            env_vars: _,
            gpu_type: _,
            timeout_secs,
        } => {
            tracing::info!(
                %job_id,
                %image,
                timeout = timeout_secs,
                "received job"
            );

            // Send initial update
            let update = NodeMessage::JobUpdate {
                job_id,
                progress: 0.0,
                message: Some("Received, queuing...".to_string()),
            };
            sink.send(Message::Text(serde_json::to_string(&update)?))
                .await?;

            // TODO: actual container execution
            // For now, report as failed (not implemented)
            let failed = NodeMessage::JobFailed {
                job_id,
                error: "job execution not yet implemented".to_string(),
                duration_secs: 0,
            };
            sink.send(Message::Text(serde_json::to_string(&failed)?))
                .await?;
        }
        PlatformMessage::CancelJob { job_id } => {
            tracing::info!(%job_id, "cancel requested");
            // TODO: kill running container
        }
        PlatformMessage::Error { code, message } => {
            tracing::error!(code = %code, message = %message, "platform error");
        }
        PlatformMessage::Registered { .. } => {
            // Duplicate, ignore
        }
    }

    Ok(())
}

/// Load access token, refreshing if expired.
async fn load_access_token(paths: &PrismPaths, endpoints: &PlatformEndpoints) -> Result<String> {
    let state = paths.load_cli_state()?;
    let creds = state
        .credentials
        .as_ref()
        .context("not logged in — run `prism login` first")?;

    // Check if token is expired
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
        .json(&serde_json::json!({
            "refresh_token": creds.refresh_token,
        }))
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

    // Update stored credentials
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

// --- PID file management ---

fn pid_file_path(paths: &PrismPaths) -> PathBuf {
    paths.state_dir.join("node.pid")
}

fn write_pid_file(path: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(path.parent().unwrap_or(path))?;
    let pid = std::process::id();
    std::fs::write(path, pid.to_string())?;

    // Set file permissions to 0600 (owner read/write only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

/// RAII guard that removes the PID file on drop.
struct PidFileGuard(PathBuf);

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

// --- Wire safety: strip absolute paths ---

/// Create a copy of capabilities with absolute paths stripped.
/// Only the sanitized name and metadata are sent — never local filesystem paths.
fn strip_paths_for_wire(caps: &NodeCapabilities) -> NodeCapabilities {
    let mut wire = caps.clone();

    for ds in &mut wire.datasets {
        // Replace absolute path with just the sanitized name
        ds.path = ds.name.clone();
    }

    for m in &mut wire.models {
        m.path = m.name.clone();
    }

    // Strip Ollama localhost endpoints — keep those since they're
    // needed for the platform to route inference requests.
    // But strip any filesystem-path endpoints that might leak.
    for svc in &mut wire.services {
        if let Some(ep) = &svc.endpoint {
            if !ep.starts_with("http://") && !ep.starts_with("https://") {
                svc.endpoint = None;
            }
        }
    }

    wire
}
