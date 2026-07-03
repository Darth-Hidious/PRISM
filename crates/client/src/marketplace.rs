use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::api::PlatformClient;

/// A resource listing from the MARC27 marketplace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceTool {
    pub name: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub resource_type: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub pricing: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub download_count: u64,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub license: Option<String>,
}

/// A single hit from the semantic find_resource search. Mirrors the
/// JSON shape the platform returns from `POST /marketplace/find`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceFindHit {
    /// Canonical name the agent invokes (e.g. `predict.elastic_moduli.mace`).
    pub canonical_name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    /// Resource type (e.g. `model`, `cli_tool`, `procedural_skill`).
    #[serde(default)]
    pub category: String,
    /// How the tool dispatches when invoked: `inference`, `local_shell`,
    /// `mcp_server`, etc. Empty when not applicable to this resource type.
    #[serde(default)]
    pub execution_target: String,
    /// Cosine similarity in [0, 1]; higher = closer to the query. Used by
    /// the agent to decide whether to invoke or fall back.
    #[serde(default)]
    pub score: f32,
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

    /// Search marketplace resources, optionally filtered by a query.
    pub async fn list_tools(&self, query: Option<&str>) -> Result<Vec<MarketplaceTool>> {
        let path = match query {
            Some(q) => {
                let encoded = urlencoding(q);
                format!("/marketplace/search?q={encoded}")
            }
            None => "/marketplace/resources".to_string(),
        };
        debug!(%path, "listing marketplace resources");
        self.platform.get(&path).await
    }

    /// Get a single resource by slug.
    ///
    /// The platform detail route is `/marketplace/{slug}` — NOT
    /// `/marketplace/resources/{slug}` (that prefix only aliases the
    /// listing; the old URL 404'd for every resource).
    pub async fn get_tool(&self, name: &str) -> Result<MarketplaceTool> {
        let path = format!("/marketplace/{name}");
        debug!(%path, "fetching marketplace resource");
        self.platform.get(&path).await
    }

    /// Semantic discovery of marketplace resources via the platform's
    /// `find_resource` cosine-similarity search.
    ///
    /// Pairs with `POST /api/v1/marketplace/find` (marc27-core #33). The
    /// platform side is the same path the research-engine REPL uses
    /// internally via the injected `find_tool()` function; this client
    /// wraps it for the PRISM CLI surface so chat-LLM tools can call it
    /// directly without going through the research-engine REPL.
    ///
    /// `types` restricts to specific resource_type values (e.g. `["model",
    /// "cli_tool"]`); pass `&[]` to search every type. `limit` caps the
    /// number of hits returned (typical: 3–10).
    pub async fn find_tool(
        &self,
        query: &str,
        types: &[String],
        limit: usize,
    ) -> Result<Vec<MarketplaceFindHit>> {
        #[derive(Serialize)]
        struct FindRequest<'a> {
            query: &'a str,
            #[serde(skip_serializing_if = "<[String]>::is_empty")]
            types: &'a [String],
            limit: usize,
        }
        let body = FindRequest {
            query,
            types,
            limit,
        };
        debug!(%query, ?types, limit, "POST /marketplace/find");
        self.platform
            .post("/marketplace/find", &body)
            .await
            .context("marketplace/find request failed")
    }

    /// Get the install URL for a resource (used by `prism marketplace install`).
    pub async fn install_url(&self, name: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct InstallInfo {
            url: String,
        }

        let path = format!("/marketplace/{name}/install");
        debug!(%path, "fetching install URL");
        let info: InstallInfo = self
            .platform
            .get(&path)
            .await
            .context("failed to fetch install URL")?;
        Ok(info.url)
    }

    /// Pull the full marketplace tool catalog and return every published
    /// tool (resource_type = "cli_tool" or empty, i.e. a Python tool PRISM
    /// can install).  Used by the auto-update sync to diff against the
    /// local `~/.prism/tools/` directory.
    ///
    /// This is a thin wrapper over `list_tools(None)` that filters to
    /// installable Python tools (excludes models, datasets, workflows,
    /// which live under different install paths).
    pub async fn list_installable_tools(&self) -> Result<Vec<MarketplaceTool>> {
        let all = self.list_tools(None).await?;
        Ok(all
            .into_iter()
            .filter(|t| {
                let rt = t.resource_type.as_str();
                rt == "cli_tool" || rt == "tool" || rt.is_empty()
            })
            .collect())
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
