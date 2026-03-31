//! JSON-RPC stdio transport for TUI communication.
//!
//! The Rust core binary spawns the Ink TUI as a child process. They communicate
//! via JSON-RPC 2.0 over stdin/stdout (one JSON object per line).

use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::types::{RpcNotification, RpcRequest, RpcResponse};

/// A running IPC connection to the Ink TUI child process.
pub struct IpcServer {
    child: Child,
    /// Send JSON-RPC messages to the TUI.
    tx: mpsc::Sender<String>,
    /// Receive JSON-RPC messages from the TUI.
    rx: mpsc::Receiver<String>,
}

impl IpcServer {
    /// Spawn the TUI binary and set up stdio IPC channels.
    pub async fn spawn(tui_binary: &str) -> Result<Self> {
        let mut child = Command::new(tui_binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // TUI debug logs go to terminal
            .spawn()
            .with_context(|| format!("Failed to spawn TUI binary: {tui_binary}"))?;

        let child_stdin = child.stdin.take().context("No stdin on child")?;
        let child_stdout = child.stdout.take().context("No stdout on child")?;

        // Channel: core → TUI (write to child stdin)
        let (write_tx, mut write_rx) = mpsc::channel::<String>(64);
        tokio::spawn(async move {
            let mut writer = child_stdin;
            while let Some(line) = write_rx.recv().await {
                if let Err(e) = writer.write_all(line.as_bytes()).await {
                    error!("Failed to write to TUI stdin: {e}");
                    break;
                }
                if let Err(e) = writer.write_all(b"\n").await {
                    error!("Failed to write newline to TUI stdin: {e}");
                    break;
                }
                if let Err(e) = writer.flush().await {
                    error!("Failed to flush TUI stdin: {e}");
                    break;
                }
            }
        });

        // Channel: TUI → core (read from child stdout)
        let (read_tx, read_rx) = mpsc::channel::<String>(64);
        tokio::spawn(async move {
            let reader = BufReader::new(child_stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim().to_string();
                if trimmed.is_empty() {
                    continue;
                }
                if read_tx.send(trimmed).await.is_err() {
                    break;
                }
            }
            debug!("TUI stdout reader ended");
        });

        Ok(Self {
            child,
            tx: write_tx,
            rx: read_rx,
        })
    }

    /// Send a JSON-RPC request to the TUI.
    pub async fn send_request(&self, request: &RpcRequest) -> Result<()> {
        let json = serde_json::to_string(request)?;
        debug!(method = %request.method, "→ TUI");
        self.tx
            .send(json)
            .await
            .map_err(|e| anyhow::anyhow!("IPC send failed: {e}"))
    }

    /// Send a JSON-RPC notification (no response expected) to the TUI.
    pub async fn send_notification(&self, notification: &RpcNotification) -> Result<()> {
        let json = serde_json::to_string(notification)?;
        debug!(method = %notification.method, "→ TUI (notification)");
        self.tx
            .send(json)
            .await
            .map_err(|e| anyhow::anyhow!("IPC send failed: {e}"))
    }

    /// Send a JSON-RPC response to the TUI.
    pub async fn send_response(&self, response: &RpcResponse) -> Result<()> {
        let json = serde_json::to_string(response)?;
        self.tx
            .send(json)
            .await
            .map_err(|e| anyhow::anyhow!("IPC send failed: {e}"))
    }

    /// Receive the next JSON-RPC message from the TUI.
    pub async fn recv(&mut self) -> Option<String> {
        self.rx.recv().await
    }

    /// Perform the initial handshake: send `ipc.hello`, expect `ipc.hello` back.
    pub async fn handshake(&mut self, node_name: &str, version: &str) -> Result<()> {
        let hello = RpcRequest::new(
            "ipc.hello",
            serde_json::json!({
                "node_name": node_name,
                "version": version,
                "protocol_version": 1,
            }),
            1,
        );
        self.send_request(&hello).await?;
        info!("Sent ipc.hello to TUI, waiting for response...");

        // Wait for response (with timeout)
        let response = tokio::time::timeout(std::time::Duration::from_secs(10), self.recv())
            .await
            .context("TUI did not respond to ipc.hello within 10s")?
            .context("TUI process exited before responding")?;

        // Parse — could be a response or a request
        if let Ok(resp) = serde_json::from_str::<RpcResponse>(&response) {
            if resp.error.is_some() {
                anyhow::bail!("TUI rejected handshake: {:?}", resp.error);
            }
            info!("IPC handshake complete");
            return Ok(());
        }

        if let Ok(req) = serde_json::from_str::<RpcRequest>(&response) {
            if req.method == "ipc.hello" {
                info!("IPC handshake complete (TUI sent hello)");
                return Ok(());
            }
            warn!(method = %req.method, "Unexpected method during handshake, continuing anyway");
            return Ok(());
        }

        anyhow::bail!("Invalid handshake response from TUI: {response}");
    }

    /// Wait for the child process to exit.
    pub async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        let status = self.child.wait().await?;
        Ok(status)
    }

    /// Kill the child process.
    pub fn kill(&mut self) -> Result<()> {
        self.child.start_kill()?;
        Ok(())
    }
}
