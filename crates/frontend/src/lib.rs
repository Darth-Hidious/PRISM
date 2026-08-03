// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Shared NATIVE driver for PRISM frontends (the Ratatui TUI and the GPUI
//! desktop app). One in-process path onto [`prism_agent`]: no subprocess,
//! no stdio pipes, no JSON-RPC transport — request lines go in on a
//! channel, protocol values come out on a channel, using the exact same
//! message contract the stdio backend emits so frontends stay
//! transport-agnostic.

use std::path::Path;

use anyhow::Result;

/// A live in-process agent session.
pub struct NativeSession {
    tx: Option<std::sync::mpsc::Sender<String>>,
    rx: Option<std::sync::mpsc::Receiver<serde_json::Value>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NativeSession {
    /// Decompose into the raw channel ends. The session thread keeps
    /// running until the returned sender is dropped.
    pub fn into_parts(mut self) -> (std::sync::mpsc::Sender<String>, std::sync::mpsc::Receiver<serde_json::Value>) {
        self.thread = None;
        (self.tx.take().expect("session already decomposed"), self.rx.take().expect("session already decomposed"))
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
        self.request("init", serde_json::json!({"auto_approve": false, "resume": ""}))
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
/// already expired) → POST /auth/refresh and persist the rotated pair to
/// BOTH stores (cli-state.json + ~/.prism/credentials.json). Fail-open:
/// a refresh failure leaves the old credential in place and the session
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
    let endpoints = prism_runtime::PlatformEndpoints::from_env();
    let creds_clone = creds.clone();
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(_) => return,
    };
    let result = rt.block_on(async {
        let client = prism_client::PlatformClient::new(&endpoints.api_base);
        let refreshed = prism_client::DeviceFlowAuth::refresh_token(
            client.inner(),
            &endpoints.api_base,
            &creds_clone.refresh_token,
        )
        .await?;
        let mut new_creds = creds_clone.clone();
        new_creds.access_token = refreshed.access_token;
        new_creds.refresh_token = refreshed.refresh_token;
        new_creds.expires_at = refreshed.expires_in.and_then(|secs| {
            chrono::Utc::now().checked_add_signed(chrono::Duration::seconds(secs as i64))
        });
        paths.persist_credentials(&new_creds)?;
        anyhow::Ok(())
    });
    match result {
        Ok(()) => tracing::info!("platform credential refreshed"),
        Err(e) => tracing::warn!(error = %e, "proactive credential refresh failed"),
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
