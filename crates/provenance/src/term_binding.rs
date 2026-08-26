//! Durable records for the post-extraction ontology resolution ladder.
//!
//! A BINDING is the store's answer to "which ontology class does this
//! free-text property term denote", recorded per `(tenant, canonical term)`
//! with the LADDER RUNG that produced it and — for probabilistic rungs — the
//! similarity score that justified it. Facts keep their free-text
//! predicate/object spelling; the binding row joins to them through the
//! term, so extending an ontology later re-resolves every fact that carries
//! the term WITHOUT re-reading any paper: re-run the ladder over
//! [`ProvenanceStore::unbound_term_bindings`] and the upgraded rows re-bind
//! everything at once.
//!
//! This module is deliberately vocabulary-neutral: rung semantics (what
//! "exact" or "semantic" means, which labels are candidates, the similarity
//! threshold) live in the ingest crate's resolver. The store validates only
//! structure: a bound row names its class and declaring ontology; an unbound
//! row names neither.

use anyhow::{Result, bail};
use turso::Value;

use crate::{ProvenanceStore, get_opt_str, get_str};

/// Rung numbers the ladder records. Smaller is stronger: deterministic
/// lexical binds outrank the probabilistic semantic bind, which outranks the
/// unbound "proposed" record. The names are the resolver's; the store keeps
/// only the ordering.
pub const TERM_BINDING_RUNG_EXACT: i64 = 1;
pub const TERM_BINDING_RUNG_NORMALIZED: i64 = 2;
pub const TERM_BINDING_RUNG_SEMANTIC: i64 = 3;
pub const TERM_BINDING_RUNG_PROPOSED: i64 = 4;

/// One resolution-ladder record for one free-text term in one tenant.
#[derive(Debug, Clone, PartialEq)]
pub struct TermBinding {
    pub tenant: String,
    /// Canonical spelling ([`crate::canonical_key`]) — the row key, and the
    /// join key to `emmo_entity.canonical_name` and to fact text.
    pub term: String,
    /// The exact source spelling the ladder first saw for this term.
    pub verbatim: String,
    /// Bound class identity; `None` records an unbound (rung 4) term.
    pub class_iri: Option<String>,
    /// Id of the loaded ontology that declared `class_iri`.
    pub ontology_id: Option<String>,
    /// Ladder rung, 1..=4 (see the `TERM_BINDING_RUNG_*` constants).
    pub rung: i64,
    /// Cosine similarity of the nearest class label, recorded for BOTH
    /// semantic binds and below-threshold outcomes so the threshold can be
    /// tuned from data instead of guessed twice. `None` when no embedding
    /// backend or class vectors were available.
    pub score: Option<f64>,
    /// The similarity threshold in force when the ladder ran. Recorded with
    /// every rung that consulted geometry.
    pub threshold: Option<f64>,
    /// Embedding model id behind `score`.
    pub model: Option<String>,
    /// Governance-queue item id when rung 4 enqueued a class proposal —
    /// the pending-extension pointer a fact stays associated with.
    pub proposal_item_id: Option<String>,
    /// The class this term was scored AGAINST — recorded even when the score
    /// fell below threshold and nothing was bound.
    ///
    /// Without it `score` is uninterpretable and the threshold cannot be
    /// tuned, which is the stated reason scores are recorded at all.
    /// Measured 2026-08-26 on a real corpus: 80 rows carried a score, **zero**
    /// named the candidate, so "0.761" meant 0.761-against-what. It also
    /// blocks adjudication — a judge asked "are these the same concept?"
    /// needs BOTH names, and only one was stored.
    pub nearest_class_iri: Option<String>,
    /// Human-readable label of [`Self::nearest_class_iri`], so a reviewer or
    /// a judge sees "solidus temperature", not an opaque IRI.
    pub nearest_label: Option<String>,
    /// RFC 3339 timestamp of the resolution.
    pub resolved_at: String,
}

/// One class-label vector of one loaded ontology, ready to store.
#[derive(Debug, Clone)]
pub struct ClassLabelEmbedding {
    pub class_iri: String,
    pub label: String,
    pub vector: Vec<f32>,
}

/// One nearest-neighbour answer over the stored class-label vectors.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassLabelNeighbor {
    pub ontology_id: String,
    pub class_iri: String,
    pub label: String,
    /// Cosine similarity (`1 - vector_distance_cos`), higher is nearer.
    pub similarity: f64,
}

/// Schema for the ladder's two durable surfaces. Called from the store's
/// `init_schema`, same `CREATE TABLE IF NOT EXISTS` idempotency as every
/// other table there.
pub(crate) async fn init_schema(conn: &turso::Connection) -> Result<()> {
    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS ontology_term_binding (
            tenant TEXT NOT NULL,
            term TEXT NOT NULL,
            verbatim TEXT NOT NULL,
            class_iri TEXT,
            ontology_id TEXT,
            rung INTEGER NOT NULL,
            score REAL,
            threshold REAL,
            model TEXT,
            proposal_item_id TEXT,
            nearest_class_iri TEXT,
            nearest_label TEXT,
            resolved_at TEXT NOT NULL,
            PRIMARY KEY (tenant, term)
        )"#,
        (),
    )
    .await?;
    // Legacy stores predate the near-miss candidate columns; CREATE TABLE IF
    // NOT EXISTS does not upgrade them.
    crate::add_column_if_absent(conn, "ontology_term_binding", "nearest_class_iri", "TEXT").await?;
    crate::add_column_if_absent(conn, "ontology_term_binding", "nearest_label", "TEXT").await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ontology_term_binding_tenant \
         ON ontology_term_binding(tenant)",
        (),
    )
    .await?;
    // Class-label vectors are keyed by the DECLARATION (ontology id + class
    // IRI + label + model), not by tenant: an ontology's geometry is shared
    // by every tenant that loads it. Deliberately a separate table from
    // `emmo_embedding`, which is keyed by ENTITY key and joined against
    // `emmo_entity` by every entity-geometry read — parking class vectors
    // there under a reserved pseudo-tenant would put non-entities into
    // entity-scoped counts and force key-parsing tricks.
    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS ontology_class_embedding (
            ontology_id TEXT NOT NULL,
            class_iri TEXT NOT NULL,
            label TEXT NOT NULL,
            model TEXT NOT NULL,
            dim INTEGER NOT NULL,
            vector BLOB NOT NULL,
            PRIMARY KEY (ontology_id, class_iri, label, model)
        )"#,
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ontology_class_embedding_partition \
         ON ontology_class_embedding(ontology_id, model)",
        (),
    )
    .await?;
    Ok(())
}

fn validate_binding(binding: &TermBinding) -> Result<()> {
    if binding.tenant.trim().is_empty() {
        bail!("term binding tenant must not be empty");
    }
    if binding.term.trim().is_empty() {
        bail!("term binding term must not be empty");
    }
    if !(TERM_BINDING_RUNG_EXACT..=TERM_BINDING_RUNG_PROPOSED).contains(&binding.rung) {
        bail!(
            "term binding rung must be {TERM_BINDING_RUNG_EXACT}..={TERM_BINDING_RUNG_PROPOSED}, \
             got {}",
            binding.rung
        );
    }
    let bound = binding.rung < TERM_BINDING_RUNG_PROPOSED;
    match (bound, &binding.class_iri, &binding.ontology_id) {
        (true, Some(iri), Some(id)) if !iri.trim().is_empty() && !id.trim().is_empty() => {}
        (true, ..) => bail!(
            "a bound term (rung {}) must name its class IRI and declaring ontology",
            binding.rung
        ),
        (false, None, None) => {}
        (false, ..) => bail!(
            "an unbound term (rung {TERM_BINDING_RUNG_PROPOSED}) must not carry a class IRI \
             or ontology id"
        ),
    }
    if let Some(score) = binding.score
        && !score.is_finite()
    {
        bail!("term binding score must be finite, got {score}");
    }
    Ok(())
}

fn validate_vector(vector: &[f32], context: &str) -> Result<()> {
    if vector.is_empty() {
        bail!("embedding vector for {context} is empty");
    }
    if vector.iter().any(|component| !component.is_finite()) {
        bail!("embedding vector for {context} contains a non-finite component");
    }
    Ok(())
}

impl ProvenanceStore {
    /// UPSERT one ladder record with never-downgrade semantics: an existing
    /// BOUND row is replaced only by an equal-or-stronger rung, while an
    /// UNBOUND row is replaced by any outcome — that replacement IS the
    /// re-resolution path (a term recorded `proposed` today binds tomorrow
    /// once the ontology grows, without touching any fact row). Returns
    /// whether the stored row now reflects `binding`.
    pub async fn record_term_binding(&self, binding: &TermBinding) -> Result<bool> {
        validate_binding(binding)?;
        let _same_handle_guard = self.write_lock.lock().await;
        let changed = self
            .conn
            .execute(
                r#"INSERT INTO ontology_term_binding
                   (tenant, term, verbatim, class_iri, ontology_id, rung,
                    score, threshold, model, proposal_item_id, resolved_at,
                    nearest_class_iri, nearest_label)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                   ON CONFLICT(tenant, term) DO UPDATE SET
                       verbatim = excluded.verbatim,
                       class_iri = excluded.class_iri,
                       ontology_id = excluded.ontology_id,
                       rung = excluded.rung,
                       score = excluded.score,
                       threshold = excluded.threshold,
                       model = excluded.model,
                       proposal_item_id = excluded.proposal_item_id,
                       nearest_class_iri = excluded.nearest_class_iri,
                       nearest_label = excluded.nearest_label,
                       resolved_at = excluded.resolved_at
                   WHERE ontology_term_binding.class_iri IS NULL
                      OR excluded.rung <= ontology_term_binding.rung"#,
                [
                    Value::Text(binding.tenant.clone()),
                    Value::Text(binding.term.clone()),
                    Value::Text(binding.verbatim.clone()),
                    opt_text(&binding.class_iri),
                    opt_text(&binding.ontology_id),
                    Value::Integer(binding.rung),
                    binding.score.map_or(Value::Null, Value::Real),
                    binding.threshold.map_or(Value::Null, Value::Real),
                    opt_text(&binding.model),
                    opt_text(&binding.proposal_item_id),
                    Value::Text(binding.resolved_at.clone()),
                    opt_text(&binding.nearest_class_iri),
                    opt_text(&binding.nearest_label),
                ],
            )
            .await?;
        Ok(changed > 0)
    }

    /// The stored record for one canonical term, if any.
    pub async fn term_binding(&self, tenant: &str, term: &str) -> Result<Option<TermBinding>> {
        let mut bindings = self
            .query_bindings(
                "SELECT tenant, term, verbatim, class_iri, ontology_id, rung, \
                 score, threshold, model, proposal_item_id, resolved_at, \
                      nearest_class_iri, nearest_label \
                 FROM ontology_term_binding WHERE tenant = ?1 AND term = ?2",
                vec![
                    Value::Text(tenant.to_string()),
                    Value::Text(term.to_string()),
                ],
            )
            .await?;
        Ok(bindings.pop())
    }

    /// Every record for `tenant`, ordered by term.
    pub async fn term_bindings(&self, tenant: &str) -> Result<Vec<TermBinding>> {
        self.query_bindings(
            "SELECT tenant, term, verbatim, class_iri, ontology_id, rung, \
             score, threshold, model, proposal_item_id, resolved_at, \
                  nearest_class_iri, nearest_label \
             FROM ontology_term_binding WHERE tenant = ?1 ORDER BY term",
            vec![Value::Text(tenant.to_string())],
        )
        .await
    }

    /// The re-resolution work list: every term still unbound in `tenant`.
    pub async fn unbound_term_bindings(&self, tenant: &str) -> Result<Vec<TermBinding>> {
        self.query_bindings(
            "SELECT tenant, term, verbatim, class_iri, ontology_id, rung, \
             score, threshold, model, proposal_item_id, resolved_at, \
                  nearest_class_iri, nearest_label \
             FROM ontology_term_binding \
             WHERE tenant = ?1 AND class_iri IS NULL ORDER BY term",
            vec![Value::Text(tenant.to_string())],
        )
        .await
    }

    async fn query_bindings(&self, sql: &str, params: Vec<Value>) -> Result<Vec<TermBinding>> {
        let mut rows = self.conn.query(sql, params).await?;
        let mut bindings = Vec::new();
        while let Some(row) = rows.next().await? {
            bindings.push(TermBinding {
                tenant: get_str(&row, 0)?,
                term: get_str(&row, 1)?,
                verbatim: get_str(&row, 2)?,
                class_iri: get_opt_str(&row, 3)?,
                ontology_id: get_opt_str(&row, 4)?,
                rung: row
                    .get_value(5)?
                    .as_integer()
                    .copied()
                    .ok_or_else(|| anyhow::anyhow!("term binding rung is not an integer"))?,
                score: row.get_value(6)?.as_real().copied(),
                threshold: row.get_value(7)?.as_real().copied(),
                model: get_opt_str(&row, 8)?,
                proposal_item_id: get_opt_str(&row, 9)?,
                resolved_at: get_str(&row, 10)?,
                nearest_class_iri: get_opt_str(&row, 11)?,
                nearest_label: get_opt_str(&row, 12)?,
            });
        }
        Ok(bindings)
    }

    /// UPSERT class-label vectors for one `(ontology, model)` partition.
    /// Vectors must be finite, non-empty, and dimensionally consistent
    /// within the call. Returns how many rows were written.
    pub async fn store_class_label_embeddings(
        &self,
        ontology_id: &str,
        model: &str,
        entries: &[ClassLabelEmbedding],
    ) -> Result<usize> {
        if ontology_id.trim().is_empty() {
            bail!("ontology id must not be empty");
        }
        if model.trim().is_empty() {
            bail!("embedding model id must not be empty");
        }
        if entries.is_empty() {
            return Ok(0);
        }
        let dimensions = entries[0].vector.len();
        for entry in entries {
            validate_vector(
                &entry.vector,
                &format!("class {} label {:?}", entry.class_iri, entry.label),
            )?;
            if entry.vector.len() != dimensions {
                bail!(
                    "class-label embedding batch for model `{model}` has mixed dimensions: \
                     expected {dimensions}, got {} for label {:?}",
                    entry.vector.len(),
                    entry.label
                );
            }
        }
        let _same_handle_guard = self.write_lock.lock().await;
        for entry in entries {
            self.conn
                .execute(
                    r#"INSERT OR REPLACE INTO ontology_class_embedding
                       (ontology_id, class_iri, label, model, dim, vector)
                       VALUES (?1, ?2, ?3, ?4, ?5, ?6)"#,
                    [
                        Value::Text(ontology_id.to_string()),
                        Value::Text(entry.class_iri.clone()),
                        Value::Text(entry.label.clone()),
                        Value::Text(model.to_string()),
                        Value::Integer(entry.vector.len() as i64),
                        Value::Blob(prism_embed::vec_to_le_bytes(&entry.vector)),
                    ],
                )
                .await?;
        }
        Ok(entries.len())
    }

    /// The `(class_iri, label)` pairs already embedded for one
    /// `(ontology, model)` partition — the seeding diff.
    pub async fn class_label_embedding_keys(
        &self,
        ontology_id: &str,
        model: &str,
    ) -> Result<Vec<(String, String)>> {
        let mut rows = self
            .conn
            .query(
                "SELECT class_iri, label FROM ontology_class_embedding \
                 WHERE ontology_id = ?1 AND model = ?2 ORDER BY class_iri, label",
                [
                    Value::Text(ontology_id.to_string()),
                    Value::Text(model.to_string()),
                ],
            )
            .await?;
        let mut keys = Vec::new();
        while let Some(row) = rows.next().await? {
            keys.push((get_str(&row, 0)?, get_str(&row, 1)?));
        }
        Ok(keys)
    }

    /// The `limit` nearest class labels to `vector` across EXACTLY the given
    /// ontology ids' stored vectors, same-model same-dimension only, ordered
    /// nearest first. `vector_distance_cos` scores the stored blobs inside
    /// Turso; the returned similarity is `1 - distance`. Tie-breaking among
    /// equal distances and any semantic threshold are caller policy — this
    /// layer only measures.
    pub async fn nearest_class_labels(
        &self,
        vector: &[f32],
        ontology_ids: &[&str],
        model: &str,
        limit: usize,
    ) -> Result<Vec<ClassLabelNeighbor>> {
        validate_vector(vector, "nearest-class probe")?;
        if model.trim().is_empty() {
            bail!("embedding model id must not be empty");
        }
        if ontology_ids.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let id_placeholders = (0..ontology_ids.len())
            .map(|index| format!("?{}", index + 4))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT ontology_id, class_iri, label, \
                    vector_distance_cos(vector, ?1) AS distance \
             FROM ontology_class_embedding \
             WHERE model = ?2 AND dim = ?3 AND ontology_id IN ({id_placeholders}) \
             ORDER BY distance ASC, ontology_id ASC, class_iri ASC, label ASC \
             LIMIT {limit}",
        );
        let mut params = vec![
            Value::Blob(prism_embed::vec_to_le_bytes(vector)),
            Value::Text(model.to_string()),
            Value::Integer(vector.len() as i64),
        ];
        params.extend(ontology_ids.iter().map(|id| Value::Text((*id).to_string())));
        let mut rows = self.conn.query(&sql, params).await?;
        let mut neighbors = Vec::new();
        while let Some(row) = rows.next().await? {
            let distance = match row.get_value(3)? {
                Value::Real(distance) => distance,
                Value::Integer(distance) => distance as f64,
                other => bail!("vector_distance_cos returned {other:?}, expected a number"),
            };
            neighbors.push(ClassLabelNeighbor {
                ontology_id: get_str(&row, 0)?,
                class_iri: get_str(&row, 1)?,
                label: get_str(&row, 2)?,
                similarity: 1.0 - distance,
            });
        }
        Ok(neighbors)
    }

    /// Stamp a bound term's class IRI onto every entity in `tenant` whose
    /// canonical name IS the term and whose class is still unknown. The
    /// `class_iri IS NULL` guard makes the stamp conservative and reversible:
    /// an identity some extraction declared is never overwritten, and a
    /// ladder stamp is exactly the set of rows that joins back to the
    /// binding row through `canonical_name = term` — reverting a binding
    /// can null them out again without touching declared classifications.
    /// Returns how many entities were stamped.
    pub async fn apply_term_binding_to_entities(
        &self,
        tenant: &str,
        term: &str,
        class_iri: &str,
    ) -> Result<u64> {
        if class_iri.trim().is_empty() {
            bail!("refusing to stamp an empty class IRI");
        }
        let _same_handle_guard = self.write_lock.lock().await;
        let stamped = self
            .conn
            .execute(
                "UPDATE emmo_entity SET class_iri = ?3 \
                 WHERE tenant = ?1 AND canonical_name = ?2 AND class_iri IS NULL",
                [
                    Value::Text(tenant.to_string()),
                    Value::Text(term.to_string()),
                    Value::Text(class_iri.to_string()),
                ],
            )
            .await?;
        Ok(stamped)
    }
}

fn opt_text(value: &Option<String>) -> Value {
    value
        .as_ref()
        .map_or(Value::Null, |text| Value::Text(text.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDb {
        path: PathBuf,
    }

    impl TempDb {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "prism_term_binding_test_{}.db",
                uuid::Uuid::new_v4()
            ));
            Self { path }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut p = self.path.clone().into_os_string();
                p.push(suffix);
                let _ = std::fs::remove_file(PathBuf::from(p));
            }
        }
    }

    fn bound(rung: i64, class_iri: &str, score: Option<f64>) -> TermBinding {
        TermBinding {
            tenant: "local".into(),
            term: "yield strength".into(),
            verbatim: "Yield Strength".into(),
            class_iri: Some(class_iri.into()),
            ontology_id: Some("emmo".into()),
            rung,
            score,
            threshold: score.map(|_| 0.85),
            model: score.map(|_| "test:mock".into()),
            proposal_item_id: None,
            resolved_at: "2026-08-24T00:00:00Z".into(),
            nearest_class_iri: None,
            nearest_label: None,
        }
    }

    fn unbound(score: Option<f64>, proposal: Option<&str>) -> TermBinding {
        TermBinding {
            class_iri: None,
            ontology_id: None,
            rung: TERM_BINDING_RUNG_PROPOSED,
            score,
            proposal_item_id: proposal.map(str::to_string),
            ..bound(TERM_BINDING_RUNG_PROPOSED, "unused", score)
        }
    }

    /// The never-downgrade contract, value by value: an unbound row is
    /// upgradable by anything (that IS re-resolution), a bound row refuses a
    /// weaker rung, accepts an equal-rung refresh, and accepts a stronger
    /// rung.
    /// A below-threshold term must keep the class it nearly matched.
    ///
    /// Measured 2026-08-26: 80 rows carried a score and ZERO named the
    /// candidate, so "0.761" meant 0.761-against-what — the threshold could
    /// not be calibrated from its own recorded data, and a judge asked "are
    /// these the same concept?" had only one of the two names.
    #[tokio::test]
    async fn a_near_miss_records_what_it_nearly_matched() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.expect("store opens");

        let mut miss = unbound(Some(0.761), None);
        miss.nearest_class_iri = Some("https://w3id.org/emmo#SolidusTemperature".into());
        miss.nearest_label = Some("solidus temperature".into());
        store.record_term_binding(&miss).await.expect("record");

        let back = store
            .term_binding(&miss.tenant, &miss.term)
            .await
            .expect("read")
            .expect("row exists");

        assert!(
            back.class_iri.is_none(),
            "still unbound — this is a near MISS"
        );
        assert_eq!(back.score, Some(0.761));
        assert_eq!(
            back.nearest_label.as_deref(),
            Some("solidus temperature"),
            "a score with no candidate cannot calibrate a threshold or be judged"
        );
        assert_eq!(
            back.nearest_class_iri.as_deref(),
            Some("https://w3id.org/emmo#SolidusTemperature")
        );
    }

    #[tokio::test]
    async fn bound_rows_never_downgrade_and_unbound_rows_upgrade() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        // Unbound first — the ladder found nothing.
        assert!(
            store
                .record_term_binding(&unbound(Some(0.4), Some("class|'x'|parents=[]")))
                .await
                .unwrap()
        );

        // Re-resolution after an ontology extension: semantic bind upgrades.
        assert!(
            store
                .record_term_binding(&bound(
                    TERM_BINDING_RUNG_SEMANTIC,
                    "https://example.test/A",
                    Some(0.9)
                ))
                .await
                .unwrap()
        );
        let row = store
            .term_binding("local", "yield strength")
            .await
            .unwrap()
            .expect("row exists");
        assert_eq!(row.rung, TERM_BINDING_RUNG_SEMANTIC);
        assert_eq!(row.class_iri.as_deref(), Some("https://example.test/A"));
        assert_eq!(row.score, Some(0.9));
        assert_eq!(
            row.proposal_item_id, None,
            "the binding superseded the pending proposal pointer"
        );

        // A later unbound outcome must NOT clobber the bind.
        assert!(
            !store
                .record_term_binding(&unbound(Some(0.2), None))
                .await
                .unwrap()
        );
        let row = store
            .term_binding("local", "yield strength")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.rung, TERM_BINDING_RUNG_SEMANTIC);
        assert_eq!(row.class_iri.as_deref(), Some("https://example.test/A"));

        // A deterministic bind outranks the probabilistic one.
        assert!(
            store
                .record_term_binding(&bound(
                    TERM_BINDING_RUNG_EXACT,
                    "https://example.test/B",
                    None
                ))
                .await
                .unwrap()
        );
        let row = store
            .term_binding("local", "yield strength")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.rung, TERM_BINDING_RUNG_EXACT);
        assert_eq!(row.class_iri.as_deref(), Some("https://example.test/B"));

        // And a semantic result can no longer displace it.
        assert!(
            !store
                .record_term_binding(&bound(
                    TERM_BINDING_RUNG_SEMANTIC,
                    "https://example.test/C",
                    Some(0.99)
                ))
                .await
                .unwrap()
        );
        let row = store
            .term_binding("local", "yield strength")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.class_iri.as_deref(), Some("https://example.test/B"));
    }

    /// Structural validation: rungs outside the ladder, bound rows without
    /// identity, and unbound rows WITH identity are all refused.
    #[tokio::test]
    async fn malformed_bindings_are_refused() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        let mut wrong_rung = bound(TERM_BINDING_RUNG_EXACT, "https://example.test/A", None);
        wrong_rung.rung = 5;
        assert!(store.record_term_binding(&wrong_rung).await.is_err());

        let mut bound_without_iri = bound(TERM_BINDING_RUNG_EXACT, "x", None);
        bound_without_iri.class_iri = None;
        assert!(store.record_term_binding(&bound_without_iri).await.is_err());

        let mut unbound_with_iri = unbound(None, None);
        unbound_with_iri.class_iri = Some("https://example.test/A".into());
        assert!(store.record_term_binding(&unbound_with_iri).await.is_err());

        assert!(
            store
                .term_binding("local", "yield strength")
                .await
                .unwrap()
                .is_none(),
            "a refused record must leave nothing behind"
        );
    }

    /// The unbound work list is exactly the re-resolution input.
    #[tokio::test]
    async fn unbound_term_bindings_list_only_unresolved_terms() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        let mut pending = unbound(Some(0.3), Some("class|'creep'|parents=[]"));
        pending.term = "creep rupture life".into();
        pending.verbatim = "creep rupture life".into();
        store.record_term_binding(&pending).await.unwrap();
        store
            .record_term_binding(&bound(
                TERM_BINDING_RUNG_EXACT,
                "https://example.test/A",
                None,
            ))
            .await
            .unwrap();

        let unresolved = store.unbound_term_bindings("local").await.unwrap();
        assert_eq!(unresolved.len(), 1, "{unresolved:?}");
        assert_eq!(unresolved[0].term, "creep rupture life");
        assert_eq!(unresolved[0].score, Some(0.3));
        assert_eq!(
            unresolved[0].proposal_item_id.as_deref(),
            Some("class|'creep'|parents=[]")
        );
    }

    /// Nearest-neighbour scoping: only the requested ontology ids and the
    /// requested model partition may answer, whatever else is stored — the
    /// SQL-level half of "the union of LOADED ontologies is consulted".
    #[tokio::test]
    async fn nearest_class_labels_scope_by_ontology_and_model() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        let entry = |iri: &str, label: &str, vector: Vec<f32>| ClassLabelEmbedding {
            class_iri: iri.into(),
            label: label.into(),
            vector,
        };
        store
            .store_class_label_embeddings(
                "emmo",
                "test:mock",
                &[entry(
                    "https://example.test/Prop",
                    "property",
                    vec![1.0, 0.0, 0.0],
                )],
            )
            .await
            .unwrap();
        // A NEARER vector in an ontology that is NOT loaded for this run.
        store
            .store_class_label_embeddings(
                "foreign",
                "test:mock",
                &[entry(
                    "https://example.test/Foreign",
                    "property",
                    vec![0.999, 0.01, 0.0],
                )],
            )
            .await
            .unwrap();
        // A NEARER vector under a DIFFERENT model id.
        store
            .store_class_label_embeddings(
                "emmo",
                "test:other-model",
                &[entry(
                    "https://example.test/Other",
                    "property",
                    vec![0.999, 0.01, 0.0],
                )],
            )
            .await
            .unwrap();

        let probe = [0.999_f32, 0.01, 0.0];
        let neighbors = store
            .nearest_class_labels(&probe, &["emmo"], "test:mock", 5)
            .await
            .unwrap();
        assert_eq!(neighbors.len(), 1, "{neighbors:?}");
        assert_eq!(neighbors[0].class_iri, "https://example.test/Prop");
        assert_eq!(neighbors[0].ontology_id, "emmo");
        assert!(
            neighbors[0].similarity > 0.99,
            "cosine similarity survived the round trip: {neighbors:?}"
        );

        // Widening the requested set widens the answer.
        let neighbors = store
            .nearest_class_labels(&probe, &["emmo", "foreign"], "test:mock", 1)
            .await
            .unwrap();
        assert_eq!(neighbors[0].class_iri, "https://example.test/Foreign");
    }

    /// The seeding diff surface answers exactly what is stored per partition.
    #[tokio::test]
    async fn class_label_embedding_keys_answer_per_partition() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        store
            .store_class_label_embeddings(
                "emmo",
                "test:mock",
                &[ClassLabelEmbedding {
                    class_iri: "https://example.test/A".into(),
                    label: "alpha".into(),
                    vector: vec![1.0, 0.0],
                }],
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .class_label_embedding_keys("emmo", "test:mock")
                .await
                .unwrap(),
            vec![("https://example.test/A".to_string(), "alpha".to_string())]
        );
        assert!(
            store
                .class_label_embedding_keys("emmo", "test:absent")
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// The graph stamp: fills ONLY rows whose class is unknown, joins by
    /// canonical name within the tenant, and never overwrites a declared
    /// classification.
    #[tokio::test]
    async fn entity_stamp_fills_null_class_iri_only() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        store
            .write_extracted_entity("Yield Strength", "Entity", None, "local")
            .await
            .unwrap();
        store
            .write_classified_entity(
                "hardness",
                crate::ClassifiedNode {
                    entity_type: "Property",
                    storage_label: "Property",
                    class_iri: "https://example.test/Declared",
                },
                None,
                "local",
            )
            .await
            .unwrap();
        // Same name in ANOTHER tenant must stay untouched.
        store
            .write_extracted_entity("Yield Strength", "Entity", None, "other-tenant")
            .await
            .unwrap();

        let stamped = store
            .apply_term_binding_to_entities("local", "yield strength", "https://example.test/YS")
            .await
            .unwrap();
        assert_eq!(stamped, 1, "exactly the one NULL row in the tenant");

        let untouched = store
            .apply_term_binding_to_entities("local", "hardness", "https://example.test/Wrong")
            .await
            .unwrap();
        assert_eq!(
            untouched, 0,
            "a declared classification is never overwritten"
        );
    }
}
