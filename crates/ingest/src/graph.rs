//! Neo4j graph store via HTTP Transactional Cypher API.
//!
//! Stores materials entities and relationships as a property graph.
//! Uses Neo4j's HTTP API (port 7474) — no bolt driver needed.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing;

use crate::{Entity, EntitySet, GraphUpdate, Neo4jConfig, Relationship};

/// Trait for graph database backends.
#[async_trait]
pub trait GraphStore: Send + Sync {
    /// Persist an entity set into the graph database.
    async fn upsert(&self, entities: &EntitySet) -> Result<GraphUpdate>;

    /// Query the graph for entities related to a given entity by name.
    async fn neighbors(&self, entity_name: &str, max_depth: usize) -> Result<EntitySet>;

    /// Delete all nodes and relationships (use with caution).
    async fn clear(&self) -> Result<()>;

    /// Count nodes and relationships in the graph.
    async fn stats(&self) -> Result<GraphStats>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStats {
    pub node_count: usize,
    pub relationship_count: usize,
}

/// Neo4j-backed graph store using the HTTP Transactional Cypher API.
pub struct Neo4jGraphStore {
    client: reqwest::Client,
    config: Neo4jConfig,
}

/// Neo4j HTTP API request body.
#[derive(Serialize)]
struct CypherRequest {
    statements: Vec<CypherStatement>,
}

#[derive(Serialize)]
struct CypherStatement {
    statement: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<serde_json::Value>,
}

/// Neo4j HTTP API response.
#[derive(Deserialize)]
struct CypherResponse {
    results: Vec<CypherResult>,
    errors: Vec<CypherError>,
}

#[derive(Deserialize)]
struct CypherResult {
    #[allow(dead_code)]
    columns: Vec<String>,
    data: Vec<CypherRow>,
}

#[derive(Deserialize)]
struct CypherRow {
    row: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct CypherError {
    code: String,
    message: String,
}

impl Neo4jGraphStore {
    pub fn new(config: Neo4jConfig) -> Self {
        let client = reqwest::Client::new();
        Self { client, config }
    }

    /// Transaction commit endpoint URL.
    fn tx_commit_url(&self) -> String {
        format!(
            "{}/db/{}/tx/commit",
            self.config.base_url, self.config.database
        )
    }

    /// Execute one or more Cypher statements in a single transaction.
    async fn execute(&self, statements: Vec<CypherStatement>) -> Result<CypherResponse> {
        let url = self.tx_commit_url();
        let body = CypherRequest { statements };

        let resp = self
            .client
            .post(&url)
            .basic_auth(&self.config.username, Some(&self.config.password))
            .json(&body)
            .send()
            .await
            .context("failed to connect to Neo4j")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Neo4j returned {}: {}", status, text);
        }

        let result: CypherResponse = resp.json().await.context("bad Neo4j response")?;

        if !result.errors.is_empty() {
            let err = &result.errors[0];
            bail!("Neo4j error {}: {}", err.code, err.message);
        }

        Ok(result)
    }

    /// Execute a single Cypher statement.
    async fn execute_one(
        &self,
        cypher: &str,
        params: Option<serde_json::Value>,
    ) -> Result<CypherResponse> {
        self.execute(vec![CypherStatement {
            statement: cypher.to_string(),
            parameters: params,
        }])
        .await
    }

    /// Build a MERGE Cypher statement for an entity node.
    fn entity_merge_cypher(entity: &Entity) -> CypherStatement {
        let label = sanitize_label(&entity.entity_type);
        let cypher = format!("MERGE (n:{label} {{name: $name}}) SET n += $props RETURN n.name");
        CypherStatement {
            statement: cypher,
            parameters: Some(serde_json::json!({
                "name": entity.name,
                "props": entity.properties,
            })),
        }
    }

    /// Build a MERGE Cypher statement for a relationship.
    fn relationship_merge_cypher(rel: &Relationship) -> CypherStatement {
        let rel_type = sanitize_label(&rel.rel_type);
        let mut props = serde_json::Map::new();
        if let Some(w) = rel.weight {
            props.insert("weight".into(), serde_json::json!(w));
        }
        if let Some(o) = rel.order {
            props.insert("order".into(), serde_json::json!(o));
        }

        let set_clause = if props.is_empty() {
            String::new()
        } else {
            " SET r += $props".to_string()
        };

        let cypher = format!(
            "MATCH (a {{name: $from}}), (b {{name: $to}}) \
             MERGE (a)-[r:{rel_type}]->(b){set_clause} \
             RETURN type(r)"
        );

        CypherStatement {
            statement: cypher,
            parameters: Some(serde_json::json!({
                "from": rel.from,
                "to": rel.to,
                "props": props,
            })),
        }
    }

    /// Check connectivity to Neo4j.
    pub async fn health_check(&self) -> Result<()> {
        self.execute_one("RETURN 1", None).await?;
        Ok(())
    }

    /// Execute an arbitrary read-only Cypher query and return rows as JSON values.
    pub async fn query_cypher(
        &self,
        cypher: &str,
        params: Option<serde_json::Value>,
    ) -> Result<Vec<serde_json::Value>> {
        let resp = self.execute_one(cypher, params).await?;
        let mut rows = Vec::new();
        if let Some(result) = resp.results.into_iter().next() {
            let columns = result.columns;
            for row in result.data {
                let mut obj = serde_json::Map::new();
                for (i, col) in columns.iter().enumerate() {
                    if let Some(val) = row.row.get(i) {
                        obj.insert(col.clone(), val.clone());
                    }
                }
                rows.push(serde_json::Value::Object(obj));
            }
        }
        Ok(rows)
    }

    /// Introspect the graph schema: node labels and relationship types.
    pub async fn schema(&self) -> Result<GraphSchemaInfo> {
        let labels_resp = self
            .execute_one("CALL db.labels() YIELD label RETURN label", None)
            .await?;
        let mut labels = Vec::new();
        if let Some(result) = labels_resp.results.into_iter().next() {
            for row in result.data {
                if let Some(label) = row.row.first().and_then(|v| v.as_str()) {
                    labels.push(label.to_string());
                }
            }
        }

        let rels_resp = self
            .execute_one(
                "CALL db.relationshipTypes() YIELD relationshipType RETURN relationshipType",
                None,
            )
            .await?;
        let mut relationship_types = Vec::new();
        if let Some(result) = rels_resp.results.into_iter().next() {
            for row in result.data {
                if let Some(rt) = row.row.first().and_then(|v| v.as_str()) {
                    relationship_types.push(rt.to_string());
                }
            }
        }

        Ok(GraphSchemaInfo {
            labels,
            relationship_types,
        })
    }
}

/// Graph schema metadata for NL→Cypher translation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphSchemaInfo {
    pub labels: Vec<String>,
    pub relationship_types: Vec<String>,
}

/// Sanitize a string for use as a Neo4j label or relationship type.
/// Only allows alphanumeric + underscore.
fn sanitize_label(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[async_trait]
impl GraphStore for Neo4jGraphStore {
    async fn upsert(&self, entities: &EntitySet) -> Result<GraphUpdate> {
        // Batch entity nodes first, then relationships.
        let mut nodes_created = 0usize;
        let mut edges_created = 0usize;

        // Upsert nodes in batches of 50.
        for chunk in entities.entities.chunks(50) {
            let statements: Vec<CypherStatement> =
                chunk.iter().map(Self::entity_merge_cypher).collect();
            let count = statements.len();
            self.execute(statements).await?;
            nodes_created += count;
            tracing::debug!(batch_size = count, "upserted entity batch");
        }

        // Upsert relationships in batches of 50.
        for chunk in entities.relationships.chunks(50) {
            let statements: Vec<CypherStatement> =
                chunk.iter().map(Self::relationship_merge_cypher).collect();
            let count = statements.len();
            self.execute(statements).await?;
            edges_created += count;
            tracing::debug!(batch_size = count, "upserted relationship batch");
        }

        tracing::info!(nodes_created, edges_created, "graph upsert complete");
        Ok(GraphUpdate {
            nodes_created,
            edges_created,
        })
    }

    async fn neighbors(&self, entity_name: &str, max_depth: usize) -> Result<EntitySet> {
        let depth = max_depth.min(5); // cap traversal depth
        let cypher = format!(
            "MATCH (start {{name: $name}})-[r*1..{depth}]-(neighbor) \
             RETURN DISTINCT neighbor.name AS name, labels(neighbor)[0] AS type, \
             properties(neighbor) AS props"
        );

        let resp = self
            .execute_one(&cypher, Some(serde_json::json!({ "name": entity_name })))
            .await?;

        let mut entities = Vec::new();

        if let Some(result) = resp.results.into_iter().next() {
            for row in result.data {
                if row.row.len() >= 3 {
                    let name = row.row[0].as_str().unwrap_or("").to_string();
                    let entity_type = row.row[1].as_str().unwrap_or("Unknown").to_string();
                    let properties = row.row[2].clone();

                    entities.push(Entity {
                        entity_type,
                        name,
                        properties,
                    });
                }
            }
        }

        tracing::info!(
            entity = entity_name,
            neighbors = entities.len(),
            depth,
            "neighbor query complete"
        );

        Ok(EntitySet {
            entities,
            relationships: vec![], // Neighbor query doesn't return relationship details
        })
    }

    async fn clear(&self) -> Result<()> {
        tracing::warn!("clearing entire Neo4j graph");
        self.execute_one("MATCH (n) DETACH DELETE n", None).await?;
        Ok(())
    }

    async fn stats(&self) -> Result<GraphStats> {
        let resp = self
            .execute_one(
                "MATCH (n) WITH count(n) AS nodes \
                 OPTIONAL MATCH ()-[r]->() \
                 RETURN nodes, count(r) AS rels",
                None,
            )
            .await?;

        let (node_count, relationship_count) = resp
            .results
            .into_iter()
            .next()
            .and_then(|r| r.data.into_iter().next())
            .map(|row| {
                let nodes = row.row[0].as_u64().unwrap_or(0) as usize;
                let rels = row.row[1].as_u64().unwrap_or(0) as usize;
                (nodes, rels)
            })
            .unwrap_or((0, 0));

        Ok(GraphStats {
            node_count,
            relationship_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_label_strips_special_chars() {
        assert_eq!(sanitize_label("HAS_PROPERTY"), "HAS_PROPERTY");
        assert_eq!(sanitize_label("my-label"), "my_label");
        assert_eq!(sanitize_label("foo bar!"), "foo_bar_");
    }

    #[test]
    fn entity_merge_cypher_format() {
        let entity = Entity {
            entity_type: "Alloy".into(),
            name: "NbMoTaW".into(),
            properties: serde_json::json!({"system": "refractory"}),
        };
        let stmt = Neo4jGraphStore::entity_merge_cypher(&entity);
        assert!(stmt.statement.contains("MERGE"));
        assert!(stmt.statement.contains(":Alloy"));
        assert!(stmt.statement.contains("$name"));
    }

    #[test]
    fn relationship_merge_cypher_with_weight() {
        let rel = Relationship {
            from: "NbMoTaW".into(),
            rel_type: "CONTAINS".into(),
            to: "Nb".into(),
            weight: Some(0.25),
            order: None,
        };
        let stmt = Neo4jGraphStore::relationship_merge_cypher(&rel);
        assert!(stmt.statement.contains("MERGE"));
        assert!(stmt.statement.contains(":CONTAINS"));
        assert!(stmt.statement.contains("SET r += $props"));
    }

    #[test]
    fn relationship_merge_cypher_without_props() {
        let rel = Relationship {
            from: "A".into(),
            rel_type: "RELATED_TO".into(),
            to: "B".into(),
            weight: None,
            order: None,
        };
        let stmt = Neo4jGraphStore::relationship_merge_cypher(&rel);
        assert!(!stmt.statement.contains("SET"));
    }

    #[test]
    fn default_neo4j_config() {
        let cfg = Neo4jConfig::default();
        assert!(cfg.base_url.contains("7474"));
        assert_eq!(cfg.database, "neo4j");
    }

    // --- sanitize_label edge cases ---

    #[test]
    fn sanitize_label_empty_string() {
        assert_eq!(sanitize_label(""), "");
    }

    #[test]
    fn sanitize_label_all_special_chars() {
        assert_eq!(sanitize_label("!@#$%^&*()-+"), "____________");
    }

    #[test]
    fn sanitize_label_unicode_letters_kept() {
        // Unicode letters are alphanumeric — they must be preserved.
        let result = sanitize_label("αβγ");
        assert_eq!(result, "αβγ");
    }

    #[test]
    fn sanitize_label_unicode_with_special_chars() {
        let result = sanitize_label("α-phase");
        assert_eq!(result, "α_phase");
    }

    #[test]
    fn sanitize_label_leading_and_trailing_specials() {
        assert_eq!(sanitize_label("-label-"), "_label_");
    }

    // --- entity_merge_cypher with special chars in entity_type ---

    #[test]
    fn entity_merge_cypher_sanitizes_entity_type() {
        let entity = Entity {
            entity_type: "my-alloy type".into(),
            name: "X".into(),
            properties: serde_json::Value::Object(Default::default()),
        };
        let stmt = Neo4jGraphStore::entity_merge_cypher(&entity);
        // The label in the cypher must be sanitized — no hyphens or spaces.
        assert!(stmt.statement.contains(":my_alloy_type"));
        assert!(!stmt.statement.contains("my-alloy type"));
    }

    // --- relationship_merge_cypher with both weight and order ---

    #[test]
    fn relationship_merge_cypher_with_both_weight_and_order() {
        let rel = Relationship {
            from: "Mat".into(),
            rel_type: "PROCESSED_BY".into(),
            to: "Anneal".into(),
            weight: Some(0.5),
            order: Some(1),
        };
        let stmt = Neo4jGraphStore::relationship_merge_cypher(&rel);
        assert!(stmt.statement.contains("SET r += $props"));
        // Both weight and order must be in the serialized parameters.
        let params_str = serde_json::to_string(&stmt.parameters).unwrap();
        assert!(params_str.contains("\"weight\""));
        assert!(params_str.contains("\"order\""));
    }

    // --- tx_commit_url format ---

    #[test]
    fn tx_commit_url_includes_database_and_commit() {
        let store = Neo4jGraphStore::new(Neo4jConfig {
            base_url: "http://localhost:7474".into(),
            database: "materials".into(),
            username: "neo4j".into(),
            password: "neo4j".into(),
        });
        let url = store.tx_commit_url();
        assert_eq!(url, "http://localhost:7474/db/materials/tx/commit");
    }

    #[test]
    fn tx_commit_url_uses_default_database() {
        let store = Neo4jGraphStore::new(Neo4jConfig::default());
        let url = store.tx_commit_url();
        assert!(url.contains("/db/neo4j/tx/commit"));
    }

    // --- default Neo4j config values ---

    #[test]
    fn default_neo4j_config_username_and_password() {
        let cfg = Neo4jConfig::default();
        assert_eq!(cfg.username, "neo4j");
        assert_eq!(cfg.password, "neo4j");
    }
}
