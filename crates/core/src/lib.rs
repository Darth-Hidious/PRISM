//! Core domain logic for a PRISM node.
//!
//! This crate is the shared foundation that every other crate depends on:
//!
//! - [`config`]: Node configuration (`prism.toml` schema).
//! - [`session`]: Multi-user session management (SQLite-backed).
//! - [`rbac`]: Role-based access control (platform + local roles, permission checks).
//! - [`audit`]: Append-only audit log (SQLite-backed, required for ESA/defense compliance).
//! - [`registry`]: Tool manifest discovery and in-memory tool registry.

pub mod config;
pub mod session;
pub mod rbac;
pub mod audit;
pub mod registry;

#[cfg(test)]
mod tests {
    #[test]
    fn core_compiles() {
        assert!(true);
    }
}
