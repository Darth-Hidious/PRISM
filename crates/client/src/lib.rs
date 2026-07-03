//! Typed HTTP client for the MARC27 platform API.
//!
//! Provides [`PlatformClient`] for authenticated REST calls and [`DeviceFlowAuth`]
//! for GitHub CLI-style device-code OAuth. Also covers marketplace browsing
//! and node registration/discovery via the platform.

pub mod api;
pub mod auth;
pub mod knowledge;
pub mod marketplace;
pub mod node_registry;

pub use api::PlatformClient;
pub use auth::DeviceFlowAuth;
pub use knowledge::KnowledgeExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_constructs() {
        let client = PlatformClient::new("https://api.marc27.com/api/v1");
        assert_eq!(client.base_url(), "https://api.marc27.com/api/v1");
    }

    #[test]
    fn client_with_token() {
        let client = PlatformClient::new("https://api.marc27.com/api/v1").with_token("test-token");
        // Just verifying it compiles and doesn't panic.
        assert_eq!(client.base_url(), "https://api.marc27.com/api/v1");
    }
}
