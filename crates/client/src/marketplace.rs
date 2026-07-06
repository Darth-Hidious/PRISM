use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::api::PlatformClient;

/// Deserialize a field the platform may send as JSON `null` into `T::default`.
///
/// `#[serde(default)]` alone only covers a *missing* key — an explicit `null`
/// still errors with "invalid type: null, expected a string" and takes down the
/// whole listing. The marketplace legitimately returns `null` for optional text
/// (e.g. HF-imported resources with no `description`), so coerce null → default
/// rather than failing every resource because one field is empty.
fn null_to_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// A resource listing from the MARC27 marketplace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceTool {
    pub name: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub slug: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub resource_type: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub version: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub description: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    pub pricing: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub tags: Vec<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    pub download_count: u64,
    #[serde(default, deserialize_with = "null_to_default")]
    pub status: String,
    #[serde(default)]
    pub license: Option<String>,
    /// How the resource is served: `on_demand` (endpoint-based, deployed on
    /// request) vs artifact-backed. Endpoint-hosted resources have nothing
    /// to download.
    #[serde(default, deserialize_with = "null_to_default")]
    pub hosting: String,
    /// Storage location of the downloadable artifact. `None`/empty means the
    /// marketplace holds no artifact for this resource — the install
    /// endpoint 422s for such resources, so sync must skip them.
    #[serde(default)]
    pub storage_path: Option<String>,
}

/// A single hit from the semantic find_resource search. Mirrors the
/// JSON shape the platform returns from `POST /marketplace/find`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceFindHit {
    /// Canonical name the agent invokes (e.g. `predict.elastic_moduli.mace`).
    pub canonical_name: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub display_name: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub description: String,
    /// Resource type (e.g. `model`, `cli_tool`, `procedural_skill`).
    #[serde(default, deserialize_with = "null_to_default")]
    pub category: String,
    /// How the tool dispatches when invoked: `inference`, `local_shell`,
    /// `mcp_server`, etc. Empty when not applicable to this resource type.
    #[serde(default, deserialize_with = "null_to_default")]
    pub execution_target: String,
    /// Cosine similarity in [0, 1]; higher = closer to the query. Used by
    /// the agent to decide whether to invoke or fall back.
    #[serde(default, deserialize_with = "null_to_default")]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marketplace_tool_tolerates_null_text_fields() {
        // Reproduces the prod failure: `GET /marketplace/resources` returns
        // `"description": null` for HF-imported resources with no description.
        // Before the null_to_default coercion this errored with
        // "invalid type: null, expected a string" and killed the whole listing.
        let json = r#"{
            "name": "PsiBotAI/SynData",
            "slug": "hf-dataset-psibotai-syndata",
            "resource_type": "dataset",
            "description": null,
            "owner_id": null,
            "org_id": null,
            "pricing": null,
            "status": null,
            "hosting": null,
            "tags": null,
            "download_count": null
        }"#;
        let tool: MarketplaceTool = serde_json::from_str(json).expect("null text must not fail");
        assert_eq!(tool.name, "PsiBotAI/SynData");
        assert_eq!(tool.description, "");
        assert_eq!(tool.pricing, "");
        assert_eq!(tool.tags, Vec::<String>::new());
        assert_eq!(tool.download_count, 0);
    }

    #[test]
    fn marketplace_tool_still_reads_present_fields() {
        let json = r#"{
            "name": "uip-mace",
            "description": "Universal MACE potential",
            "download_count": 42,
            "tags": ["materials", "mlip"]
        }"#;
        let tool: MarketplaceTool = serde_json::from_str(json).unwrap();
        assert_eq!(tool.description, "Universal MACE potential");
        assert_eq!(tool.download_count, 42);
        assert_eq!(tool.tags, vec!["materials", "mlip"]);
    }

    #[test]
    fn find_hit_tolerates_null_fields() {
        let json = r#"{
            "canonical_name": "predict.elastic_moduli.mace",
            "display_name": null,
            "description": null,
            "score": null
        }"#;
        let hit: MarketplaceFindHit = serde_json::from_str(json).unwrap();
        assert_eq!(hit.canonical_name, "predict.elastic_moduli.mace");
        assert_eq!(hit.display_name, "");
        assert_eq!(hit.score, 0.0);
    }
}
