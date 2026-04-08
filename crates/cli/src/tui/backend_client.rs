#![allow(dead_code)]

use crate::tui::protocol::RpcNotification;
use anyhow::{Context, Result};
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

pub struct BackendClient {
    pub process: Child,
    pub tx_requests: mpsc::Sender<String>,
    pub rx_notifications: mpsc::Receiver<RpcNotification>,
}

impl BackendClient {
    pub async fn spawn(prism_exe: &Path, project_root: &Path, python_bin: &Path) -> Result<Self> {
        let mut child = Command::new(prism_exe)
            .arg("backend")
            .arg("--project-root")
            .arg(project_root)
            .arg("--python")
            .arg(python_bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn prism backend")?;

        let stdout = child.stdout.take().expect("Failed to grab stdout");
        let mut stdin = child.stdin.take().expect("Failed to grab stdin");
        let stderr = child.stderr.take().expect("Failed to grab stderr");

        // We can optionally log stderr or ignore it (TUI will claim the terminal anyway)
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(_line)) = reader.next_line().await {
                // Background logging could be stored to a file
            }
        });

        let (tx_notif, rx_notifications) = mpsc::channel(100);
        let (tx_requests, mut rx_requests) = mpsc::channel::<String>(100);

        // Read from backend stdout
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if let Ok(notif) = serde_json::from_str::<RpcNotification>(&line) {
                    if tx_notif.send(notif).await.is_err() {
                        break;
                    }
                } else if line.contains(r#""result":"#) {
                    // Ignore generic RPC success responses for now
                }
            }
        });

        // Write to backend stdin
        tokio::spawn(async move {
            while let Some(mut req) = rx_requests.recv().await {
                req.push('\n');
                if stdin.write_all(req.as_bytes()).await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        Ok(Self {
            process: child,
            tx_requests,
            rx_notifications,
        })
    }
}
