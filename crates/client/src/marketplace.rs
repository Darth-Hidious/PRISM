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

/// A resource listing from the configured provider marketplace.
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
    /// Publisher-supplied metadata blob. PRISM's own tool entries carry an
    /// `install` object here saying what the user must `pip install`; see
    /// [`MarketplaceTool::install_instructions`].
    #[serde(default, deserialize_with = "null_to_default")]
    pub metadata: serde_json::Value,
}

impl MarketplaceTool {
    /// The command a user runs to make this resource usable, when the
    /// resource has no downloadable artifact but says how to install itself.
    ///
    /// Resources whose capability ships inside PRISM (the materials tools)
    /// have nothing to download — `GET /{slug}/install` 422s for them. An
    /// entry that only 422'd would be worse than no entry, so it declares
    /// `metadata.install.command` instead and the CLI prints that.
    #[must_use]
    pub fn install_instructions(&self) -> Option<(String, Option<String>)> {
        let install = self.metadata.get("install")?;
        let command = install.get("command")?.as_str()?.to_string();
        let note = install
            .get("note")
            .and_then(|n| n.as_str())
            .map(str::to_string);
        Some((command, note))
    }
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

/// Client for provider marketplace endpoints.
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

    /// Publish one catalog entry: create it, then set the fields the create
    /// route does not accept, then submit it for review.
    ///
    /// `POST /marketplace` takes only name/slug/type/description/metadata and
    /// lands the resource in `draft`; tags and license go through
    /// `PATCH /marketplace/{slug}`; `POST /marketplace/{slug}/submit` moves
    /// draft → pending_review. Nothing here approves anything — a reviewer
    /// with rights on the platform still has to, or the entry stays invisible
    /// to the public listing.
    pub async fn publish_entry(&self, entry: &CatalogEntry) -> Result<()> {
        #[derive(Serialize)]
        struct Create<'a> {
            name: &'a str,
            slug: &'a str,
            resource_type: &'a str,
            description: &'a str,
            metadata: &'a serde_json::Value,
        }
        #[derive(Serialize)]
        struct Patch<'a> {
            tags: &'a [String],
            license: &'a str,
            metadata: &'a serde_json::Value,
        }

        let created: serde_json::Value = self
            .platform
            .post(
                "/marketplace",
                &Create {
                    name: &entry.name,
                    slug: &entry.slug,
                    resource_type: &entry.resource_type,
                    description: &entry.description,
                    metadata: &entry.metadata,
                },
            )
            .await
            .with_context(|| format!("publishing {}", entry.slug))?;
        debug!(slug = %entry.slug, ?created, "resource created (draft)");

        let _: serde_json::Value = self
            .platform
            .patch(
                &format!("/marketplace/{}", entry.slug),
                &Patch {
                    tags: &entry.tags,
                    license: &entry.license,
                    metadata: &entry.metadata,
                },
            )
            .await
            .with_context(|| format!("setting tags/license on {}", entry.slug))?;

        let _: serde_json::Value = self
            .platform
            .post(&format!("/marketplace/{}/submit", entry.slug), &())
            .await
            .with_context(|| format!("submitting {} for review", entry.slug))?;
        Ok(())
    }
}

/// One publishable entry from `app/tools/marketplace_catalog.json`.
///
/// The JSON is the single source of truth — `app/tools/marketplace_catalog.py`
/// and `tests/test_marketplace_catalog.py` hold it to account against the live
/// Python tool registry and the imports those tools actually perform, so an
/// entry cannot promise an install that fails on import. This struct only
/// carries it to the platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub slug: String,
    pub name: String,
    pub resource_type: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub license: String,
    #[serde(default)]
    pub requires_extras: Vec<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// The catalog compiled into the binary.
#[derive(Debug, Clone, Deserialize)]
pub struct Catalog {
    pub entries: Vec<CatalogEntry>,
    /// tool name → why it is deliberately NOT published.
    pub bundled: std::collections::BTreeMap<String, String>,
}

const CATALOG_JSON: &str = include_str!("../../../app/tools/marketplace_catalog.json");

/// PRISM's own publishable tool catalog. Parsed once per call — it is a few
/// KB of static JSON and publishing is not a hot path.
pub fn builtin_catalog() -> Result<Catalog> {
    serde_json::from_str(CATALOG_JSON).context("app/tools/marketplace_catalog.json is malformed")
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

    #[test]
    fn builtin_catalog_parses_and_is_not_empty() {
        let catalog = builtin_catalog().expect("catalog must parse");
        assert!(!catalog.entries.is_empty());
        assert!(!catalog.bundled.is_empty());
        // `resource_type` must be one the platform enum accepts AND one the
        // hub already renders a tab for — inventing a kind the UI cannot
        // display is how a listing becomes invisible.
        const KINDS: &[&str] = &[
            "plugin",
            "model",
            "mcp_server",
            "cli_tool",
            "hpc_cluster",
            "robot_lab",
            "test_facility",
            "dataset",
            "procedural_skill",
        ];
        for entry in &catalog.entries {
            assert!(
                KINDS.contains(&entry.resource_type.as_str()),
                "{}",
                entry.slug
            );
            assert!(matches!(
                entry.license.as_str(),
                "MIT" | "LicenseRef-Mirdyne-Dual"
            ));
            assert!(!entry.slug.is_empty() && !entry.description.is_empty());
            // Shell execution must never become an installable listing.
            let tools = entry.metadata["tools"].as_array().expect("metadata.tools");
            for tool in tools {
                let name = tool.as_str().unwrap();
                assert!(
                    !matches!(
                        name,
                        "execute_bash" | "execute_python" | "bash_task" | "file"
                    ),
                    "{name} must stay bundled"
                );
            }
        }
    }

    #[test]
    fn catalog_entry_round_trips_through_the_marketplace_model() {
        // What we publish must come back as the listing type the rest of
        // PRISM reads — otherwise `marketplace search` renders the entries
        // we just created as blanks.
        for entry in builtin_catalog().unwrap().entries {
            let wire = serde_json::json!({
                "name": entry.name,
                "slug": entry.slug,
                "resource_type": entry.resource_type,
                "description": entry.description,
                "tags": entry.tags,
                "license": entry.license,
                "metadata": entry.metadata,
                "pricing": "free",
                "status": "pending_review",
                "hosting": "on_demand",
                "storage_path": null,
            });
            let tool: MarketplaceTool =
                serde_json::from_value(wire).expect("entry must deserialise as a listing");
            assert_eq!(tool.slug, entry.slug);
            assert_eq!(tool.tags, entry.tags);
            assert_eq!(tool.license.as_deref(), Some(entry.license.as_str()));
            // No artifact: the sync path must skip it, and the install path
            // must have something honest to say instead of a 422.
            assert!(tool.storage_path.is_none());
            let (command, _note) = tool
                .install_instructions()
                .expect("an artifact-less entry must declare how to install");
            assert!(
                command.starts_with("pip install") || command.starts_with("curl "),
                "{command}"
            );
        }
    }

    #[test]
    fn install_instructions_absent_without_metadata() {
        let tool: MarketplaceTool =
            serde_json::from_str(r#"{"name":"x","metadata":null}"#).unwrap();
        assert!(tool.install_instructions().is_none());
    }
}
