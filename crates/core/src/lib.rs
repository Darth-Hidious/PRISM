//! Core domain logic for a PRISM node.
//!
//! This crate is the shared foundation that every other crate depends on:
//!
//! - [`config`]: Node configuration (`prism.toml` schema).
//! - [`session`]: Multi-user session management (SQLite-backed).
//! - [`rbac`]: Role-based access control (platform + local roles, permission checks).
//! - [`audit`]: Append-only audit log (SQLite-backed, required for ESA/defense compliance).
//! - [`registry`]: Tool manifest discovery and in-memory tool registry.

pub mod audit;
pub mod config;
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
