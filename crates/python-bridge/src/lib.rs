// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Python worker launch and supervision primitives.
//!
//! Makes the Python TAOR runtime an explicitly supervised child process rather
//! than the top-level CLI shell. Handles subprocess spawning, venv discovery,
//! environment variable injection, and stdio piping for JSON-RPC communication.

pub mod tool_server;
pub mod venv;
pub use tool_server::{TOOL_SERVER_MODULE, ToolServer, ToolServerHandle};
pub use venv::ensure_venv;

#[derive(Debug, thiserror::Error)]
pub enum PythonBridgeError {
    #[error("failed to spawn python worker: {0}")]
    Spawn(#[from] std::io::Error),
}
