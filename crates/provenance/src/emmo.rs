//! EMMO materials ontology + PROV-O assertions on the same Turso store.
//!
//! Local mirror of marc27-core's cloud ontology writers (`ontology/schema.rs`,
//! `ontology/holistic.rs`, `ontology/prov.rs`) expressed as SQL tables instead
//! of a property graph. Typed entities and edges follow the EMMO taxonomy
//! (Matter, Measurement, Property, Phase, …); every written fact is also
//! reified as a PROV-O assertion with noisy-OR corroboration, so the graph
//! and the audit trail stay consistent.
//!
//! The read API returns the exact shapes the cloud research LLM consumes
//! (`GraphNode` / `GraphEdge` / `TraversalResult` / `RecalledFact`), so a
//! federated fetch from this local store is a drop-in.

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use turso::Value;

use crate::{get_str, ProvenanceStore};

// ─────────────────────────────────────────────────────────────────────────
// Write-side types (mirror core's `ExtractedFact` / `Provenance`)
// ─────────────────────────────────────────────────────────────────────────

/// One extracted fact in the EMMO-aligned shape (mirrors core's
/// `ExtractedFact`, holistic.rs). `kind` routes to the right typed
/// node/edge structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    /// EMMO shape hint: measurement | phase | composition | contains |
    /// processing | structure | application. Unknown/None falls back to a
    /// generic edge.
    #[serde(default)]
    pub kind: Option<String>,
}

/// Who ran the extraction and over what (mirrors core's `Provenance`, plus
/// `locality` = "local" | "cloud" recording where the write happened).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalProvenance {
    pub activity_id: String,
    pub agent_id: String,
    pub agent_kind: String,
    pub source_entity_id: String,
    pub source_kind: String,
    pub tenant: String,
    pub started_at: String,
    pub ended_at: String,
    pub locality: String,
}

/// A subject/predicate/object triple to reify as a PROV-O assertion
/// (mirrors core's `Assertion`; the stable id is derived, not carried).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalAssertion {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default)]
    pub confidence: Option<f64>,
}

// ─────────────────────────────────────────────────────────────────────────
// Read-side types — field names must match the cloud shapes EXACTLY
// ─────────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GraphNode {
    pub name: String,
    pub entity_type: String,
    pub label: String,
    pub tenant: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub rel_type: String,
    pub count: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TraversalResult {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RecalledFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f64,
    pub source: String,
    pub agent: String,
}

// ─────────────────────────────────────────────────────────────────────────
// Canonicalization + assertion identity
// ─────────────────────────────────────────────────────────────────────────

/// Deterministic canonical key: trim, lowercase, collapse whitespace.
/// Self-consistent locally (need not match the cloud's resolver).
#[must_use]
pub fn canonical_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Label-qualified entity key ("{label}:{canonical name}"). Qualifying by
/// label keeps one node per (label, name) — the same name extracted as e.g.
/// both a Phase and a Matter stays two nodes instead of one label-churning
/// row (mirrors core, which keeps a node per label).
fn entity_key(label: &str, name: &str) -> String {
    format!("{label}:{}", canonical_key(name))
}

/// Stable assertion id: SHA-256 of `canonical(subject)|predicate|canonical(object)`,
/// so re-extraction corroborates one row instead of duplicating facts.
#[must_use]
pub fn assertion_id(subject: &str, predicate: &str, object: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(canonical_key(subject).as_bytes());
    h.update(b"|");
    h.update(predicate.as_bytes());
    h.update(b"|");
    h.update(canonical_key(object).as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Combine independent evidence for the same fact (noisy-OR): each new
/// sighting shrinks the remaining doubt multiplicatively. Capped below 1.0 —
/// corroboration never yields certainty (mirrors core).
fn corroborate_confidence(old: f64, new_evidence: f64) -> f64 {
    let combined = 1.0 - (1.0 - old.clamp(0.0, 1.0)) * (1.0 - new_evidence.clamp(0.0, 1.0));
    combined.min(0.99)
}

// ─────────────────────────────────────────────────────────────────────────
// Schema (called from `ProvenanceStore::init_schema`)
// ─────────────────────────────────────────────────────────────────────────

pub(crate) async fn init_schema(conn: &turso::Connection) -> Result<()> {
    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS emmo_entity (
            key TEXT PRIMARY KEY,
            name TEXT,
            label TEXT,
            entity_type TEXT,
            tenant TEXT,
            props_json TEXT,
            created_at TEXT
        )"#,
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_entity_tenant ON emmo_entity(tenant)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_entity_label ON emmo_entity(label)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_entity_name ON emmo_entity(name)",
        (),
    )
    .await?;

    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS emmo_edge (
            id TEXT PRIMARY KEY,
            source_key TEXT,
            target_key TEXT,
            rel_type TEXT,
            predicate TEXT,
            confidence REAL,
            tenant TEXT,
            props_json TEXT
        )"#,
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_edge_source ON emmo_edge(source_key)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_edge_target ON emmo_edge(target_key)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_edge_tenant ON emmo_edge(tenant)",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS prov_agent (id TEXT PRIMARY KEY, kind TEXT)",
        (),
    )
    .await?;

    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS prov_activity (
            id TEXT PRIMARY KEY,
            agent_id TEXT,
            source_entity_id TEXT,
            tenant TEXT,
            started_at TEXT,
            ended_at TEXT,
            locality TEXT
        )"#,
        (),
    )
    .await?;

    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS prov_assertion (
            id TEXT PRIMARY KEY,
            subject TEXT,
            predicate TEXT,
            object TEXT,
            confidence REAL,
            corroborations INTEGER,
            activity_id TEXT,
            source TEXT,
            agent TEXT,
            tenant TEXT
        )"#,
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_prov_assertion_tenant ON prov_assertion(tenant)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_prov_assertion_subject ON prov_assertion(subject)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_prov_assertion_object ON prov_assertion(object)",
        (),
    )
    .await?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Write API
// ─────────────────────────────────────────────────────────────────────────

impl ProvenanceStore {
    /// UPSERT one typed entity, merging on its label-qualified canonical key
    /// so re-ingest never duplicates (mirrors core's MERGE-per-label).
    /// Returns the key for edge writes. Last write wins on name;
    /// `props_json` is only replaced when provided.
    async fn upsert_entity(
        &self,
        name: &str,
        label: &str,
        tenant: &str,
        props_json: Option<String>,
    ) -> Result<String> {
        let key = entity_key(label, name);
        self.conn
            .execute(
                r#"INSERT INTO emmo_entity
                   (key, name, label, entity_type, tenant, props_json, created_at)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                   ON CONFLICT(key) DO UPDATE SET
                       name = excluded.name,
                       label = excluded.label,
                       entity_type = excluded.entity_type,
                       tenant = excluded.tenant,
                       props_json = COALESCE(excluded.props_json, emmo_entity.props_json)"#,
                [
                    Value::Text(key.clone()),
                    Value::Text(name.to_string()),
                    Value::Text(label.to_string()),
                    // No separate short-code taxonomy locally — the EMMO label
                    // doubles as the entity_type the read shapes expose.
                    Value::Text(label.to_string()),
                    Value::Text(tenant.to_string()),
                    match props_json {
                        Some(p) => Value::Text(p),
                        None => Value::Null,
                    },
                    Value::Text(Utc::now().to_rfc3339()),
                ],
            )
            .await?;
        Ok(key)
    }

    /// UPSERT one typed edge. The id is deterministic over
    /// (tenant, source, rel_type, target) so re-ingest updates in place.
    /// `props_json` carries edge attributes (e.g. a composition fraction or
    /// a processing-step order) and is only replaced when provided.
    #[allow(clippy::too_many_arguments)]
    async fn upsert_edge(
        &self,
        source_key: &str,
        target_key: &str,
        rel_type: &str,
        predicate: &str,
        confidence: f64,
        tenant: &str,
        props_json: Option<&str>,
    ) -> Result<()> {
        let id = format!("{tenant}|{source_key}|{rel_type}|{target_key}");
        self.conn
            .execute(
                r#"INSERT INTO emmo_edge
                   (id, source_key, target_key, rel_type, predicate, confidence, tenant, props_json)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                   ON CONFLICT(id) DO UPDATE SET
                       predicate = excluded.predicate,
                       confidence = excluded.confidence,
                       props_json = COALESCE(excluded.props_json, emmo_edge.props_json)"#,
                [
                    Value::Text(id),
                    Value::Text(source_key.to_string()),
                    Value::Text(target_key.to_string()),
                    Value::Text(rel_type.to_string()),
                    Value::Text(predicate.to_string()),
                    Value::Real(confidence),
                    Value::Text(tenant.to_string()),
                    match props_json {
                        Some(p) => Value::Text(p.to_string()),
                        None => Value::Null,
                    },
                ],
            )
            .await?;
        Ok(())
    }

    /// Write one fact as typed EMMO entities + edges, routing on `fact.kind`
    /// exactly like core's typed `write_*_fact` writers, then reify it as a
    /// PROV-O assertion so graph and audit trail stay consistent.
    pub async fn write_fact(&self, fact: &LocalFact, prov: &LocalProvenance) -> Result<()> {
        let confidence = fact.confidence.unwrap_or(0.5);
        let tenant = prov.tenant.as_str();

        match fact.kind.as_deref() {
            Some("measurement") => {
                // Mirror core: a measurement without a value fails schema
                // validation and is dropped (not written half-typed, not
                // recorded as an assertion).
                let Some(value) = fact.value else {
                    return Ok(());
                };
                let unit = fact.unit.clone().unwrap_or_default();
                let meas_name = format!(
                    "meas_{}_{}_{value}",
                    canonical_key(&fact.subject),
                    canonical_key(&fact.object)
                );
                let props = serde_json::json!({
                    "value": value, "unit": unit, "confidence": confidence
                });
                let subj_key = self
                    .upsert_entity(&fact.subject, "Matter", tenant, None)
                    .await?;
                let meas_key = self
                    .upsert_entity(&meas_name, "Measurement", tenant, Some(props.to_string()))
                    .await?;
                let obj_key = self
                    .upsert_entity(&fact.object, "Property", tenant, None)
                    .await?;
                self.upsert_edge(&subj_key, &meas_key, "HAS_MEASUREMENT", &fact.predicate, confidence, tenant, None)
                    .await?;
                self.upsert_edge(&meas_key, &obj_key, "OF_PROPERTY", &fact.predicate, confidence, tenant, None)
                    .await?;
            }
            Some("phase") => {
                let subj_key = self
                    .upsert_entity(&fact.subject, "Matter", tenant, None)
                    .await?;
                let obj_key = self
                    .upsert_entity(&fact.object, "Phase", tenant, None)
                    .await?;
                self.upsert_edge(&subj_key, &obj_key, "HAS_PHASE", &fact.predicate, confidence, tenant, None)
                    .await?;
            }
            Some("composition") => {
                let props = serde_json::json!({ "canonical_formula": &fact.object });
                let subj_key = self
                    .upsert_entity(&fact.subject, "Matter", tenant, None)
                    .await?;
                let obj_key = self
                    .upsert_entity(&fact.object, "Composition", tenant, Some(props.to_string()))
                    .await?;
                self.upsert_edge(&subj_key, &obj_key, "HAS_COMPOSITION", &fact.predicate, confidence, tenant, None)
                    .await?;
            }
            // Mirrors core's Element node + CONTAINS_ELEMENT edge; the
            // composition fraction (when `value` carries it) rides on the
            // edge props, not on the nodes.
            Some("contains") => {
                let props = fact
                    .value
                    .map(|f| serde_json::json!({ "fraction": f }).to_string());
                let subj_key = self
                    .upsert_entity(&fact.subject, "Matter", tenant, None)
                    .await?;
                let obj_key = self
                    .upsert_entity(&fact.object, "Element", tenant, None)
                    .await?;
                self.upsert_edge(&subj_key, &obj_key, "CONTAINS_ELEMENT", &fact.predicate, confidence, tenant, props.as_deref())
                    .await?;
            }
            Some("processing") => {
                // The step order (when `value` carries it) rides on the edge.
                let props = fact
                    .value
                    .map(|o| serde_json::json!({ "order": o }).to_string());
                let subj_key = self
                    .upsert_entity(&fact.subject, "Matter", tenant, None)
                    .await?;
                let obj_key = self
                    .upsert_entity(&fact.object, "Manufacturing", tenant, None)
                    .await?;
                self.upsert_edge(&subj_key, &obj_key, "PROCESSED_BY", &fact.predicate, confidence, tenant, props.as_deref())
                    .await?;
            }
            Some("structure") => {
                let props = serde_json::json!({ "system": &fact.object });
                let subj_key = self
                    .upsert_entity(&fact.subject, "Matter", tenant, None)
                    .await?;
                let obj_key = self
                    .upsert_entity(&fact.object, "CrystalStructure", tenant, Some(props.to_string()))
                    .await?;
                self.upsert_edge(&subj_key, &obj_key, "HAS_STRUCTURE", &fact.predicate, confidence, tenant, None)
                    .await?;
            }
            Some("application") => {
                let subj_key = self
                    .upsert_entity(&fact.subject, "Matter", tenant, None)
                    .await?;
                let obj_key = self
                    .upsert_entity(&fact.object, "Application", tenant, None)
                    .await?;
                self.upsert_edge(&subj_key, &obj_key, "USED_IN", &fact.predicate, confidence, tenant, None)
                    .await?;
            }
            // Unknown kind: keep the fact as a generic edge, don't drop it.
            _ => {
                let subj_key = self
                    .upsert_entity(&fact.subject, "Matter", tenant, None)
                    .await?;
                let obj_key = self
                    .upsert_entity(&fact.object, "Entity", tenant, None)
                    .await?;
                self.upsert_edge(&subj_key, &obj_key, &fact.predicate, &fact.predicate, confidence, tenant, None)
                    .await?;
            }
        }

        self.record_assertion(
            &LocalAssertion {
                subject: fact.subject.clone(),
                predicate: fact.predicate.clone(),
                object: fact.object.clone(),
                confidence: fact.confidence,
            },
            prov,
        )
        .await
    }

    /// UPSERT the PROV-O agent + activity for one run (idempotent).
    pub async fn record_activity(&self, prov: &LocalProvenance) -> Result<()> {
        self.conn
            .execute(
                r#"INSERT INTO prov_agent (id, kind) VALUES (?1, ?2)
                   ON CONFLICT(id) DO UPDATE SET kind = excluded.kind"#,
                [
                    Value::Text(prov.agent_id.clone()),
                    Value::Text(prov.agent_kind.clone()),
                ],
            )
            .await?;
        self.conn
            .execute(
                r#"INSERT INTO prov_activity
                   (id, agent_id, source_entity_id, tenant, started_at, ended_at, locality)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                   ON CONFLICT(id) DO UPDATE SET
                       agent_id = excluded.agent_id,
                       source_entity_id = excluded.source_entity_id,
                       tenant = excluded.tenant,
                       started_at = excluded.started_at,
                       ended_at = excluded.ended_at,
                       locality = excluded.locality"#,
                [
                    Value::Text(prov.activity_id.clone()),
                    Value::Text(prov.agent_id.clone()),
                    Value::Text(prov.source_entity_id.clone()),
                    Value::Text(prov.tenant.clone()),
                    Value::Text(prov.started_at.clone()),
                    Value::Text(prov.ended_at.clone()),
                    Value::Text(prov.locality.clone()),
                ],
            )
            .await?;
        Ok(())
    }

    /// Reify one triple as a PROV-O assertion. First sighting inserts with
    /// the extractor's confidence and `corroborations = 1`; every re-record
    /// of the same triple (stable SHA-256 id over canonical forms) combines
    /// confidence noisy-OR and increments `corroborations`.
    pub async fn record_assertion(
        &self,
        a: &LocalAssertion,
        prov: &LocalProvenance,
    ) -> Result<()> {
        self.record_activity(prov).await?;

        let id = assertion_id(&a.subject, &a.predicate, &a.object);
        let evidence = a.confidence.unwrap_or(1.0);

        // Read current belief, then corroborate or insert. The id is a
        // tenant-independent hash and the table's PRIMARY KEY (per schema),
        // so identical triples share one row; reads stay tenant-filtered.
        //
        // The read cursor is fully consumed and dropped BEFORE the write:
        // turso (pre-release) mishandles a write issued while a read
        // statement is still open on the same connection — the write lands
        // but later table scans can see the stale row.
        let existing = {
            let mut rows = self
                .conn
                .query(
                    "SELECT confidence, corroborations FROM prov_assertion WHERE id = ?1",
                    [Value::Text(id.clone())],
                )
                .await?;
            match rows.next().await? {
                Some(row) => {
                    let old_conf = row
                        .get_value(0)
                        .ok()
                        .and_then(|v| v.as_real().copied())
                        .unwrap_or(0.0);
                    let old_corr = row
                        .get_value(1)
                        .ok()
                        .and_then(|v| v.as_integer().copied())
                        .unwrap_or(1);
                    while rows.next().await?.is_some() {}
                    Some((old_conf, old_corr))
                }
                None => None,
            }
        };
        if let Some((old_conf, old_corr)) = existing {
            self.conn
                .execute(
                    r#"UPDATE prov_assertion
                       SET confidence = ?1, corroborations = ?2,
                           activity_id = ?3, source = ?4, agent = ?5
                       WHERE id = ?6"#,
                    [
                        Value::Real(corroborate_confidence(old_conf, evidence)),
                        Value::Integer(old_corr + 1),
                        Value::Text(prov.activity_id.clone()),
                        Value::Text(prov.source_entity_id.clone()),
                        Value::Text(prov.agent_id.clone()),
                        Value::Text(id),
                    ],
                )
                .await?;
        } else {
            self.conn
                .execute(
                    r#"INSERT INTO prov_assertion
                       (id, subject, predicate, object, confidence, corroborations,
                        activity_id, source, agent, tenant)
                       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
                    [
                        Value::Text(id),
                        Value::Text(a.subject.clone()),
                        Value::Text(a.predicate.clone()),
                        Value::Text(a.object.clone()),
                        Value::Real(evidence),
                        Value::Integer(1),
                        Value::Text(prov.activity_id.clone()),
                        Value::Text(prov.source_entity_id.clone()),
                        Value::Text(prov.agent_id.clone()),
                        Value::Text(prov.tenant.clone()),
                    ],
                )
                .await?;
        }
        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────
    // Read API — cloud-shaped, tenant-scoped
    // ─────────────────────────────────────────────────────────────────────

    /// Substring search over entity names (shortest names first, like the
    /// cloud's CONTAINS fallback).
    pub async fn graph_search(
        &self,
        term: &str,
        tenant: &str,
        limit: i64,
    ) -> Result<Vec<GraphNode>> {
        let mut rows = self
            .conn
            .query(
                r#"SELECT name, entity_type, label, tenant FROM emmo_entity
                   WHERE tenant = ?1 AND name LIKE ?2
                   ORDER BY LENGTH(name) LIMIT ?3"#,
                [
                    Value::Text(tenant.to_string()),
                    Value::Text(format!("%{term}%")),
                    Value::Integer(limit),
                ],
            )
            .await?;
        let mut nodes = Vec::new();
        while let Some(row) = rows.next().await? {
            nodes.push(row_to_node(&row, 0)?);
        }
        Ok(nodes)
    }

    /// Edges incident to the named entity (resolved via its canonical key or
    /// exact display name) plus the adjacent nodes, optionally filtered by
    /// relationship type. Keys are label-qualified, so one name may resolve
    /// to several centers (e.g. the same name as Matter and as Phase) —
    /// edges of all of them are returned.
    pub async fn get_neighbors(
        &self,
        name: &str,
        rel_type: Option<&str>,
        tenant: &str,
        limit: i64,
    ) -> Result<TraversalResult> {
        // Resolve name → center keys/nodes: exact display name first
        // (indexed), else compare the canonical part of each key in Rust
        // (canonical_key is not expressible in SQL).
        let mut centers: Vec<(String, GraphNode)> = Vec::new();
        {
            let mut rows = self
                .conn
                .query(
                    r#"SELECT key, name, entity_type, label, tenant FROM emmo_entity
                       WHERE tenant = ?1 AND name = ?2"#,
                    [
                        Value::Text(tenant.to_string()),
                        Value::Text(name.to_string()),
                    ],
                )
                .await?;
            while let Some(row) = rows.next().await? {
                centers.push((get_str(&row, 0)?, row_to_node(&row, 1)?));
            }
        }
        if centers.is_empty() {
            let canon = canonical_key(name);
            let mut rows = self
                .conn
                .query(
                    "SELECT key, name, entity_type, label, tenant FROM emmo_entity \
                     WHERE tenant = ?1",
                    [Value::Text(tenant.to_string())],
                )
                .await?;
            while let Some(row) = rows.next().await? {
                let key = get_str(&row, 0)?;
                // "{label}:{canonical}"; a pre-qualification key is the
                // canonical name itself, so it still resolves.
                let key_canon = key.split_once(':').map_or(key.as_str(), |(_, c)| c);
                if key_canon == canon {
                    let node = row_to_node(&row, 1)?;
                    centers.push((key, node));
                }
            }
        }
        if centers.is_empty() {
            return Ok(TraversalResult {
                nodes: Vec::new(),
                edges: Vec::new(),
            });
        }

        let mut nodes: Vec<GraphNode> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (_, node) in &centers {
            if seen.insert(format!("{}:{}", node.label, node.name)) {
                nodes.push(node.clone());
            }
        }

        const EDGE_COLS: &str = "e.rel_type, \
             s.name, s.entity_type, s.label, s.tenant, \
             t.name, t.entity_type, t.label, t.tenant";
        let mut edges: Vec<GraphEdge> = Vec::new();
        let mut seen_edges: std::collections::HashSet<(String, String, String)> =
            std::collections::HashSet::new();
        // One edge query per center, each cursor fully drained before the
        // next statement (turso pre-release is sensitive to interleaved
        // open statements).
        for (center_key, _) in &centers {
            let mut rows = match rel_type {
                Some(rt) => {
                    self.conn
                        .query(
                            &format!(
                                "SELECT {EDGE_COLS} FROM emmo_edge e \
                                 JOIN emmo_entity s ON s.key = e.source_key \
                                 JOIN emmo_entity t ON t.key = e.target_key \
                                 WHERE e.tenant = ?1 AND (e.source_key = ?2 OR e.target_key = ?3) \
                                   AND e.rel_type = ?4 LIMIT ?5"
                            ),
                            [
                                Value::Text(tenant.to_string()),
                                Value::Text(center_key.clone()),
                                Value::Text(center_key.clone()),
                                Value::Text(rt.to_string()),
                                Value::Integer(limit),
                            ],
                        )
                        .await?
                }
                None => {
                    self.conn
                        .query(
                            &format!(
                                "SELECT {EDGE_COLS} FROM emmo_edge e \
                                 JOIN emmo_entity s ON s.key = e.source_key \
                                 JOIN emmo_entity t ON t.key = e.target_key \
                                 WHERE e.tenant = ?1 AND (e.source_key = ?2 OR e.target_key = ?3) \
                                 LIMIT ?4"
                            ),
                            [
                                Value::Text(tenant.to_string()),
                                Value::Text(center_key.clone()),
                                Value::Text(center_key.clone()),
                                Value::Integer(limit),
                            ],
                        )
                        .await?
                }
            };
            while let Some(row) = rows.next().await? {
                let source = row_to_node(&row, 1)?;
                let target = row_to_node(&row, 5)?;
                let rel = get_str(&row, 0)?;
                // An edge between two centers shows up in both queries.
                if !seen_edges.insert((source.name.clone(), target.name.clone(), rel.clone())) {
                    continue;
                }
                edges.push(GraphEdge {
                    source: source.name.clone(),
                    target: target.name.clone(),
                    rel_type: rel,
                    count: 1,
                });
                for node in [source, target] {
                    if seen.insert(format!("{}:{}", node.label, node.name)) {
                        nodes.push(node);
                    }
                }
            }
        }
        Ok(TraversalResult { nodes, edges })
    }

    /// Recall assertions whose subject or object matches the query,
    /// highest-confidence first.
    pub async fn recall(
        &self,
        query: &str,
        tenant: &str,
        limit: i64,
    ) -> Result<Vec<RecalledFact>> {
        let pattern = format!("%{query}%");
        let mut rows = self
            .conn
            .query(
                r#"SELECT subject, predicate, object, confidence, source, agent
                   FROM prov_assertion
                   WHERE tenant = ?1 AND (subject LIKE ?2 OR object LIKE ?3)
                   ORDER BY confidence DESC LIMIT ?4"#,
                [
                    Value::Text(tenant.to_string()),
                    Value::Text(pattern.clone()),
                    Value::Text(pattern),
                    Value::Integer(limit),
                ],
            )
            .await?;
        let mut facts = Vec::new();
        while let Some(row) = rows.next().await? {
            facts.push(RecalledFact {
                subject: get_str(&row, 0)?,
                predicate: get_str(&row, 1)?,
                object: get_str(&row, 2)?,
                confidence: row
                    .get_value(3)
                    .ok()
                    .and_then(|v| v.as_real().copied())
                    .unwrap_or(0.0),
                source: get_str(&row, 4)?,
                agent: get_str(&row, 5)?,
            });
        }
        Ok(facts)
    }
}

/// Read a `GraphNode` from four consecutive columns starting at `offset`
/// (name, entity_type, label, tenant).
fn row_to_node(row: &turso::Row, offset: usize) -> Result<GraphNode> {
    Ok(GraphNode {
        name: get_str(row, offset)?,
        entity_type: get_str(row, offset + 1)?,
        label: get_str(row, offset + 2)?,
        tenant: get_str(row, offset + 3)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Tempfile-backed Turso DB, removed (with WAL sidecars) on drop.
    struct TempDb {
        path: PathBuf,
    }

    impl TempDb {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("prism_emmo_test_{}.db", uuid::Uuid::new_v4()));
            Self { path }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut p = self.path.clone().into_os_string();
                p.push(suffix);
                let _ = std::fs::remove_file(p);
            }
        }
    }

    fn test_prov() -> LocalProvenance {
        LocalProvenance {
            activity_id: "act_test_1".into(),
            agent_id: "gemma-4-12b".into(),
            agent_kind: "SoftwareAgent".into(),
            source_entity_id: "doc:test_paper".into(),
            source_kind: "Document".into(),
            tenant: "t1".into(),
            started_at: "2026-07-13T00:00:00Z".into(),
            ended_at: "2026-07-13T00:00:01Z".into(),
            locality: "local".into(),
        }
    }

    fn fact(kind: &str, subject: &str, predicate: &str, object: &str) -> LocalFact {
        LocalFact {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            value: None,
            unit: None,
            confidence: Some(0.8),
            kind: Some(kind.into()),
        }
    }

    async fn count(store: &ProvenanceStore, sql: &str) -> i64 {
        let mut rows = store.conn.query(sql, ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        row.get_value(0)
            .ok()
            .and_then(|v| v.as_integer().copied())
            .unwrap_or(-1)
    }

    async fn query_str(store: &ProvenanceStore, sql: &str) -> String {
        let mut rows = store.conn.query(sql, ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        get_str(&row, 0).unwrap()
    }

    #[tokio::test]
    async fn write_fact_each_kind_is_searchable_and_traversable() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        let cases = [
            ("measurement", "Ti-6Al-4V", "has_measurement", "UTS", "HAS_MEASUREMENT"),
            ("phase", "Ti-6Al-4V", "has_phase", "alpha-beta", "HAS_PHASE"),
            ("composition", "Inconel 718", "has_composition", "NiCr19Fe18", "HAS_COMPOSITION"),
            ("contains", "Inconel 718", "contains", "Ni", "CONTAINS_ELEMENT"),
            ("processing", "Inconel 718", "processed_by", "LPBF", "PROCESSED_BY"),
            ("structure", "Ti-6Al-4V", "has_structure", "hexagonal", "HAS_STRUCTURE"),
            ("application", "Ti-6Al-4V", "used_in", "turbine blades", "USED_IN"),
        ];
        for (kind, s, p, o, rel) in cases {
            let mut f = fact(kind, s, p, o);
            if kind == "measurement" {
                f.value = Some(1140.0);
                f.unit = Some("MPa".into());
            }
            store.write_fact(&f, &prov).await.unwrap();

            let hits = store.graph_search(s, "t1", 10).await.unwrap();
            assert!(hits.iter().any(|n| n.name == s), "graph_search missed subject for {kind}");

            let tr = store.get_neighbors(s, Some(rel), "t1", 10).await.unwrap();
            assert!(
                tr.edges.iter().any(|e| e.rel_type == rel),
                "get_neighbors missed {rel} edge for {kind}"
            );
            assert!(tr.nodes.len() >= 2, "expected center + neighbor for {kind}");
        }

        // Unknown kind is kept as a generic predicate edge, not dropped.
        let f = LocalFact {
            subject: "X material".into(),
            predicate: "related_to".into(),
            object: "Y material".into(),
            value: None,
            unit: None,
            confidence: None,
            kind: None,
        };
        store.write_fact(&f, &prov).await.unwrap();
        let tr = store.get_neighbors("X material", None, "t1", 10).await.unwrap();
        assert!(tr.edges.iter().any(|e| e.rel_type == "related_to"));

        // Tenant scoping: nothing leaks into another tenant.
        assert!(store.graph_search("Ti", "other", 10).await.unwrap().is_empty());
        assert!(store
            .get_neighbors("Ti-6Al-4V", None, "other", 10)
            .await
            .unwrap()
            .edges
            .is_empty());
    }

    #[tokio::test]
    async fn same_name_under_two_labels_keeps_two_entities() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        // "alpha" as a Phase (object) and as Matter (subject) — with
        // unqualified keys these collapsed into one label-churning row.
        store
            .write_fact(&fact("phase", "Ti-6Al-4V", "has_phase", "alpha"), &prov)
            .await
            .unwrap();
        store
            .write_fact(&fact("phase", "alpha", "has_phase", "beta"), &prov)
            .await
            .unwrap();

        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM emmo_entity WHERE name = 'alpha'").await,
            2
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM emmo_entity WHERE key = 'Matter:alpha'").await,
            1
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM emmo_entity WHERE key = 'Phase:alpha'").await,
            1
        );

        // Traversal from the shared name sees edges of BOTH labels.
        let tr = store.get_neighbors("alpha", None, "t1", 10).await.unwrap();
        assert_eq!(tr.edges.len(), 2, "expected one edge per label: {:?}", tr.edges);
        assert!(tr.edges.iter().any(|e| e.source == "Ti-6Al-4V" && e.target == "alpha"));
        assert!(tr.edges.iter().any(|e| e.source == "alpha" && e.target == "beta"));
    }

    #[tokio::test]
    async fn contains_kind_writes_element_and_fraction_edge_props() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        let mut f = fact("contains", "Nb25Mo25Ta25W25", "contains", "Nb");
        f.value = Some(0.25);
        store.write_fact(&f, &prov).await.unwrap();

        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM emmo_entity WHERE key = 'Element:nb'").await,
            1
        );
        let props = query_str(
            &store,
            "SELECT props_json FROM emmo_edge WHERE rel_type = 'CONTAINS_ELEMENT'",
        )
        .await;
        let props: serde_json::Value = serde_json::from_str(&props).unwrap();
        assert_eq!(props["fraction"].as_f64(), Some(0.25));

        // Re-upsert WITHOUT a fraction must keep the stored props (COALESCE).
        f.value = None;
        store.write_fact(&f, &prov).await.unwrap();
        let props = query_str(
            &store,
            "SELECT props_json FROM emmo_edge WHERE rel_type = 'CONTAINS_ELEMENT'",
        )
        .await;
        let props: serde_json::Value = serde_json::from_str(&props).unwrap();
        assert_eq!(props["fraction"].as_f64(), Some(0.25));
    }

    #[tokio::test]
    async fn processing_order_lands_in_edge_props() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        let mut f = fact("processing", "Inconel 718", "processed_by", "annealing");
        f.value = Some(2.0);
        store.write_fact(&f, &prov).await.unwrap();

        let props = query_str(
            &store,
            "SELECT props_json FROM emmo_edge WHERE rel_type = 'PROCESSED_BY'",
        )
        .await;
        let props: serde_json::Value = serde_json::from_str(&props).unwrap();
        assert_eq!(props["order"].as_f64(), Some(2.0));
    }

    #[tokio::test]
    async fn write_fact_upserts_are_idempotent() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();
        let f = fact("phase", "Ti-6Al-4V", "has_phase", "alpha-beta");

        store.write_fact(&f, &prov).await.unwrap();
        store.write_fact(&f, &prov).await.unwrap();

        // Re-ingest never duplicates: 2 entities (Matter + Phase), 1 edge,
        // 1 assertion (corroborated), 1 agent, 1 activity.
        assert_eq!(count(&store, "SELECT COUNT(*) FROM emmo_entity").await, 2);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM emmo_edge").await, 1);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM prov_assertion").await, 1);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM prov_agent").await, 1);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM prov_activity").await, 1);
    }

    #[tokio::test]
    async fn same_triple_twice_corroborates_one_assertion() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();
        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };

        store.record_assertion(&a, &prov).await.unwrap();
        store.record_assertion(&a, &prov).await.unwrap();

        assert_eq!(count(&store, "SELECT COUNT(*) FROM prov_assertion").await, 1);
        assert_eq!(
            count(&store, "SELECT corroborations FROM prov_assertion").await,
            2
        );

        // Noisy-OR: 1 - (1-0.8)*(1-0.8) = 0.96 — combined and strictly higher.
        let facts = store.recall("alpha-beta", "t1", 10).await.unwrap();
        assert_eq!(facts.len(), 1);
        assert!((facts[0].confidence - 0.96).abs() < 1e-9);
        assert_eq!(facts[0].source, "doc:test_paper");
        assert_eq!(facts[0].agent, "gemma-4-12b");

        // recall is ordered by confidence DESC.
        let weak = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "beta".into(),
            confidence: Some(0.3),
        };
        store.record_assertion(&weak, &prov).await.unwrap();
        let facts = store.recall("Ti-6Al-4V", "t1", 10).await.unwrap();
        assert_eq!(facts.len(), 2);
        assert!(facts[0].confidence >= facts[1].confidence);

        // Tenant scoping on recall.
        assert!(store.recall("alpha-beta", "other", 10).await.unwrap().is_empty());
    }

    #[test]
    fn canonical_key_normalizes() {
        assert_eq!(canonical_key("  Ti-6Al-4V  "), "ti-6al-4v");
        assert_eq!(canonical_key("Yield   Strength"), "yield strength");
        assert_eq!(canonical_key("YIELD\tstrength"), canonical_key("yield STRENGTH "));
        assert_ne!(canonical_key("yield strength"), canonical_key("tensile strength"));
    }

    #[test]
    fn assertion_id_is_stable_and_canonical() {
        let a = assertion_id("Ti-6Al-4V", "has_phase", "alpha-beta");
        let b = assertion_id("  ti-6al-4v ", "has_phase", "ALPHA-BETA");
        let c = assertion_id("alpha-beta", "has_phase", "Ti-6Al-4V");
        assert_eq!(a, b, "spelling variants must corroborate one assertion");
        assert_ne!(a, c, "direction matters");
        assert_eq!(a.len(), 64);
    }
}
