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

use crate::{ProvenanceStore, get_str};

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

/// A QUDT unit identifier such as `QUDT:K` or `QUDT:W-PER-M-K`.
///
/// This is deliberately an identifier newtype, not a PRISM-specific unit
/// enum: QUDT is the vocabulary, and accepting its open identifier space
/// avoids creating a second, inevitably incomplete unit taxonomy here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct QudtUnit(String);

impl QudtUnit {
    pub fn new(identifier: impl Into<String>) -> Result<Self> {
        let identifier = identifier.into();
        if !identifier.starts_with("QUDT:") || identifier.len() == "QUDT:".len() {
            anyhow::bail!("unit must be a QUDT identifier such as QUDT:K");
        }
        Ok(Self(identifier))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for QudtUnit {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let identifier = String::deserialize(deserializer)?;
        Self::new(identifier).map_err(serde::de::Error::custom)
    }
}

/// A numerical or categorical boundary-condition value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConditionValue {
    Number(f64),
    Text(String),
}

/// One solver-consumable measurement condition. Numerical conditions carry
/// a QUDT unit; categorical conditions (for example atmosphere=`air`) carry
/// `unit: null` rather than smuggling the condition into prose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeasurementCondition {
    pub name: String,
    pub value: ConditionValue,
    #[serde(default)]
    pub unit: Option<QudtUnit>,
}

/// The shared four-level evidence vocabulary, aligned with RHEA-JAX
/// `ClaimStatus`. Colors are presentation labels; these serialized values are
/// the stable machine contract used by facts and computed results.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    /// RED: model assertion with no grounding.
    #[default]
    Indeterminate,
    /// ORANGE: extracted from literature, not independently verified.
    Research,
    /// YELLOW: computed by a cited method.
    Screening,
    /// GREEN: executed or measured with reference evidence.
    ReferenceValidated,
}

impl EvidenceClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Indeterminate => "indeterminate",
            Self::Research => "research",
            Self::Screening => "screening",
            Self::ReferenceValidated => "reference_validated",
        }
    }

    #[must_use]
    pub fn color(self) -> &'static str {
        match self {
            Self::Indeterminate => "red",
            Self::Research => "orange",
            Self::Screening => "yellow",
            Self::ReferenceValidated => "green",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Indeterminate => 0,
            Self::Research => 1,
            Self::Screening => 2,
            Self::ReferenceValidated => 3,
        }
    }

    fn from_stored(value: &str) -> Self {
        match value {
            "research" => Self::Research,
            "screening" => Self::Screening,
            "reference_validated" => Self::ReferenceValidated,
            _ => Self::Indeterminate,
        }
    }
}

/// How a result was produced. This sets the best class the producer is
/// allowed to claim before input evidence is considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceSource {
    Execution,
    CitedComputation,
    LiteratureExtraction,
    ModelAssertion,
}

/// Classify a result conservatively: only execution has a GREEN ceiling, and
/// the result can never outrank its worst input.
#[must_use]
pub fn evidence_for_result(
    source: EvidenceSource,
    inputs: impl IntoIterator<Item = EvidenceClass>,
) -> EvidenceClass {
    let ceiling = match source {
        EvidenceSource::Execution => EvidenceClass::ReferenceValidated,
        EvidenceSource::CitedComputation => EvidenceClass::Screening,
        EvidenceSource::LiteratureExtraction => EvidenceClass::Research,
        EvidenceSource::ModelAssertion => EvidenceClass::Indeterminate,
    };
    inputs.into_iter().fold(ceiling, |worst, input| {
        if input.rank() < worst.rank() {
            input
        } else {
            worst
        }
    })
}

/// New extraction/storage contract. The legacy [`LocalFact`] remains source
/// compatible for CLI/server/mesh callers, while all new text extraction uses
/// this type so conditions and QUDT units cannot be omitted from the path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub unit: Option<QudtUnit>,
    #[serde(default)]
    pub conditions: Vec<MeasurementCondition>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub evidence_class: EvidenceClass,
}

/// Common storage view implemented by both the additive conditioned contract
/// and the source-compatible legacy fact.
pub trait FactPayload {
    fn to_local_fact(&self) -> LocalFact;
    fn conditions(&self) -> &[MeasurementCondition];
    fn evidence_class(&self) -> EvidenceClass;
}

impl FactPayload for LocalFact {
    fn to_local_fact(&self) -> LocalFact {
        self.clone()
    }

    fn conditions(&self) -> &[MeasurementCondition] {
        &[]
    }

    fn evidence_class(&self) -> EvidenceClass {
        EvidenceClass::Indeterminate
    }
}

fn validate_conditions(conditions: &[MeasurementCondition]) -> Result<()> {
    for condition in conditions {
        if condition.name.trim().is_empty() {
            anyhow::bail!("measurement condition name cannot be empty");
        }
        if matches!(&condition.value, ConditionValue::Number(_)) && condition.unit.is_none() {
            anyhow::bail!(
                "numerical measurement condition '{}' requires a QUDT unit",
                condition.name
            );
        }
    }
    Ok(())
}

impl FactPayload for MaterialFact {
    fn to_local_fact(&self) -> LocalFact {
        LocalFact {
            subject: self.subject.clone(),
            predicate: self.predicate.clone(),
            object: self.object.clone(),
            value: self.value,
            unit: self.unit.as_ref().map(|unit| unit.as_str().to_string()),
            confidence: self.confidence,
            kind: self.kind.clone(),
        }
    }

    fn conditions(&self) -> &[MeasurementCondition] {
        &self.conditions
    }

    fn evidence_class(&self) -> EvidenceClass {
        self.evidence_class
    }
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

/// Additive read shape for conditioned, evidence-classed facts. The legacy
/// [`RecalledFact`] remains unchanged so external struct literals and old
/// consumers continue to compile; new scientific reads use this complete
/// shape and therefore never render a fact without its class.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RecalledMaterialFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub conditions: Vec<MeasurementCondition>,
    pub evidence_class: EvidenceClass,
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

/// Tenant- and label-qualified entity key
/// ("{tenant}|{label}:{canonical name}").
///
/// Qualifying by label keeps one node per (label, name) — the same name
/// extracted as e.g. both a Phase and a Matter stays two nodes instead of
/// one label-churning row (mirrors core, which keeps a node per label).
///
/// Qualifying by TENANT is what keeps tenants from destroying each other.
/// Every read filters `WHERE tenant = ?`, and `upsert_entity` merges on
/// this key, so a tenant-blind key meant whichever tenant wrote last owned
/// the row and the other one's entity silently disappeared from its own
/// view. `upsert_edge` has always qualified its id by tenant; entities
/// were the outlier.
fn entity_key(tenant: &str, label: &str, name: &str) -> String {
    format!("{tenant}|{label}:{}", canonical_key(name))
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

fn conditioned_assertion_id(
    subject: &str,
    predicate: &str,
    object: &str,
    value: Option<f64>,
    unit: Option<&str>,
    conditions: &[MeasurementCondition],
) -> Result<String> {
    if value.is_none() && unit.is_none() && conditions.is_empty() {
        return Ok(assertion_id(subject, predicate, object));
    }

    use sha2::{Digest, Sha256};
    let mut canonical_conditions = conditions.to_vec();
    canonical_conditions.sort_by(|left, right| left.name.cmp(&right.name));
    let mut h = Sha256::new();
    h.update(canonical_key(subject).as_bytes());
    h.update(b"|");
    h.update(predicate.as_bytes());
    h.update(b"|");
    h.update(canonical_key(object).as_bytes());
    h.update(b"|");
    if let Some(value) = value {
        h.update(value.to_bits().to_le_bytes());
    }
    h.update(b"|");
    if let Some(unit) = unit {
        h.update(unit.as_bytes());
    }
    h.update(b"|");
    h.update(serde_json::to_vec(&canonical_conditions)?);
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
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
            value REAL,
            unit TEXT,
            conditions_json TEXT NOT NULL DEFAULT '[]',
            evidence_class TEXT NOT NULL DEFAULT 'indeterminate',
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

    // Additive migration for databases created before conditioned facts and
    // evidence classes existed. Defaults keep every legacy row readable and
    // conservatively RED; no old value is rewritten or dropped.
    crate::add_column_if_absent(conn, "prov_assertion", "value", "REAL").await?;
    crate::add_column_if_absent(conn, "prov_assertion", "unit", "TEXT").await?;
    crate::add_column_if_absent(
        conn,
        "prov_assertion",
        "conditions_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )
    .await?;
    crate::add_column_if_absent(
        conn,
        "prov_assertion",
        "evidence_class",
        "TEXT NOT NULL DEFAULT 'indeterminate'",
    )
    .await?;

    // Entity vectors for local semantic search: one little-endian f32 blob
    // per emmo_entity key (same encoding as `provenance_embeddings`),
    // written lazily by `embed_and_store_entities` — never on the
    // `write_fact` path. Turso-side counterpart of the Qdrant collection so
    // a local ingest is semantically searchable without any services.
    //
    // `vector` is a plain BLOB on purpose, and it is already Turso's native
    // vector wire format: the engine reads the vector type off the blob
    // ("even-sized blobs are always float32"), not off the column
    // declaration, so `vector_distance_cos(vector, ?)` scores these rows
    // directly. Declaring `F32_BLOB(384)` instead would buy nothing —
    // Turso 0.7 attaches no meaning to it — while baking one embedding
    // model's dimensionality into the schema, which is exactly the thing
    // `semantic_search_entities` has to stay honest about when the backend
    // changes. There is likewise no vector index: `libsql_vector_idx` does
    // not exist in this engine, whose only index method is an experimental
    // sparse-only one, so ranking is a scan — correct, and fine at
    // local-ingest scale.
    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS emmo_embedding (
            key TEXT PRIMARY KEY,
            tenant TEXT,
            dim INTEGER,
            vector BLOB
        )"#,
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_embedding_tenant ON emmo_embedding(tenant)",
        (),
    )
    .await?;

    migrate_keys_to_tenant_qualified(conn).await?;

    Ok(())
}

/// Rewrite pre-existing `{label}:{name}` keys to `{tenant}|{label}:{name}`.
///
/// Entity keys used to omit the tenant, which let one tenant's write take
/// ownership of another's row. Now that the tenant is part of the key, a
/// legacy database would keep its old rows under the old keys: the next
/// re-ingest would write a SECOND row for the same entity, and edges would
/// split across the two key spaces. Rewriting them keeps one row per
/// (tenant, label, name) across the change.
///
/// Idempotent: keys already containing `|` are left alone, so reopening a
/// migrated database is a no-op. Rows whose tenant is NULL/empty are also
/// left alone — there is no tenant to qualify them with, and inventing one
/// would be a worse guess than leaving them where the old readers expect.
async fn migrate_keys_to_tenant_qualified(conn: &turso::Connection) -> Result<()> {
    // `instr(key, '|') = 0` ⇒ not yet qualified. Entities and vectors
    // first, then the edge endpoints that reference them.
    for sql in [
        "UPDATE emmo_entity SET key = tenant || '|' || key
           WHERE instr(key, '|') = 0 AND tenant IS NOT NULL AND tenant <> ''",
        "UPDATE emmo_embedding SET key = tenant || '|' || key
           WHERE instr(key, '|') = 0 AND tenant IS NOT NULL AND tenant <> ''",
        "UPDATE emmo_edge SET source_key = tenant || '|' || source_key
           WHERE instr(source_key, '|') = 0 AND tenant IS NOT NULL AND tenant <> ''",
        "UPDATE emmo_edge SET target_key = tenant || '|' || target_key
           WHERE instr(target_key, '|') = 0 AND tenant IS NOT NULL AND tenant <> ''",
        // `emmo_edge.id` is derived from (tenant, source_key, rel_type,
        // target_key), so rewriting the endpoints invalidates it — the
        // next `upsert_edge` would compute a different id and insert a
        // duplicate. Recompute it from its components, which is exactly
        // what `upsert_edge` does and is therefore idempotent.
        "UPDATE emmo_edge
            SET id = tenant || '|' || source_key || '|' || rel_type || '|' || target_key
          WHERE tenant IS NOT NULL AND tenant <> ''",
    ] {
        conn.execute(sql, ())
            .await
            .map_err(|e| anyhow::anyhow!(e).context("tenant-qualified key migration failed"))?;
    }
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
        let key = entity_key(tenant, label, name);
        // `tenant` is deliberately NOT in the DO UPDATE set: the key now
        // carries it, so a conflict can only ever be the same tenant
        // re-ingesting. Reassigning it here is what let one tenant take
        // ownership of another's row.
        self.conn
            .execute(
                r#"INSERT INTO emmo_entity
                   (key, name, label, entity_type, tenant, props_json, created_at)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                   ON CONFLICT(key) DO UPDATE SET
                       name = excluded.name,
                       label = excluded.label,
                       entity_type = excluded.entity_type,
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
    pub async fn write_fact<F: FactPayload>(&self, fact: &F, prov: &LocalProvenance) -> Result<()> {
        self.write_fact_as(fact, prov, fact.evidence_class()).await
    }

    /// Store a source-compatible legacy fact with an explicit class. This is
    /// used by the tabular LLM ingest path, whose old `LocalFact` shape cannot
    /// carry the new field but whose origin is known to be literature/data
    /// extraction (ORANGE), not an ungrounded model assertion (RED).
    pub async fn write_fact_with_evidence(
        &self,
        fact: &LocalFact,
        prov: &LocalProvenance,
        evidence_class: EvidenceClass,
    ) -> Result<()> {
        self.write_fact_as(fact, prov, evidence_class).await
    }

    async fn write_fact_as<F: FactPayload>(
        &self,
        payload: &F,
        prov: &LocalProvenance,
        evidence_class: EvidenceClass,
    ) -> Result<()> {
        let conditions = payload.conditions().to_vec();
        validate_conditions(&conditions)?;
        let fact = payload.to_local_fact();
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
                    "value": value,
                    "unit": unit,
                    "conditions": conditions,
                    "evidence_class": evidence_class,
                    "evidence_color": evidence_class.color(),
                    "confidence": confidence,
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
                self.upsert_edge(
                    &subj_key,
                    &meas_key,
                    "HAS_MEASUREMENT",
                    &fact.predicate,
                    confidence,
                    tenant,
                    None,
                )
                .await?;
                self.upsert_edge(
                    &meas_key,
                    &obj_key,
                    "OF_PROPERTY",
                    &fact.predicate,
                    confidence,
                    tenant,
                    None,
                )
                .await?;
            }
            Some("phase") => {
                let subj_key = self
                    .upsert_entity(&fact.subject, "Matter", tenant, None)
                    .await?;
                let obj_key = self
                    .upsert_entity(&fact.object, "Phase", tenant, None)
                    .await?;
                self.upsert_edge(
                    &subj_key,
                    &obj_key,
                    "HAS_PHASE",
                    &fact.predicate,
                    confidence,
                    tenant,
                    None,
                )
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
                self.upsert_edge(
                    &subj_key,
                    &obj_key,
                    "HAS_COMPOSITION",
                    &fact.predicate,
                    confidence,
                    tenant,
                    None,
                )
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
                self.upsert_edge(
                    &subj_key,
                    &obj_key,
                    "CONTAINS_ELEMENT",
                    &fact.predicate,
                    confidence,
                    tenant,
                    props.as_deref(),
                )
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
                self.upsert_edge(
                    &subj_key,
                    &obj_key,
                    "PROCESSED_BY",
                    &fact.predicate,
                    confidence,
                    tenant,
                    props.as_deref(),
                )
                .await?;
            }
            Some("structure") => {
                let props = serde_json::json!({ "system": &fact.object });
                let subj_key = self
                    .upsert_entity(&fact.subject, "Matter", tenant, None)
                    .await?;
                let obj_key = self
                    .upsert_entity(
                        &fact.object,
                        "CrystalStructure",
                        tenant,
                        Some(props.to_string()),
                    )
                    .await?;
                self.upsert_edge(
                    &subj_key,
                    &obj_key,
                    "HAS_STRUCTURE",
                    &fact.predicate,
                    confidence,
                    tenant,
                    None,
                )
                .await?;
            }
            Some("application") => {
                let subj_key = self
                    .upsert_entity(&fact.subject, "Matter", tenant, None)
                    .await?;
                let obj_key = self
                    .upsert_entity(&fact.object, "Application", tenant, None)
                    .await?;
                self.upsert_edge(
                    &subj_key,
                    &obj_key,
                    "USED_IN",
                    &fact.predicate,
                    confidence,
                    tenant,
                    None,
                )
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
                self.upsert_edge(
                    &subj_key,
                    &obj_key,
                    &fact.predicate,
                    &fact.predicate,
                    confidence,
                    tenant,
                    None,
                )
                .await?;
            }
        }

        self.record_assertion_with_context(
            &LocalAssertion {
                subject: fact.subject.clone(),
                predicate: fact.predicate.clone(),
                object: fact.object.clone(),
                confidence: fact.confidence,
            },
            prov,
            fact.value,
            fact.unit.as_deref(),
            &conditions,
            evidence_class,
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
    pub async fn record_assertion(&self, a: &LocalAssertion, prov: &LocalProvenance) -> Result<()> {
        self.record_assertion_with_context(a, prov, None, None, &[], EvidenceClass::Indeterminate)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_assertion_with_context(
        &self,
        a: &LocalAssertion,
        prov: &LocalProvenance,
        value: Option<f64>,
        unit: Option<&str>,
        conditions: &[MeasurementCondition],
        evidence_class: EvidenceClass,
    ) -> Result<()> {
        self.record_activity(prov).await?;

        let id =
            conditioned_assertion_id(&a.subject, &a.predicate, &a.object, value, unit, conditions)?;
        let confidence_evidence = a.confidence.unwrap_or(1.0);
        let conditions_json = serde_json::to_string(conditions)?;

        // Read current belief, then corroborate or insert. Confidence and
        // evidence class are independent: noisy-OR may increase confidence,
        // but the stored class remains the WORST class ever attached to this
        // assertion. Agreement therefore cannot turn literature into GREEN.
        // The cursor is fully consumed before the write (Turso pre-release is
        // sensitive to interleaved statements on one connection).
        let existing = {
            let mut rows = self
                .conn
                .query(
                    "SELECT confidence, corroborations, evidence_class \
                     FROM prov_assertion WHERE id = ?1",
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
                    let old_class = match row.get_value(2).ok() {
                        Some(Value::Text(value)) => EvidenceClass::from_stored(&value),
                        _ => EvidenceClass::Indeterminate,
                    };
                    while rows.next().await?.is_some() {}
                    Some((old_conf, old_corr, old_class))
                }
                None => None,
            }
        };
        if let Some((old_conf, old_corr, old_class)) = existing {
            let retained_class =
                evidence_for_result(EvidenceSource::Execution, [old_class, evidence_class]);
            self.conn
                .execute(
                    r#"UPDATE prov_assertion
                       SET confidence = ?1, corroborations = ?2,
                           activity_id = ?3, source = ?4, agent = ?5,
                           value = ?6, unit = ?7, conditions_json = ?8,
                           evidence_class = ?9
                       WHERE id = ?10"#,
                    [
                        Value::Real(corroborate_confidence(old_conf, confidence_evidence)),
                        Value::Integer(old_corr + 1),
                        Value::Text(prov.activity_id.clone()),
                        Value::Text(prov.source_entity_id.clone()),
                        Value::Text(prov.agent_id.clone()),
                        value.map_or(Value::Null, Value::Real),
                        unit.map_or(Value::Null, |unit| Value::Text(unit.to_string())),
                        Value::Text(conditions_json),
                        Value::Text(retained_class.as_str().to_string()),
                        Value::Text(id),
                    ],
                )
                .await?;
        } else {
            self.conn
                .execute(
                    r#"INSERT INTO prov_assertion
                       (id, subject, predicate, object, value, unit,
                        conditions_json, evidence_class, confidence, corroborations,
                        activity_id, source, agent, tenant)
                       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                               ?11, ?12, ?13, ?14)"#,
                    [
                        Value::Text(id),
                        Value::Text(a.subject.clone()),
                        Value::Text(a.predicate.clone()),
                        Value::Text(a.object.clone()),
                        value.map_or(Value::Null, Value::Real),
                        unit.map_or(Value::Null, |unit| Value::Text(unit.to_string())),
                        Value::Text(conditions_json),
                        Value::Text(evidence_class.as_str().to_string()),
                        Value::Real(confidence_evidence),
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

    /// Legacy cloud-shaped recall. New scientific consumers should use
    /// [`Self::recall_with_context`], which also returns value, unit,
    /// conditions, and evidence class.
    pub async fn recall(&self, query: &str, tenant: &str, limit: i64) -> Result<Vec<RecalledFact>> {
        Ok(self
            .recall_with_context(query, tenant, limit)
            .await?
            .into_iter()
            .map(|fact| RecalledFact {
                subject: fact.subject,
                predicate: fact.predicate,
                object: fact.object,
                confidence: fact.confidence,
                source: fact.source,
                agent: fact.agent,
            })
            .collect())
    }

    /// Recall complete assertions, highest-confidence first. Every returned
    /// row includes the additive condition and evidence fields; legacy rows
    /// read as empty conditions with RED/indeterminate evidence.
    pub async fn recall_with_context(
        &self,
        query: &str,
        tenant: &str,
        limit: i64,
    ) -> Result<Vec<RecalledMaterialFact>> {
        let pattern = format!("%{query}%");
        let mut rows = self
            .conn
            .query(
                r#"SELECT subject, predicate, object, value, unit, conditions_json,
                          evidence_class, confidence, source, agent
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
            let conditions_json = get_str(&row, 5)?;
            let conditions = serde_json::from_str(&conditions_json).map_err(|error| {
                anyhow::anyhow!("stored fact has invalid conditions_json: {error}")
            })?;
            facts.push(RecalledMaterialFact {
                subject: get_str(&row, 0)?,
                predicate: get_str(&row, 1)?,
                object: get_str(&row, 2)?,
                value: row
                    .get_value(3)
                    .ok()
                    .and_then(|value| value.as_real().copied()),
                unit: match row.get_value(4)? {
                    Value::Text(unit) if !unit.is_empty() => Some(unit),
                    _ => None,
                },
                conditions,
                evidence_class: EvidenceClass::from_stored(&get_str(&row, 6)?),
                confidence: row
                    .get_value(7)
                    .ok()
                    .and_then(|value| value.as_real().copied())
                    .unwrap_or(0.0),
                source: get_str(&row, 8)?,
                agent: get_str(&row, 9)?,
            });
        }
        Ok(facts)
    }

    // ─────────────────────────────────────────────────────────────────────
    // Entity vectors — local semantic search without Qdrant
    // ─────────────────────────────────────────────────────────────────────

    /// UPSERT one vector for a label-qualified entity key (little-endian
    /// f32 blob, same encoding as `provenance_embeddings`).
    pub async fn store_entity_embedding(
        &self,
        key: &str,
        tenant: &str,
        vector: &[f32],
    ) -> Result<()> {
        self.conn
            .execute(
                r#"INSERT OR REPLACE INTO emmo_embedding
                   (key, tenant, dim, vector) VALUES (?1, ?2, ?3, ?4)"#,
                [
                    Value::Text(key.to_string()),
                    Value::Text(tenant.to_string()),
                    Value::Integer(vector.len() as i64),
                    Value::Blob(prism_embed::vec_to_le_bytes(vector)),
                ],
            )
            .await?;
        Ok(())
    }

    /// Embed the distinct subject/object names of `facts` with `backend`
    /// and store one vector per matching `emmo_entity` row. Names are
    /// resolved to their label-qualified keys via the entity table itself
    /// (no duplicate of `write_fact`'s kind→label routing), so names that
    /// never landed there (e.g. value-less measurements dropped by
    /// `write_fact`) are skipped. Returns the number of vectors stored.
    ///
    /// Like `embed_and_store`, deliberately NOT part of `write_fact`:
    /// graph writes must never wait on (or fail because of) an embedding
    /// model. Callers run this after the fact writes succeed.
    pub async fn embed_and_store_entities<F: FactPayload>(
        &self,
        facts: &[F],
        tenant: &str,
        backend: &dyn prism_embed::EmbedBackend,
    ) -> Result<usize> {
        // Distinct display names, first-seen order.
        let mut seen = std::collections::HashSet::new();
        let mut names: Vec<String> = Vec::new();
        for payload in facts {
            let fact = payload.to_local_fact();
            for name in [fact.subject, fact.object] {
                if seen.insert(canonical_key(&name)) {
                    names.push(name);
                }
            }
        }
        if names.is_empty() {
            return Ok(0);
        }
        let vectors = backend.embed(&names).await?;

        let mut stored = 0usize;
        for (name, vector) in names.iter().zip(&vectors) {
            // The read cursor is fully drained BEFORE the writes below
            // (turso pre-release mishandles interleaved open statements —
            // see `record_assertion`).
            let keys = {
                let mut rows = self
                    .conn
                    .query(
                        "SELECT key FROM emmo_entity WHERE tenant = ?1 AND name = ?2",
                        [Value::Text(tenant.to_string()), Value::Text(name.clone())],
                    )
                    .await?;
                let mut keys = Vec::new();
                while let Some(row) = rows.next().await? {
                    keys.push(get_str(&row, 0)?);
                }
                keys
            };
            for key in keys {
                self.store_entity_embedding(&key, tenant, vector).await?;
                stored += 1;
            }
        }
        Ok(stored)
    }

    /// Best-effort entity embedding for freshly written facts: builds the
    /// configured `prism-embed` backend (on the blocking pool — the first
    /// ever native init downloads the model) and stores one vector per
    /// entity. Failures are logged and swallowed — an ingest must never
    /// fail because of the embedding model.
    pub async fn embed_entities_best_effort<F: FactPayload>(&self, facts: &[F], tenant: &str) {
        if facts.is_empty() {
            return;
        }
        let backend = match tokio::task::spawn_blocking(prism_embed::from_config).await {
            Ok(Some(backend)) => backend,
            Ok(None) => {
                tracing::debug!("embedding backend unavailable — entity vectors skipped");
                return;
            }
            Err(e) => {
                tracing::warn!("embedding backend init failed: {e} — entity vectors skipped");
                return;
            }
        };
        match self
            .embed_and_store_entities(facts, tenant, backend.as_ref())
            .await
        {
            Ok(stored) => tracing::debug!(stored, tenant, "entity vectors stored in Turso"),
            Err(e) => tracing::warn!("entity embedding failed: {e:#} — graph write unaffected"),
        }
    }

    /// Number of stored entity vectors for `tenant` — cheap existence
    /// check so query paths can skip embedding-model init (and fall back
    /// to other stores) when there is nothing to search.
    pub async fn entity_embedding_count(&self, tenant: &str) -> Result<i64> {
        let mut rows = self
            .conn
            .query(
                "SELECT COUNT(*) FROM emmo_embedding WHERE tenant = ?1",
                [Value::Text(tenant.to_string())],
            )
            .await?;
        Ok(match rows.next().await? {
            Some(row) => row
                .get_value(0)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0),
            None => 0,
        })
    }

    /// Distinct stored vector widths (in bytes) for `tenant`, read from the
    /// blobs themselves rather than the `dim` column, so a NULL or stale
    /// `dim` cannot misreport what the index actually holds.
    async fn entity_vector_widths(&self, tenant: &str) -> Result<Vec<usize>> {
        let mut rows = self
            .conn
            .query(
                "SELECT DISTINCT LENGTH(vector) FROM emmo_embedding WHERE tenant = ?1",
                [Value::Text(tenant.to_string())],
            )
            .await?;
        let mut widths = Vec::new();
        while let Some(row) = rows.next().await? {
            if let Some(bytes) = row.get_value(0)?.as_integer().copied() {
                widths.push(bytes.max(0) as usize);
            }
        }
        Ok(widths)
    }

    /// Semantic entity search ranked by Turso's **native** vector support:
    /// `vector_distance_cos()` scores the stored f32 blobs inside the
    /// database, and `GROUP BY` collapses the same display name under two
    /// labels to its best-scoring row. Returns up to `limit` distinct
    /// `(display name, similarity)` pairs, best first, similarities in
    /// `[-1, 1]`.
    ///
    /// # Honesty contract
    ///
    /// `Ok(vec![])` means exactly one thing: **nothing is embedded for this
    /// tenant**. It never means "the index is broken". Every unusable-index
    /// condition is an `Err` naming the problem — above all a dimension
    /// mismatch, which used to be skipped row-by-row and so was
    /// indistinguishable from "no matches".
    pub async fn semantic_search_entities(
        &self,
        query_vec: &[f32],
        tenant: &str,
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        let stored = self.entity_vector_widths(tenant).await?;
        if stored.is_empty() {
            return Ok(Vec::new()); // genuinely empty index — not a failure
        }
        // A mismatch silently matches nothing, so refuse loudly instead.
        // Checked up front so the message can name both dimensionalities;
        // Turso's own error ("Vectors must have the same dimensions")
        // names neither.
        let want = query_vec.len() * 4;
        if stored.iter().any(|w| *w != want) {
            let mut dims: Vec<usize> = stored.iter().map(|w| w / 4).collect();
            dims.sort_unstable();
            let dims: Vec<String> = dims.iter().map(usize::to_string).collect();
            anyhow::bail!(
                "local semantic index is unusable: tenant '{tenant}' holds {}-dimension \
                 vectors but the query embedding is {}-dimension. The embedding backend \
                 changed since those vectors were written — re-ingest with the current \
                 backend, or point PRISM_EMBED_BACKEND back at the one that wrote them.",
                dims.join("/"),
                query_vec.len(),
            );
        }

        let mut rows = self
            .conn
            .query(
                "SELECT n.name, MIN(vector_distance_cos(e.vector, ?2)) AS distance \
                 FROM emmo_embedding e JOIN emmo_entity n ON n.key = e.key \
                 WHERE e.tenant = ?1 \
                 GROUP BY n.name ORDER BY distance ASC LIMIT ?3",
                [
                    Value::Text(tenant.to_string()),
                    Value::Blob(prism_embed::vec_to_le_bytes(query_vec)),
                    Value::Integer(limit.max(1) as i64),
                ],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let name = get_str(&row, 0)?;
            // `vector_distance_cos` is `1 - cosine_similarity`, in [0, 2].
            let distance = match row.get_value(1)? {
                Value::Real(d) => d,
                Value::Integer(d) => d as f64,
                other => anyhow::bail!("vector_distance_cos returned {other:?}, expected a number"),
            };
            out.push((name, 1.0 - distance as f32));
        }
        Ok(out)
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

    /// Tempfile-backed Turso DB, removed (with SQLite journal sidecars) on drop.
    struct TempDb {
        path: PathBuf,
    }

    impl TempDb {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("prism_emmo_test_{}.db", uuid::Uuid::new_v4()));
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
            (
                "measurement",
                "Ti-6Al-4V",
                "has_measurement",
                "UTS",
                "HAS_MEASUREMENT",
            ),
            ("phase", "Ti-6Al-4V", "has_phase", "alpha-beta", "HAS_PHASE"),
            (
                "composition",
                "Inconel 718",
                "has_composition",
                "NiCr19Fe18",
                "HAS_COMPOSITION",
            ),
            (
                "contains",
                "Inconel 718",
                "contains",
                "Ni",
                "CONTAINS_ELEMENT",
            ),
            (
                "processing",
                "Inconel 718",
                "processed_by",
                "LPBF",
                "PROCESSED_BY",
            ),
            (
                "structure",
                "Ti-6Al-4V",
                "has_structure",
                "hexagonal",
                "HAS_STRUCTURE",
            ),
            (
                "application",
                "Ti-6Al-4V",
                "used_in",
                "turbine blades",
                "USED_IN",
            ),
        ];
        for (kind, s, p, o, rel) in cases {
            let mut f = fact(kind, s, p, o);
            if kind == "measurement" {
                f.value = Some(1140.0);
                f.unit = Some("MPa".into());
            }
            store.write_fact(&f, &prov).await.unwrap();

            let hits = store.graph_search(s, "t1", 10).await.unwrap();
            assert!(
                hits.iter().any(|n| n.name == s),
                "graph_search missed subject for {kind}"
            );

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
        let tr = store
            .get_neighbors("X material", None, "t1", 10)
            .await
            .unwrap();
        assert!(tr.edges.iter().any(|e| e.rel_type == "related_to"));

        // Tenant scoping: nothing leaks into another tenant.
        assert!(
            store
                .graph_search("Ti", "other", 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .get_neighbors("Ti-6Al-4V", None, "other", 10)
                .await
                .unwrap()
                .edges
                .is_empty()
        );
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
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE name = 'alpha'"
            )
            .await,
            2
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE key = 't1|Matter:alpha'"
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE key = 't1|Phase:alpha'"
            )
            .await,
            1
        );

        // Traversal from the shared name sees edges of BOTH labels.
        let tr = store.get_neighbors("alpha", None, "t1", 10).await.unwrap();
        assert_eq!(
            tr.edges.len(),
            2,
            "expected one edge per label: {:?}",
            tr.edges
        );
        assert!(
            tr.edges
                .iter()
                .any(|e| e.source == "Ti-6Al-4V" && e.target == "alpha")
        );
        assert!(
            tr.edges
                .iter()
                .any(|e| e.source == "alpha" && e.target == "beta")
        );
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
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE key = 't1|Element:nb'"
            )
            .await,
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
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion").await,
            1
        );
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

        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion").await,
            1
        );
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
        assert!(
            store
                .recall("alpha-beta", "other", 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn canonical_key_normalizes() {
        assert_eq!(canonical_key("  Ti-6Al-4V  "), "ti-6al-4v");
        assert_eq!(canonical_key("Yield   Strength"), "yield strength");
        assert_eq!(
            canonical_key("YIELD\tstrength"),
            canonical_key("yield STRENGTH ")
        );
        assert_ne!(
            canonical_key("yield strength"),
            canonical_key("tensile strength")
        );
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

    #[test]
    fn computed_evidence_inherits_the_worst_input() {
        assert_eq!(
            evidence_for_result(
                EvidenceSource::Execution,
                [EvidenceClass::ReferenceValidated, EvidenceClass::Research],
            ),
            EvidenceClass::Research,
            "executing a solver must not launder an orange boundary condition",
        );
        assert_eq!(
            evidence_for_result(
                EvidenceSource::CitedComputation,
                [EvidenceClass::ReferenceValidated],
            ),
            EvidenceClass::Screening,
            "a cited computation is yellow even with green inputs",
        );
        assert_eq!(
            evidence_for_result(EvidenceSource::LiteratureExtraction, []),
            EvidenceClass::Research,
            "literature extraction is orange regardless of confidence",
        );
    }

    #[tokio::test]
    async fn legacy_assertion_rows_migrate_to_empty_conditions_and_red() {
        let db = TempDb::new();
        {
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            conn.execute(
                r#"CREATE TABLE prov_assertion (
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
            .await
            .unwrap();
            conn.execute(
                r#"INSERT INTO prov_assertion
                   (id, subject, predicate, object, confidence, corroborations,
                    activity_id, source, agent, tenant)
                   VALUES ('legacy', 'steel', 'has_phase', 'bcc', 0.7, 1,
                           'activity', 'legacy.csv', 'legacy-agent', 't1')"#,
                (),
            )
            .await
            .unwrap();
        }

        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let recalled = store.recall_with_context("steel", "t1", 10).await.unwrap();
        assert_eq!(recalled.len(), 1);
        assert_eq!(recalled[0].value, None);
        assert_eq!(recalled[0].unit, None);
        assert!(recalled[0].conditions.is_empty());
        assert_eq!(recalled[0].evidence_class, EvidenceClass::Indeterminate);
    }

    #[tokio::test]
    async fn conditions_distinguish_measurements_and_corroboration_cannot_upgrade_them() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();
        let measurement = |temperature, evidence_class| MaterialFact {
            subject: "test ceramic".into(),
            predicate: "has_measurement".into(),
            object: "thermal conductivity".into(),
            value: Some(22.0),
            unit: Some(QudtUnit::new("QUDT:W-PER-M-K").unwrap()),
            conditions: vec![MeasurementCondition {
                name: "temperature".into(),
                value: ConditionValue::Number(temperature),
                unit: Some(QudtUnit::new("QUDT:K").unwrap()),
            }],
            confidence: Some(0.9),
            kind: Some("measurement".into()),
            evidence_class,
        };

        let at_1200 = measurement(1200.0, EvidenceClass::Research);
        let at_1300 = measurement(1300.0, EvidenceClass::Research);
        store.write_fact(&at_1200, &prov).await.unwrap();
        store.write_fact(&at_1300, &prov).await.unwrap();
        // A later execution that agrees with the 1200 K value raises
        // confidence but must not upgrade the literature-derived class.
        store
            .write_fact(
                &measurement(1200.0, EvidenceClass::ReferenceValidated),
                &prov,
            )
            .await
            .unwrap();

        let recalled = store
            .recall_with_context("thermal conductivity", "t1", 10)
            .await
            .unwrap();
        assert_eq!(
            recalled.len(),
            2,
            "different conditions are different facts"
        );
        assert!(
            recalled
                .iter()
                .all(|fact| fact.evidence_class == EvidenceClass::Research)
        );
        assert!(recalled.iter().any(|fact| {
            fact.conditions[0].value == ConditionValue::Number(1200.0) && fact.confidence > 0.9
        }));
    }

    // ── Entity vectors ───────────────────────────────────────────────────

    /// Deterministic 3-dim stand-in for the real ONNX backend.
    struct MockEmbed;

    fn mock_vec(text: &str) -> Vec<f32> {
        match text {
            "Ti-6Al-4V" => vec![1.0, 0.0, 0.0],
            "alpha" => vec![0.0, 1.0, 0.0],
            "Inconel 718" => vec![0.0, 0.0, 1.0],
            _ => vec![0.6, 0.6, 0.6],
        }
    }

    #[async_trait::async_trait]
    impl prism_embed::EmbedBackend for MockEmbed {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| mock_vec(t)).collect())
        }
        fn dimensions(&self) -> usize {
            3
        }
        fn id(&self) -> &str {
            "test:mock"
        }
    }

    /// An empty index is a legitimate empty ANSWER, not a failure — and it
    /// is the only condition allowed to produce `Ok(vec![])`.
    #[tokio::test]
    async fn semantic_search_entities_empty_store_is_empty() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        assert_eq!(store.entity_embedding_count("t1").await.unwrap(), 0);
        let hits = store
            .semantic_search_entities(&[1.0, 0.0, 0.0], "t1", 5)
            .await
            .expect("an empty index must not be reported as a broken one");
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn entity_vectors_store_and_search_ranked() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        let facts = vec![
            fact("phase", "Ti-6Al-4V", "has_phase", "alpha"),
            fact("processing", "Inconel 718", "processed_by", "LPBF"),
        ];
        for f in &facts {
            store.write_fact(f, &prov).await.unwrap();
        }

        // 4 distinct names, each resolving to exactly one entity key.
        let stored = store
            .embed_and_store_entities(&facts, "t1", &MockEmbed)
            .await
            .unwrap();
        assert_eq!(stored, 4);
        assert_eq!(store.entity_embedding_count("t1").await.unwrap(), 4);

        // Query near the Ti-6Al-4V axis → ranked best-first.
        let hits = store
            .semantic_search_entities(&[1.0, 0.2, 0.0], "t1", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 4);
        assert_eq!(hits[0].0, "Ti-6Al-4V");
        assert!(hits[0].1 > 0.9);
        assert!(
            hits.windows(2).all(|w| w[0].1 >= w[1].1),
            "scores must be descending: {hits:?}"
        );

        // Limit is respected.
        let hits = store
            .semantic_search_entities(&[1.0, 0.2, 0.0], "t1", 1)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);

        // Tenant scoping: nothing leaks into another tenant.
        assert!(
            store
                .semantic_search_entities(&[1.0, 0.2, 0.0], "other", 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn same_name_under_two_labels_searches_once() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        // "alpha" exists as both Phase (object) and Matter (subject).
        let facts = vec![
            fact("phase", "Ti-6Al-4V", "has_phase", "alpha"),
            fact("phase", "alpha", "has_phase", "beta"),
        ];
        for f in &facts {
            store.write_fact(f, &prov).await.unwrap();
        }

        // One vector per entity ROW (both labels of "alpha" get one) …
        let stored = store
            .embed_and_store_entities(&facts, "t1", &MockEmbed)
            .await
            .unwrap();
        assert_eq!(
            stored, 4,
            "Matter:ti-6al-4v, Phase:alpha, Matter:alpha, Phase:beta"
        );

        // … but search reports the display name once.
        let hits = store
            .semantic_search_entities(&[0.0, 1.0, 0.0], "t1", 10)
            .await
            .unwrap();
        assert_eq!(hits[0].0, "alpha");
        assert_eq!(
            hits.iter().filter(|(name, _)| name == "alpha").count(),
            1,
            "same name under two labels must be deduped: {hits:?}"
        );
    }

    /// A dimension mismatch matches nothing, so it must be an error that
    /// names both dimensionalities — never an empty list, which the caller
    /// cannot tell apart from "the index is empty".
    #[tokio::test]
    async fn semantic_search_entities_errors_on_mismatched_dims() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();
        store
            .write_fact(&fact("phase", "Ti-6Al-4V", "has_phase", "alpha"), &prov)
            .await
            .unwrap();

        store
            .store_entity_embedding(
                &entity_key("t1", "Matter", "Ti-6Al-4V"),
                "t1",
                &[1.0, 0.0, 0.0],
            )
            .await
            .unwrap();

        // 4-dim query cannot compare against the 3-dim vector.
        let err = store
            .semantic_search_entities(&[1.0, 0.0, 0.0, 0.0], "t1", 10)
            .await
            .expect_err("a dimension mismatch must be loud, not an empty list");
        let msg = format!("{err:#}");
        assert!(
            msg.contains('3') && msg.contains('4'),
            "error must name the stored and query dimensionality: {msg}"
        );

        // Matching dimensionality finds it.
        let hits = store
            .semantic_search_entities(&[1.0, 0.0, 0.0], "t1", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "Ti-6Al-4V");
    }

    /// Similarity must come back on the documented `[-1, 1]` scale after
    /// the conversion from Turso's `[0, 2]` cosine *distance*.
    #[tokio::test]
    async fn semantic_search_entities_similarity_scale() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();
        store
            .write_fact(&fact("phase", "Ti-6Al-4V", "has_phase", "alpha"), &prov)
            .await
            .unwrap();
        store
            .store_entity_embedding(
                &entity_key("t1", "Matter", "Ti-6Al-4V"),
                "t1",
                &[1.0, 0.0, 0.0],
            )
            .await
            .unwrap();

        let same = store
            .semantic_search_entities(&[1.0, 0.0, 0.0], "t1", 1)
            .await
            .unwrap();
        assert!((same[0].1 - 1.0).abs() < 1e-5, "identical → +1: {same:?}");

        let orthogonal = store
            .semantic_search_entities(&[0.0, 1.0, 0.0], "t1", 1)
            .await
            .unwrap();
        assert!(
            orthogonal[0].1.abs() < 1e-5,
            "orthogonal → 0: {orthogonal:?}"
        );

        let opposite = store
            .semantic_search_entities(&[-1.0, 0.0, 0.0], "t1", 1)
            .await
            .unwrap();
        assert!(
            (opposite[0].1 + 1.0).abs() < 1e-5,
            "opposite → -1: {opposite:?}"
        );
    }

    /// Retrieval by MEANING with the real on-device model: a paraphrase
    /// that shares **no word at all** with any stored entity must still
    /// rank the metal-joining entities above the bread-making ones. A
    /// keyword index scores this query 0 against everything.
    ///
    /// `#[ignore]`d: needs the ~90 MB ONNX model in `~/.prism/models/embed/`.
    /// Run with `cargo test -p prism-provenance -- --ignored`.
    /// Not compiled on Intel macOS, which has no ONNX Runtime build and so
    /// no `NativeOnnx` (see `prism_embed`).
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[tokio::test]
    #[ignore = "downloads/uses the local ONNX embedding model"]
    async fn native_embeddings_retrieve_by_meaning_not_keywords() {
        use prism_embed::EmbedBackend as _;

        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        let facts = vec![
            fact(
                "processing",
                "aluminium bicycle frame welding",
                "processed_by",
                "friction stir welding",
            ),
            fact(
                "phase",
                "sourdough bread fermentation",
                "has_phase",
                "wild yeast starter",
            ),
        ];
        for f in &facts {
            store.write_fact(f, &prov).await.unwrap();
        }
        let backend = prism_embed::NativeOnnx::new().expect("local ONNX model");
        store
            .embed_and_store_entities(&facts, "t1", &backend)
            .await
            .unwrap();

        let query = "joining two pieces of metal together without melting them";
        let query_vec = backend
            .embed(std::slice::from_ref(&query.to_string()))
            .await
            .unwrap()
            .remove(0);
        assert_eq!(query_vec.len(), 384, "BGE-small-en-v1.5 is 384-dimension");

        let hits = store
            .semantic_search_entities(&query_vec, "t1", 4)
            .await
            .unwrap();
        assert_eq!(hits.len(), 4, "all four entities are scored: {hits:?}");

        // The premise: this is retrieval by meaning, not by keyword. Assert
        // it rather than trusting the wording — no query word occurs in any
        // entity name, so lexical search has nothing to match on.
        let query_words: std::collections::HashSet<&str> = query.split_whitespace().collect();
        for (name, _) in &hits {
            for word in name.split_whitespace() {
                assert!(
                    !query_words.contains(word),
                    "'{word}' is shared with the query — the test would no longer \
                     distinguish semantic retrieval from keyword matching"
                );
            }
        }

        let metal_joining = ["aluminium bicycle frame welding", "friction stir welding"];
        assert!(
            metal_joining.contains(&hits[0].0.as_str())
                && metal_joining.contains(&hits[1].0.as_str()),
            "both metal-joining entities must outrank both bread-making ones: {hits:?}"
        );
        assert!(
            hits[1].1 > hits[2].1,
            "the two domains must be separated, not tied: {hits:?}"
        );
        assert!(
            hits.iter().all(|(_, s)| (-1.0..=1.0).contains(s)),
            "similarities must stay in [-1, 1]: {hits:?}"
        );
    }

    // ── Tenant isolation ───────────────────────────────────────────────

    /// `crates/mesh/src/sync.rs` writes every peer-supplied entity under
    /// the `"mesh"` tenant precisely "so peer-synced data never blends
    /// with locally [ingested data]". A mesh peer chooses the entity
    /// `name` verbatim, so if the entity primary key is not
    /// tenant-qualified, naming an entity the user already has hands the
    /// peer that row: the ON CONFLICT branch reassigns `tenant`, and
    /// every local read filters `WHERE tenant = 'local'`, so the user's
    /// own knowledge silently disappears.
    #[tokio::test]
    async fn peer_tenant_cannot_capture_a_local_entity() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        // The user ingests a paper locally.
        let mut local = test_prov();
        local.tenant = "local".into();
        store
            .write_fact(
                &fact("phase", "Ti-6Al-4V", "has_phase", "alpha-beta"),
                &local,
            )
            .await
            .unwrap();

        assert!(
            store
                .graph_search("Ti-6Al-4V", "local", 10)
                .await
                .unwrap()
                .iter()
                .any(|n| n.name == "Ti-6Al-4V"),
            "precondition: the local entity must exist before the peer syncs"
        );

        // A subscribed mesh peer returns a dataset row whose `name` is
        // the same material, spelled its way. This is peer-controlled
        // input: sync.rs takes `row["name"]` straight off the wire.
        let mut mesh = test_prov();
        mesh.tenant = "mesh".into();
        store
            .write_fact(
                &LocalFact {
                    subject: "TI-6AL-4V".into(),
                    predicate: "SYNCED_FROM".into(),
                    object: "peer-dataset".into(),
                    value: None,
                    unit: None,
                    confidence: None,
                    kind: None,
                },
                &mesh,
            )
            .await
            .unwrap();

        // The user's own entity must still be theirs.
        let local_hits = store.graph_search("Ti-6Al-4V", "local", 10).await.unwrap();
        assert!(
            local_hits.iter().any(|n| n.name == "Ti-6Al-4V"),
            "a mesh peer captured the local tenant's entity — the user's own \
             ingested knowledge vanished from every `tenant = 'local'` read"
        );
    }

    /// A database written before entity keys carried the tenant must be
    /// rewritten on open, or the next re-ingest writes a SECOND row for
    /// the same entity and edges split across two key spaces.
    #[tokio::test]
    async fn legacy_unqualified_keys_migrate_on_open() {
        let db = TempDb::new();
        {
            let store = ProvenanceStore::open(&db.path).await.unwrap();
            let mut prov = test_prov();
            prov.tenant = "local".into();
            store
                .write_fact(
                    &fact("phase", "Ti-6Al-4V", "has_phase", "alpha-beta"),
                    &prov,
                )
                .await
                .unwrap();
            // Rewind to the pre-fix on-disk shape.
            for sql in [
                "UPDATE emmo_entity SET key = replace(key, 'local|', '')",
                "UPDATE emmo_edge SET source_key = replace(source_key, 'local|', ''),
                     target_key = replace(target_key, 'local|', ''),
                     id = tenant || '|' || replace(source_key, 'local|', '') || '|'
                          || rel_type || '|' || replace(target_key, 'local|', '')",
            ] {
                store.conn.execute(sql, ()).await.unwrap();
            }
            assert_eq!(
                count(
                    &store,
                    "SELECT COUNT(*) FROM emmo_entity WHERE instr(key, '|') = 0",
                )
                .await,
                2,
                "precondition: the legacy shape must have unqualified keys"
            );
        }

        // Reopening runs init_schema, which must migrate.
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE instr(key, '|') = 0",
            )
            .await,
            0,
            "legacy entity keys were not tenant-qualified on open"
        );

        // Re-ingesting the same fact must merge, not duplicate.
        let mut prov = test_prov();
        prov.tenant = "local".into();
        store
            .write_fact(
                &fact("phase", "Ti-6Al-4V", "has_phase", "alpha-beta"),
                &prov,
            )
            .await
            .unwrap();
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM emmo_entity").await,
            2,
            "re-ingest duplicated entities across the key-format change"
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM emmo_edge").await,
            1,
            "re-ingest duplicated the edge across the key-format change"
        );
    }

    /// The same name under two tenants must be two rows, each keeping its
    /// own owner. `emmo_embedding` is keyed by the entity key, so once the
    /// entity key separates, entity vectors separate with it.
    #[tokio::test]
    async fn same_name_under_two_tenants_stays_two_owned_rows() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        let mut local = test_prov();
        local.tenant = "local".into();
        store
            .write_fact(
                &fact("phase", "Ti-6Al-4V", "has_phase", "alpha-beta"),
                &local,
            )
            .await
            .unwrap();
        let mut mesh = test_prov();
        mesh.tenant = "mesh".into();
        store
            .write_fact(&fact("phase", "Ti-6Al-4V", "has_phase", "beta"), &mesh)
            .await
            .unwrap();

        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE label = 'Matter' \
                 AND tenant = 'local'",
            )
            .await,
            1,
            "the local tenant lost its Matter row to the peer"
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE label = 'Matter' \
                 AND tenant = 'mesh'",
            )
            .await,
            1,
            "the peer tenant has no Matter row of its own"
        );
    }
}
