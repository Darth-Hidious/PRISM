use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::api::PlatformClient;

/// A tool listing from the MARC27 marketplace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceTool {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub install_count: u64,
}

/// Client for the MARC27 marketplace endpoints.
#[derive(Debug)]
pub struct MarketplaceClient<'a> {
    platform: &'a PlatformClient,
}

impl<'a> MarketplaceClient<'a> {
    pub fn new(platform: &'a PlatformClient) -> Self {
        Self { platform }
    }

    /// List marketplace tools, optionally filtered by a search query.
    pub async fn list_tools(&self, query: Option<&str>) -> Result<Vec<MarketplaceTool>> {
        let path = match query {
            Some(q) => {
                let encoded = urlencoding(q);
                format!("/marketplace/tools?q={encoded}")
            }
            None => "/marketplace/tools".to_string(),
        };
        debug!(%path, "listing marketplace tools");
        self.platform.get(&path).await
    }

    /// Get a single tool by name.
    pub async fn get_tool(&self, name: &str) -> Result<MarketplaceTool> {
        let path = format!("/marketplace/tools/{name}");
        debug!(%path, "fetching marketplace tool");
        self.platform.get(&path).await
    }

    /// Get the install URL for a tool (used by `prism plugin install`).
    pub async fn install_url(&self, name: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct InstallInfo {
            url: String,
        }

        let path = format!("/marketplace/tools/{name}/install");
        debug!(%path, "fetching install URL");
        let info: InstallInfo = self
            .platform
            .get(&path)
            .await
            .context("failed to fetch install URL")?;
        Ok(info.url)
    }
}

/// Minimal percent-encoding for query parameter values.
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(char::from(HEX[(b >> 4) as usize]));
                out.push(char::from(HEX[(b & 0x0f) as usize]));
            }
        }
    }
    out
}

const HEX: [u8; 16] = *b"0123456789ABCDEF";
