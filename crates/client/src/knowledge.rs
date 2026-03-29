//! MARC27 Knowledge Service API client.
//!
//! Mirrors the Python SDK's `client.knowledge.*` namespace, giving Rust code
//! access to the 200K+ node materials science knowledge graph on
//! platform.marc27.com.
//!
//! # Endpoints
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | `/knowledge/search?q=...` | Graph entity search |
//! | GET | `/knowledge/entity/{name}` | Entity + neighbors |
//! | GET | `/knowledge/paths?from=...&to=...` | Shortest paths |
//! | GET | `/knowledge/stats` | Graph statistics |
//! | POST | `/knowledge/search/semantic` | Semantic vector search |
//! | GET | `/knowledge/corpora` | List available data corpora |
//! | POST | `/knowledge/ingest-job` | Submit ingest job |

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::api::PlatformClient;

/// A knowledge graph entity (node).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KgEntity {
    pub name: String,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default, flatten)]
    pub properties: serde_json::Value,
}

/// Full entity with neighbors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KgEntityDetail {
    pub entity: Option<KgEntity>,
    #[serde(default)]
    pub neighbors: serde_json::Value,
}

/// A path between two entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KgPath {
    pub nodes: Vec<String>,
    pub relationships: Vec<String>,
    pub length: usize,
}

/// Graph-level statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KgStats {
    #[serde(default)]
    pub nodes: u64,
    #[serde(default)]
    pub edges: u64,
    #[serde(default)]
    pub entity_types: u64,
}

/// Semantic search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticResult {
    pub id: String,
    pub score: f64,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// A data corpus available on the platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Corpus {
    pub name: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub record_count: u64,
}

/// Result of an ingest job submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestJobResult {
    pub job_id: String,
    #[serde(default)]
    pub status: String,
}

/// Client for the MARC27 Knowledge Service API.
///
/// Wraps `PlatformClient` and adds knowledge-specific methods matching
/// the Python SDK's `client.knowledge.*` namespace.
pub struct KnowledgeClient<'a> {
    platform: &'a PlatformClient,
}

impl<'a> KnowledgeClient<'a> {
    pub fn new(platform: &'a PlatformClient) -> Self {
        Self { platform }
    }

    /// Search the knowledge graph for entities by name.
    ///
    /// Equivalent to Python: `client.knowledge.graph_search(term, limit=20)`
    pub async fn graph_search(&self, term: &str, limit: usize) -> Result<Vec<KgEntity>> {
        let encoded = urlencoding::encode(term);
        let path = format!("/knowledge/search?q={encoded}&limit={limit}");
        self.platform
            .get(&path)
            .await
            .context("knowledge graph_search failed")
    }

    /// Get an entity and its neighbors from the knowledge graph.
    ///
    /// Equivalent to Python: `client.knowledge.graph_entity(name, limit=10)`
    pub async fn graph_entity(&self, name: &str, limit: usize) -> Result<KgEntityDetail> {
        let encoded = urlencoding::encode(name);
        let path = format!("/knowledge/entity/{encoded}?limit={limit}");
        self.platform
            .get(&path)
            .await
            .context("knowledge graph_entity failed")
    }

    /// Find shortest paths between two entities.
    ///
    /// Equivalent to Python: `client.knowledge.graph_paths(from, to, max_hops=3)`
    pub async fn graph_paths(
        &self,
        from: &str,
        to: &str,
        max_hops: usize,
    ) -> Result<Vec<KgPath>> {
        let from_enc = urlencoding::encode(from);
        let to_enc = urlencoding::encode(to);
        let path = format!("/knowledge/paths?from={from_enc}&to={to_enc}&max_hops={max_hops}");
        self.platform
            .get(&path)
            .await
            .context("knowledge graph_paths failed")
    }

    /// Get knowledge graph statistics (total nodes, edges, entity types).
    ///
    /// Equivalent to Python: `client.knowledge.graph_stats()`
    pub async fn graph_stats(&self) -> Result<KgStats> {
        self.platform
            .get("/knowledge/stats")
            .await
            .context("knowledge graph_stats failed")
    }

    /// Semantic similarity search over embedded documents.
    ///
    /// Equivalent to Python: `client.knowledge.search(query, corpus_id=None, limit=10)`
    pub async fn semantic_search(
        &self,
        query: &str,
        corpus_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SemanticResult>> {
        let mut body = serde_json::json!({
            "query": query,
            "limit": limit,
        });
        if let Some(cid) = corpus_id {
            body["corpus_id"] = serde_json::Value::String(cid.to_string());
        }
        self.platform
            .post("/knowledge/search/semantic", &body)
            .await
            .context("knowledge semantic_search failed")
    }

    /// List available data corpora on the platform.
    ///
    /// Equivalent to Python: `client.knowledge.list_corpora(domain=None, kind=None, limit=50)`
    pub async fn list_corpora(
        &self,
        domain: Option<&str>,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Corpus>> {
        let mut params = vec![format!("limit={limit}")];
        if let Some(d) = domain {
            params.push(format!("domain={}", urlencoding::encode(d)));
        }
        if let Some(k) = kind {
            params.push(format!("kind={}", urlencoding::encode(k)));
        }
        let path = format!("/knowledge/corpora?{}", params.join("&"));
        self.platform
            .get(&path)
            .await
            .context("knowledge list_corpora failed")
    }

    /// Submit a knowledge ingest job.
    ///
    /// Equivalent to Python: `client.knowledge.ingest(url=..., query=..., mode="full")`
    pub async fn ingest_job(
        &self,
        source_url: Option<&str>,
        query: Option<&str>,
        mode: &str,
    ) -> Result<IngestJobResult> {
        let mut body = serde_json::json!({"mode": mode});
        if let Some(url) = source_url {
            body["source"] = serde_json::json!({"type": "url", "url": url});
        } else if let Some(q) = query {
            body["source"] = serde_json::json!({"type": "query", "query": q});
        }
        self.platform
            .post("/knowledge/ingest-job", &body)
            .await
            .context("knowledge ingest_job failed")
    }
}

/// Extension trait so callers can write `platform_client.knowledge().graph_search(...)`.
pub trait KnowledgeExt {
    fn knowledge(&self) -> KnowledgeClient<'_>;
}

impl KnowledgeExt for PlatformClient {
    fn knowledge(&self) -> KnowledgeClient<'_> {
        KnowledgeClient::new(self)
    }
}
