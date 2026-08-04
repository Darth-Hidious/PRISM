// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Core domain logic for a PRISM node.
//!
//! This crate holds node-level domain types shared across the binaries that
//! need them. It is **not** a universal base: only a minority of workspace
//! crates depend on it (the CLI, server, node daemon, frontend, and runtime);
//! leaf crates (`llm`, `mesh`, `policy`, `compute`, `audit`, `agent`, …)
//! deliberately do not, to keep their dependency graphs small. It exposes:
//!
//! - [`config`]: Node configuration (`prism.toml` schema).
//! - [`session`]: Multi-user session management (SQLite-backed).
//! - [`rbac`]: Role-based access control (platform + local roles, permission checks).
//! - [`audit`]: Append-only audit log (SQLite-backed, required for ESA/defense compliance).
//! - [`registry`]: Tool manifest discovery and in-memory tool registry.
//! - [`execution`]: Harness execution envelope + trace events (who is
//!   executing, under which policy mode, reconstructable from trace events).

pub mod audit;
pub mod brand;
pub mod chat_config;
pub mod config;
pub mod execution;
pub mod providers;
pub mod rbac;
pub mod registry;
pub mod session;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_compiles() {
        let _ = config::NodeConfig::default();
    }
}
