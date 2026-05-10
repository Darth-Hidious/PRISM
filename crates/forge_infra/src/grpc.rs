use std::sync::{Arc, Mutex};

use tonic::transport::Channel;
use url::Url;

/// Wrapper for a shared gRPC channel to the workspace server
///
/// This struct manages a lazily-connected gRPC channel that can be cheaply
/// cloned and shared across multiple gRPC clients. The channel is only created
/// on first access.
#[derive(Clone)]
pub struct ForgeGrpcClient {
    server_url: String,
    channel: Arc<Mutex<Option<Channel>>>,
}

impl ForgeGrpcClient {
    /// Creates a new gRPC client that will lazily connect on first use
    ///
    /// # Arguments
    /// * `server_url` - The URL of the gRPC server
    pub fn new(server_url: String) -> Self {
        Self {
            server_url,
            channel: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns a clone of the underlying gRPC channel
    ///
    /// Channels are cheap to clone and can be shared across multiple clients.
    /// The channel is created on first call and cached for subsequent calls.
    ///
    /// Returns `Err` (instead of panicking) when the configured `server_url`
    /// is empty or malformed, so the caller can surface the failure into the
    /// chat loop's normal tool-error recovery path. Previously this used
    /// `.expect("Invalid server URL")` which would panic the entire chat UI
    /// if any code path constructed a `ForgeGrpcClient` with an empty URL
    /// (default `services_url` is `""` per `forge_config::config`). PRISM's
    /// long-horizon agent runs touched this path during semantic-search /
    /// fuzzy-search calls and crashed mid-conversation; see the BimoTech /
    /// Fraunhofer end-to-end trace for the failing case.
    pub fn channel(&self) -> anyhow::Result<Channel> {
        let mut guard = self.channel.lock().unwrap();

        if let Some(channel) = guard.as_ref() {
            return Ok(channel.clone());
        }

        if self.server_url.is_empty() {
            anyhow::bail!(
                "Forge services URL is not configured \
                 (`services_url` is empty in config) — \
                 the gRPC channel cannot be opened. \
                 Set `services_url` in .forge.toml or skip the gRPC-backed \
                 indexing path"
            );
        }

        let mut channel = Channel::from_shared(self.server_url.to_string())
            .map_err(|e| {
                anyhow::anyhow!("invalid services URL `{}`: {e}", self.server_url)
            })?
            .concurrency_limit(256);

        // Enable TLS for https URLs (webpki-roots is faster than native-roots)
        if Url::parse(&self.server_url)?.scheme().contains("https") {
            let tls_config = tonic::transport::ClientTlsConfig::new().with_webpki_roots();
            channel = channel
                .tls_config(tls_config)
                .expect("Failed to configure TLS");
        }

        let new_channel = channel.connect_lazy();
        *guard = Some(new_channel.clone());
        Ok(new_channel)
    }

    /// Hydrates the gRPC channel by forcing its initialization
    ///
    /// This clears any existing cached channel and forces a fresh connection
    /// on the next call to `channel()`.
    /// Used to warm up or reset the connection.
    pub fn hydrate(&self) {
        let mut guard = self.channel.lock().unwrap();
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: empty `server_url` must not panic the chat UI.
    ///
    /// Default `forge_config::ForgeConfig::services_url` is `""` (serde
    /// `#[serde(default)]` on a `String`). When PRISM's long-horizon agent
    /// loop reached a `context_engine` / `fuzzy_search` step, the resulting
    /// `ForgeGrpcClient::channel()` panicked at `.expect("Invalid server URL")`,
    /// taking down the whole chat UI mid-conversation. The fix is to return
    /// `Err` so the chat loop's recovery rules can do their job.
    #[test]
    fn channel_returns_err_on_empty_url_instead_of_panicking() {
        let client = ForgeGrpcClient::new(String::new());
        let result = client.channel();
        assert!(
            result.is_err(),
            "empty server_url must produce an Err, not a Channel"
        );
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("services_url") || msg.contains("not configured"),
            "error must explain the misconfiguration, got: {msg}"
        );
    }

    /// Belt-and-braces: a syntactically invalid URL must also propagate as
    /// `Err`, not panic. (`Channel::from_shared` returns the error path.)
    #[test]
    fn channel_returns_err_on_malformed_url_instead_of_panicking() {
        let client = ForgeGrpcClient::new("not a url".to_string());
        let result = client.channel();
        assert!(
            result.is_err(),
            "malformed server_url must produce an Err, not a Channel"
        );
    }
}
