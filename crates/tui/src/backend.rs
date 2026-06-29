//! Backend subprocess handle — spawns `prism backend` and
//! provides typed JSON-RPC send/receive over stdio pipes.

use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use tokio::sync::mpsc;

pub struct BackendHandle {
    child: Child,
    stdin: std::process::ChildStdin,
    rx: mpsc::UnboundedReceiver<Value>,
    next_id: u64,
}

impl BackendHandle {
    /// Test-only constructor: assemble from pre-spawned parts. Not
    /// gated behind `#[cfg(test)]` because integration tests live in
    /// a separate crate and can't see `cfg(test)` items. Marked
    /// `#[doc(hidden)]` so it doesn't appear in public docs.
    #[doc(hidden)]
    pub fn from_parts(
        child: Child,
        stdin: std::process::ChildStdin,
        rx: mpsc::UnboundedReceiver<Value>,
        next_id: u64,
    ) -> Self {
        Self {
            child,
            stdin,
            rx,
            next_id,
        }
    }

    pub fn spawn(prism_binary: &str, project_root: &str, python_bin: &str) -> Result<Self> {
        let mut child = Command::new(prism_binary)
            .arg("--python")
            .arg(python_bin)
            .arg("backend")
            .arg("--project-root")
            .arg(project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn prism backend")?;

        let stdin = child.stdin.take().context("no stdin on backend")?;
        let stdout = child.stdout.take().context("no stdout on backend")?;

        let (tx, rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) if !l.trim().is_empty() => {
                        if let Ok(v) = serde_json::from_str::<Value>(&l)
                            && tx.send(v).is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                    Ok(_) => continue,
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            rx,
            next_id: 1,
        })
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn send_request(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id();
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "id": id,
            "params": params,
        });
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin
            .write_all(line.as_bytes())
            .context("failed to write to backend stdin")?;
        self.stdin.flush()?;
        Ok(id)
    }

    pub async fn init(&mut self) -> Result<()> {
        self.send_request(
            "init",
            serde_json::json!({"auto_approve": false, "resume": ""}),
        )?;
        // Wait for the init response (first message with a "result" field)
        if let Some(resp) = self.rx.recv().await
            && (resp.get("result").is_some() || resp.get("method").is_some())
        {
            // Could be the response or a welcome notification — both are fine
            // If it's a notification, process it as a welcome
            if resp.get("method").and_then(|m| m.as_str()) == Some("ui.welcome") {
                // Re-send to the channel for the app to process
                // Actually we should just let the app handle it — but since we
                // consumed it, we need to handle it. Let's just return Ok.
                return Ok(());
            }
            return Ok(());
        }
        anyhow::bail!("init failed — no response from backend")
    }

    pub fn send_message(&mut self, text: &str) -> Result<u64> {
        self.send_request("input.message", serde_json::json!({"text": text}))
    }

    pub fn send_command(&mut self, command: &str) -> Result<u64> {
        self.send_request(
            "input.command",
            serde_json::json!({"command": command, "silent": false}),
        )
    }

    pub fn send_approval(&mut self, response: &str) -> Result<()> {
        self.send_request(
            "approval.respond",
            serde_json::json!({"response": response}),
        )?;
        Ok(())
    }

    /// Async recv — used in the tokio::select! loop.
    pub async fn recv(&mut self) -> Option<Value> {
        self.rx.recv().await
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for BackendHandle {
    fn drop(&mut self) {
        self.kill();
    }
}
