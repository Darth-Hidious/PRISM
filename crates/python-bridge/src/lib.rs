// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Python worker launch and IPC primitives.
//!
//! Spawns the Python TAOR runtime as a child process (rather than as the
//! top-level CLI shell), discovers its venv, injects environment variables,
//! and pipes stdio for JSON-RPC communication.
//!
//! # Not a supervisor
//!
//! Despite the historical "supervision" wording, this crate does **not**
//! supervise the worker: there is no automatic restart on exit and no health
//! check. A dead worker stays dead until a [`ToolServerHandle`] call surfaces
//! a [`PythonBridgeError::WorkerExited`] or I/O error and the caller spawns a
//! replacement. Callers that need liveness/restart must implement it
//! themselves.

pub mod tool_server;
pub mod venv;
pub use tool_server::{TOOL_SERVER_MODULE, ToolServer, ToolServerHandle};
pub use venv::ensure_venv;

/// Failures from launching or talking to the Python worker.
///
/// Each variant names a distinct failure mode so logs distinguish a process
/// that would not start ([`Spawn`](Self::Spawn)) from one that started but
/// timed out ([`Timeout`](Self::Timeout)), died mid-call
/// ([`WorkerExited`](Self::WorkerExited)), or replied with malformed JSON
/// ([`Parse`](Self::Parse)). Previously all of these collapsed into `Spawn`,
/// so every fault was logged as "failed to spawn python worker".
#[derive(Debug, thiserror::Error)]
pub enum PythonBridgeError {
    /// The worker process could not be started, or a stdio pipe to it failed
    /// (spawn, write, flush, read, kill).
    #[error("failed to spawn python worker: {0}")]
    Spawn(#[from] std::io::Error),

    /// The worker was started but produced no response line within the
    /// deadline (it is slow or wedged).
    #[error("python worker timed out (no response within {0:?})")]
    Timeout(std::time::Duration),

    /// The worker process closed its stdout before replying — it exited or
    /// crashed mid-call.
    #[error("python worker exited before responding (stdout closed)")]
    WorkerExited,

    /// The worker replied, but its response line could not be parsed as the
    /// expected JSON value.
    #[error("failed to parse python worker response: {0}")]
    Parse(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_spawn_faults_are_not_mislabeled_as_spawn() {
        // Regression: previously every fault (timeout, worker-exit, parse)
        // collapsed into `Spawn` and was logged as "failed to spawn python
        // worker". Each must now carry its own honest label so logs tell the
        // three failure modes apart.
        let timeout = PythonBridgeError::Timeout(std::time::Duration::from_secs(60));
        assert!(
            timeout.to_string().contains("timed out"),
            "got: {}",
            timeout
        );

        let exited = PythonBridgeError::WorkerExited;
        assert_eq!(
            exited.to_string(),
            "python worker exited before responding (stdout closed)"
        );

        let bad_json = serde_json::from_str::<serde_json::Value>("{ not valid }").unwrap_err();
        let parse = PythonBridgeError::Parse(bad_json);
        assert!(
            parse
                .to_string()
                .contains("failed to parse python worker response"),
            "got: {}",
            parse
        );

        // None of the non-spawn faults may read as a spawn failure.
        for err in [timeout, exited, parse] {
            assert!(
                !err.to_string().contains("failed to spawn"),
                "non-spawn fault mislabeled as spawn: {err}"
            );
        }
    }
}
