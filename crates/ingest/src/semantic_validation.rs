//! Advisory geometry checks for proposed graph writes.
//!
//! Structural validation remains authoritative. These checks measure a
//! proposal against the existing embedding geometry and return an auditable
//! report; they never edit, merge, drop, or block an entity or fact.

use std::collections::{HashMap, HashSet};

use prism_embed::{EmbedBackend, cosine_similarity};
use prism_provenance::{
    ClassRegionDistance, EntityGeometryProbe, LocalFact, ProvenanceStore, TripleGeometryProbe,
};
use serde::{Deserialize, Serialize};

/// Policy for all advisory semantic checks.
///
/// The defaults are conservative operational starting points for BGE-small,
/// not universal truth thresholds or calibrated probabilities. PRISM has no
/// labeled materials corpus that would justify presenting them as decisive.
/// Every finding therefore remains report-only and carries the measurements
/// needed to recalibrate the policy against a deployment's own corpus.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SemanticValidationPolicy {
    pub near_duplicate: NearDuplicatePolicy,
    pub typing: TypingPolicy,
    pub triple_plausibility: TriplePlausibilityPolicy,
}

impl SemanticValidationPolicy {
    /// Validate every configured boundary before any model or database work.
    pub fn validate(&self) -> Result<(), String> {
        self.near_duplicate.validate()?;
        self.typing.validate()?;
        self.triple_plausibility.validate()?;
        Ok(())
    }

    fn any_enabled(&self) -> bool {
        self.near_duplicate.enabled || self.typing.enabled || self.triple_plausibility.enabled
    }
}

/// Policy for entity/class identity collisions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NearDuplicatePolicy {
    /// Turn this check on. Disabled checks are reported as `Disabled`, never
    /// as a successful validation.
    pub enabled: bool,
    /// Largest cosine distance considered geometrically close. `0.08`
    /// corresponds to cosine similarity `0.92`; paired with the lexical gate
    /// below, it intentionally reports only very close aliases by default.
    pub maximum_cosine_distance: f32,
    /// Largest normalized character edit distance considered a trivial name
    /// variation. `0.20` permits one edit in a five-character token while
    /// rejecting broad semantic similarity on its own.
    pub maximum_normalized_edit_distance: f32,
    /// Maximum stored neighbors returned per proposal by the batched SQL
    /// scan. `8` bounds result volume; exact alphanumeric lexical matches are
    /// ranked first. When the compatible graph is larger, a clean result is
    /// conservatively `Unavailable` because non-exact edit variants may lie
    /// outside this bounded candidate set.
    pub maximum_neighbors_per_entity: usize,
    /// Maximum collision examples retained. `20` keeps the serialized report
    /// bounded; candidate/evaluated counts remain complete.
    pub maximum_findings: usize,
}

impl Default for NearDuplicatePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            maximum_cosine_distance: 0.08,
            maximum_normalized_edit_distance: 0.20,
            maximum_neighbors_per_entity: 8,
            maximum_findings: 20,
        }
    }
}

impl NearDuplicatePolicy {
    fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        validate_distance(
            "near_duplicate.maximum_cosine_distance",
            self.maximum_cosine_distance,
        )?;
        validate_unit_interval(
            "near_duplicate.maximum_normalized_edit_distance",
            self.maximum_normalized_edit_distance,
        )?;
        validate_positive(
            "near_duplicate.maximum_neighbors_per_entity",
            self.maximum_neighbors_per_entity,
        )?;
        validate_positive("near_duplicate.maximum_findings", self.maximum_findings)
    }
}

/// Policy for checking an instance against graph-derived class regions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TypingPolicy {
    /// Turn this check on. It remains advisory even when enabled.
    pub enabled: bool,
    /// Number of nearest exemplars averaged for each class region. `3`
    /// reduces dependence on one possibly mislabeled graph node while
    /// keeping sparse local graphs usable.
    pub neighbors_per_class: usize,
    /// Smallest class population accepted as a region. `3` means a lone
    /// example can never make a typing judgement look validated.
    pub minimum_class_examples: usize,
    /// Assigned-class mean cosine distance that must be exceeded before an
    /// outlier can be reported. `0.35` is deliberately broad; it is an
    /// auditable alert boundary, not a learned class separator.
    pub minimum_assigned_class_distance: f32,
    /// Largest mean distance allowed for the proposed nearer class. `0.20`
    /// requires a materially close alternative rather than merely the least
    /// distant bad choice.
    pub maximum_nearer_class_distance: f32,
    /// Minimum distance advantage the nearer class must have over the
    /// assigned class. `0.15` prevents small ranking noise from producing a
    /// mistyping report.
    pub minimum_distance_advantage: f32,
    /// Maximum mistyping examples retained. `20` bounds output only.
    pub maximum_findings: usize,
}

impl Default for TypingPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            neighbors_per_class: 3,
            minimum_class_examples: 3,
            minimum_assigned_class_distance: 0.35,
            maximum_nearer_class_distance: 0.20,
            minimum_distance_advantage: 0.15,
            maximum_findings: 20,
        }
    }
}

impl TypingPolicy {
    fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        validate_positive("typing.neighbors_per_class", self.neighbors_per_class)?;
        validate_positive("typing.minimum_class_examples", self.minimum_class_examples)?;
        validate_distance(
            "typing.minimum_assigned_class_distance",
            self.minimum_assigned_class_distance,
        )?;
        validate_distance(
            "typing.maximum_nearer_class_distance",
            self.maximum_nearer_class_distance,
        )?;
        validate_distance(
            "typing.minimum_distance_advantage",
            self.minimum_distance_advantage,
        )?;
        validate_positive("typing.maximum_findings", self.maximum_findings)
    }
}

/// Policy for fusing extractor confidence with a graph-derived triple prior.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TriplePlausibilityPolicy {
    /// Turn this check on. A finding never changes the proposed triple.
    pub enabled: bool,
    /// Largest cosine distance accepted for EACH endpoint of comparable
    /// graph evidence. `0.20` prevents one exact endpoint from hiding a
    /// distant other endpoint.
    pub maximum_neighbor_distance: f32,
    /// Maximum graph priors retained per proposed triple. `8` bounds Turso
    /// result volume for common predicates at million-paper scale while
    /// still allowing a small neighborhood consensus.
    pub maximum_neighbors_per_triple: usize,
    /// Smallest compatible prior neighborhood used for a judgement. `3`
    /// prevents one noisy stored assertion from masquerading as consensus.
    pub minimum_prior_examples: usize,
    /// Fact kinds eligible for the numeric prior. The default restricts the
    /// check to unconditioned composition fractions (`composition` and
    /// `contains`); measurement conditions are not represented in
    /// `LocalFact`, so comparing them would be unreliable.
    pub eligible_fact_kinds: Vec<String>,
    /// Relative numeric deviation that reduces value agreement to zero.
    /// `0.25` is intentionally tolerant of reporting/rounding variation; it
    /// still makes a swapped `0.90` versus `0.06` composition unambiguous.
    pub numeric_relative_tolerance: f64,
    /// Denominator floor for relative numeric error. `0.01` prevents a
    /// near-zero stored value from turning floating noise into infinity.
    pub numeric_scale_floor: f64,
    /// Weight assigned to extractor confidence in the fused score. `0.60`
    /// leaves geometry influential without allowing it to replace what the
    /// extractor actually reported.
    pub extractor_confidence_weight: f64,
    /// Honest fallback when a proposal has no confidence. `0.50` is neutral,
    /// not a claim that an unscored extractor is 80% accurate.
    pub fallback_extractor_confidence: f64,
    /// Honest fallback for a legacy graph assertion with no confidence.
    /// `0.50` prevents missing evidence metadata from posing as certainty.
    pub fallback_graph_confidence: f64,
    /// Fused score below which the triple is reported. `0.60` is an
    /// operational review boundary only; the report names every component
    /// and must not be rendered as a calibrated probability.
    pub minimum_combined_score: f64,
    /// Maximum triple examples retained. `20` bounds output only.
    pub maximum_findings: usize,
}

impl Default for TriplePlausibilityPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            maximum_neighbor_distance: 0.20,
            maximum_neighbors_per_triple: 8,
            minimum_prior_examples: 3,
            eligible_fact_kinds: vec!["composition".into(), "contains".into()],
            numeric_relative_tolerance: 0.25,
            numeric_scale_floor: 0.01,
            extractor_confidence_weight: 0.60,
            fallback_extractor_confidence: 0.50,
            fallback_graph_confidence: 0.50,
            minimum_combined_score: 0.60,
            maximum_findings: 20,
        }
    }
}

impl TriplePlausibilityPolicy {
    fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        validate_distance(
            "triple_plausibility.maximum_neighbor_distance",
            self.maximum_neighbor_distance,
        )?;
        validate_positive(
            "triple_plausibility.maximum_neighbors_per_triple",
            self.maximum_neighbors_per_triple,
        )?;
        validate_positive(
            "triple_plausibility.minimum_prior_examples",
            self.minimum_prior_examples,
        )?;
        if self.minimum_prior_examples > self.maximum_neighbors_per_triple {
            return Err(format!(
                "triple_plausibility.minimum_prior_examples ({}) cannot exceed maximum_neighbors_per_triple ({})",
                self.minimum_prior_examples, self.maximum_neighbors_per_triple
            ));
        }
        if self.eligible_fact_kinds.is_empty()
            || self
                .eligible_fact_kinds
                .iter()
                .any(|kind| kind.trim().is_empty())
        {
            return Err(
                "triple_plausibility.eligible_fact_kinds must contain non-empty kinds".to_string(),
            );
        }
        if !self.numeric_relative_tolerance.is_finite() || self.numeric_relative_tolerance <= 0.0 {
            return Err(format!(
                "triple_plausibility.numeric_relative_tolerance must be finite and positive, got {}",
                self.numeric_relative_tolerance
            ));
        }
        if !self.numeric_scale_floor.is_finite() || self.numeric_scale_floor <= 0.0 {
            return Err(format!(
                "triple_plausibility.numeric_scale_floor must be finite and positive, got {}",
                self.numeric_scale_floor
            ));
        }
        validate_probability(
            "triple_plausibility.extractor_confidence_weight",
            self.extractor_confidence_weight,
        )?;
        validate_probability(
            "triple_plausibility.fallback_extractor_confidence",
            self.fallback_extractor_confidence,
        )?;
        validate_probability(
            "triple_plausibility.fallback_graph_confidence",
            self.fallback_graph_confidence,
        )?;
        validate_probability(
            "triple_plausibility.minimum_combined_score",
            self.minimum_combined_score,
        )?;
        validate_positive(
            "triple_plausibility.maximum_findings",
            self.maximum_findings,
        )
    }
}

fn validate_distance(name: &str, value: f32) -> Result<(), String> {
    if value.is_finite() && (0.0..=2.0).contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "{name} must be finite and within [0, 2], got {value}"
        ))
    }
}

fn validate_unit_interval(name: &str, value: f32) -> Result<(), String> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "{name} must be finite and within [0, 1], got {value}"
        ))
    }
}

fn validate_probability(name: &str, value: f64) -> Result<(), String> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "{name} must be finite and within [0, 1], got {value}"
        ))
    }
}

fn validate_positive(name: &str, value: usize) -> Result<(), String> {
    if value > 0 {
        Ok(())
    } else {
        Err(format!("{name} must be at least 1"))
    }
}

/// Whether one semantic check ran over every candidate it claims to cover.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticValidationStatus {
    /// The declared policy disabled this check.
    #[default]
    Disabled,
    /// Every candidate was measured against compatible graph geometry.
    Applied,
    /// Compatible embeddings or enough graph exemplars were absent.
    Unavailable,
    /// Backend initialization, embedding, vector validation, or SQL failed.
    Failed,
}

impl SemanticValidationStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Applied => "applied",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
        }
    }
}

/// Honest accounting shared by the three checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SemanticCheckReport<T> {
    pub status: SemanticValidationStatus,
    pub candidates: usize,
    pub evaluated: usize,
    /// `Some` only when `status == Applied`; unavailable work cannot pose as
    /// a passed validation.
    pub passed: Option<bool>,
    #[serde(default)]
    pub findings: Vec<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Whether a collision was found inside this proposal or against the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionSource {
    Proposal,
    StoredGraph,
}

/// One geometrically close, lexically trivial identity collision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NearDuplicateFinding {
    pub proposed_name: String,
    pub proposed_type: String,
    pub proposed_storage_label: String,
    pub colliding_name: String,
    pub colliding_type: String,
    pub colliding_storage_label: String,
    pub cosine_distance: f32,
    pub normalized_edit_distance: f32,
    pub source: CollisionSource,
}

/// One instance lying far from its assigned class and close to another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypingFinding {
    pub name: String,
    pub assigned_class: String,
    pub assigned_class_iri: String,
    pub assigned_distance: f32,
    pub assigned_examples: usize,
    pub nearer_class: String,
    pub nearer_class_iri: String,
    pub nearer_distance: f32,
    pub nearer_examples: usize,
    pub distance_advantage: f32,
}

/// One proposed triple whose confidence and graph prior fuse below policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriplePlausibilityFinding {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub proposed_value: Option<f64>,
    pub extractor_confidence: f64,
    pub extractor_confidence_was_fallback: bool,
    pub graph_prior: f64,
    pub combined_score: f64,
    pub prior_examples: usize,
    pub evidence: Vec<TriplePriorEvidence>,
}

/// Every component contributed by one neighboring stored assertion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriplePriorEvidence {
    pub subject: String,
    pub object: String,
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub stored_confidence: f64,
    pub stored_confidence_was_fallback: bool,
    pub subject_distance: f32,
    pub object_distance: f32,
    pub geometric_similarity: f64,
    pub numeric_agreement: f64,
    pub prior_contribution: f64,
}

/// Semantic measurements for one proposed write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticValidationReport {
    /// Exact policy snapshot used for this judgement. Distances without the
    /// thresholds that interpreted them are not auditable evidence.
    pub policy: SemanticValidationPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    pub near_duplicates: SemanticCheckReport<NearDuplicateFinding>,
    pub typing: SemanticCheckReport<TypingFinding>,
    pub triple_plausibility: SemanticCheckReport<TriplePlausibilityFinding>,
}

/// One raw ontology label retained before the induction builder performs its
/// existing deterministic lexical merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OntologyLabelProposal {
    pub label: String,
    /// `class` or `relation`; kept in findings so cross-kind collisions are
    /// visible rather than silently interpreted as the same identity.
    pub kind: String,
}

/// Persistable semantic report for an induced ontology artifact.
///
/// Class-region typing and triple plausibility require instance/assertion
/// geometry, which an ontology-label batch does not contain. This report
/// therefore applies only the declared near-duplicate policy instead of
/// inventing evidence from unrelated graph-instance vectors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OntologySemanticValidationReport {
    pub policy: NearDuplicatePolicy,
    /// Raw model surfaces retained before the induction builder's lexical
    /// normalization. Keeping them in the artifact makes any collision (or
    /// unavailable judgement) independently auditable after the write.
    #[serde(default)]
    pub proposals: Vec<OntologyLabelProposal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    pub near_duplicates: SemanticCheckReport<NearDuplicateFinding>,
}

impl Default for OntologySemanticValidationReport {
    fn default() -> Self {
        Self {
            policy: NearDuplicatePolicy::default(),
            proposals: Vec::new(),
            backend: None,
            near_duplicates: unfinished_check(
                0,
                SemanticValidationStatus::Unavailable,
                "legacy ontology artifact carries no semantic validation report".to_string(),
            ),
        }
    }
}

/// One proposed entity/class at the semantic boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticEntityProposal {
    pub name: String,
    pub entity_type: String,
    pub storage_label: String,
    pub class_iri: Option<String>,
}

/// Report plus the exact one-pass vectors that can be stored after the graph
/// write. The vectors are deliberately not serialized into ingest output.
#[derive(Debug, Clone)]
pub struct SemanticValidationBatch {
    pub report: SemanticValidationReport,
    embedding_model: Option<String>,
    embedding_names: Vec<String>,
    embedding_vectors: Vec<Vec<f32>>,
}

/// Check raw model-proposed ontology labels before the builder's established
/// lexical normalization/merge. The whole ontology costs one embedding batch
/// and O(L²) in-memory comparisons for L raw labels; no graph instance vectors
/// are consulted because they are not a reliable class-label prior.
pub async fn validate_ontology_labels_best_effort(
    labels: &[OntologyLabelProposal],
    policy: &NearDuplicatePolicy,
) -> OntologySemanticValidationReport {
    if !policy.enabled || policy.validate().is_err() {
        return validate_ontology_labels_with_backend(labels, policy, None).await;
    }
    let backend = match tokio::task::spawn_blocking(prism_embed::from_config).await {
        Ok(Some(backend)) => backend,
        Ok(None) => {
            return validate_ontology_labels_with_backend(labels, policy, None).await;
        }
        Err(error) => {
            let mut report = ontology_label_report(policy, labels);
            report.near_duplicates = unfinished_check(
                labels.len(),
                SemanticValidationStatus::Failed,
                format!(
                    "embedding backend initialization task failed ({error}); the ontology artifact is unvalidated"
                ),
            );
            return report;
        }
    };
    validate_ontology_labels_with_backend(labels, policy, Some(backend.as_ref())).await
}

/// Deterministic backend-injection seam for ontology semantic tests and
/// library callers that already own an embedding model.
pub async fn validate_ontology_labels_with_backend(
    labels: &[OntologyLabelProposal],
    policy: &NearDuplicatePolicy,
    backend: Option<&dyn EmbedBackend>,
) -> OntologySemanticValidationReport {
    let mut report = ontology_label_report(policy, labels);
    if let Err(error) = policy.validate() {
        report.near_duplicates = unfinished_check(
            labels.len(),
            SemanticValidationStatus::Failed,
            format!("ontology near-duplicate policy is invalid ({error})"),
        );
        return report;
    }
    if !policy.enabled {
        return report;
    }
    let Some(backend) = backend else {
        report.near_duplicates = unfinished_check(
            labels.len(),
            SemanticValidationStatus::Unavailable,
            "no embedding backend was available; the ontology artifact is unvalidated".to_string(),
        );
        return report;
    };
    if backend.id().trim().is_empty() {
        report.near_duplicates = unfinished_check(
            labels.len(),
            SemanticValidationStatus::Failed,
            "embedding backend returned an empty model id; ontology vectors cannot be attributed"
                .to_string(),
        );
        return report;
    }
    report.backend = Some(backend.id().to_string());
    let names: Vec<String> = labels
        .iter()
        .map(|proposal| proposal.label.clone())
        .collect();
    let vectors = match backend.embed(&names).await {
        Ok(vectors) => vectors,
        Err(error) => {
            report.near_duplicates = unfinished_check(
                labels.len(),
                SemanticValidationStatus::Failed,
                format!(
                    "embedding ontology labels with '{}' failed ({error:#})",
                    backend.id()
                ),
            );
            return report;
        }
    };
    if let Err(error) = validate_vectors(&names, &vectors) {
        report.near_duplicates = unfinished_check(
            labels.len(),
            SemanticValidationStatus::Failed,
            format!(
                "embedding backend '{}' returned unusable ontology vectors ({error})",
                backend.id()
            ),
        );
        return report;
    }
    let returned_dimensions = vectors.first().map_or(0, Vec::len);
    if !names.is_empty() && backend.dimensions() != 0 && returned_dimensions != backend.dimensions()
    {
        report.near_duplicates = unfinished_check(
            labels.len(),
            SemanticValidationStatus::Failed,
            format!(
                "embedding backend '{}' declared {} dimensions but returned {returned_dimensions}",
                backend.id(),
                backend.dimensions()
            ),
        );
        return report;
    }

    let mut findings = Vec::new();
    for left in 0..labels.len() {
        for right in (left + 1)..labels.len() {
            let a = &labels[left];
            let b = &labels[right];
            if a.label == b.label && a.kind == b.kind {
                continue;
            }
            let cosine_distance =
                (1.0 - cosine_similarity(&vectors[left], &vectors[right])).clamp(0.0, 2.0);
            let edit_distance = normalized_edit_distance(&a.label, &b.label);
            if cosine_distance <= policy.maximum_cosine_distance
                && edit_distance <= policy.maximum_normalized_edit_distance
            {
                findings.push(NearDuplicateFinding {
                    proposed_name: a.label.clone(),
                    proposed_type: a.kind.clone(),
                    proposed_storage_label: a.kind.clone(),
                    colliding_name: b.label.clone(),
                    colliding_type: b.kind.clone(),
                    colliding_storage_label: b.kind.clone(),
                    cosine_distance,
                    normalized_edit_distance: edit_distance,
                    source: CollisionSource::Proposal,
                });
            }
        }
    }
    findings.sort_by(|a, b| a.cosine_distance.total_cmp(&b.cosine_distance));
    findings.truncate(policy.maximum_findings);
    report.near_duplicates = applied_check(labels.len(), labels.len(), findings);
    report
}

fn ontology_label_report(
    policy: &NearDuplicatePolicy,
    labels: &[OntologyLabelProposal],
) -> OntologySemanticValidationReport {
    OntologySemanticValidationReport {
        policy: policy.clone(),
        proposals: labels.to_vec(),
        backend: None,
        near_duplicates: initial_check(policy.enabled, labels.len()),
    }
}

impl SemanticValidationBatch {
    #[must_use]
    pub fn embedding_model(&self) -> Option<&str> {
        self.embedding_model.as_deref()
    }

    #[must_use]
    pub fn embedding_names(&self) -> &[String] {
        &self.embedding_names
    }

    #[must_use]
    pub fn embedding_vectors(&self) -> &[Vec<f32>] {
        &self.embedding_vectors
    }
}

/// Initialize the configured local embedding backend and validate a proposed
/// write. Backend absence/failure is data in the returned report, never an
/// ingest error.
pub async fn validate_write_best_effort(
    store: &ProvenanceStore,
    entities: &[SemanticEntityProposal],
    facts: &[LocalFact],
    tenant: &str,
    policy: &SemanticValidationPolicy,
) -> SemanticValidationBatch {
    let policy_error = policy.validate().err();
    let backend = match tokio::task::spawn_blocking(prism_embed::from_config).await {
        Ok(Some(backend)) => backend,
        Ok(None) => {
            return report_without_backend(
                entities,
                facts,
                policy,
                if policy_error.is_some() {
                    SemanticValidationStatus::Failed
                } else {
                    SemanticValidationStatus::Unavailable
                },
                policy_error.map_or_else(
                    || {
                        "no embedding backend was available; the graph write is unvalidated"
                            .to_string()
                    },
                    |error| format!("semantic validation policy is invalid ({error})"),
                ),
            );
        }
        Err(error) => {
            let (status, message) = policy_error.map_or_else(
                || {
                    (
                        SemanticValidationStatus::Failed,
                        format!(
                            "embedding backend initialization task failed ({error}); the graph write is unvalidated"
                        ),
                    )
                },
                |policy_error| {
                    (
                        SemanticValidationStatus::Failed,
                        format!("semantic validation policy is invalid ({policy_error})"),
                    )
                },
            );
            return report_without_backend(entities, facts, policy, status, message);
        }
    };
    validate_write_with_backend(
        store,
        entities,
        facts,
        tenant,
        policy,
        Some(backend.as_ref()),
    )
    .await
}

/// Validate with an explicitly supplied backend. `None` is a deterministic
/// test/library seam for an unavailable embedding model.
pub async fn validate_write_with_backend(
    store: &ProvenanceStore,
    entities: &[SemanticEntityProposal],
    facts: &[LocalFact],
    tenant: &str,
    policy: &SemanticValidationPolicy,
    backend: Option<&dyn EmbedBackend>,
) -> SemanticValidationBatch {
    let policy_error = policy.validate().err();
    let Some(backend) = backend else {
        return report_without_backend(
            entities,
            facts,
            policy,
            if policy_error.is_some() {
                SemanticValidationStatus::Failed
            } else {
                SemanticValidationStatus::Unavailable
            },
            policy_error.map_or_else(
                || "no embedding backend was available; the graph write is unvalidated".to_string(),
                |error| format!("semantic validation policy is invalid ({error})"),
            ),
        );
    };
    if backend.id().trim().is_empty() {
        return report_without_backend(
            entities,
            facts,
            policy,
            SemanticValidationStatus::Failed,
            "embedding backend returned an empty model id; vectors cannot be attributed"
                .to_string(),
        );
    }

    let (embedding_names, name_to_index) = distinct_embedding_names(entities, facts);
    // Cost per document: one `embed` call covers every distinct endpoint in
    // this extraction batch. A document split into B extraction batches costs
    // B backend initializations and B model calls, plus two metadata queries
    // and at most three batched Turso scans per batch — never one model/SQL
    // round trip per name. Turso 0.7 has no vector index, so batching removes
    // round-trip amplification but database compute remains
    // O(proposals × stored vectors); callers processing many documents should
    // retain a backend and use `validate_write_with_backend`.
    let embedding_vectors = match backend.embed(&embedding_names).await {
        Ok(vectors) => vectors,
        Err(error) => {
            return report_without_backend(
                entities,
                facts,
                policy,
                SemanticValidationStatus::Failed,
                format!(
                    "embedding proposals with '{}' failed ({error:#}); the graph write is unvalidated",
                    backend.id()
                ),
            );
        }
    };
    if let Err(error) = validate_vectors(&embedding_names, &embedding_vectors) {
        return report_without_backend(
            entities,
            facts,
            policy,
            SemanticValidationStatus::Failed,
            format!(
                "embedding backend '{}' returned unusable vectors ({error}); the graph write is unvalidated",
                backend.id()
            ),
        );
    }
    let returned_dimensions = embedding_vectors.first().map_or(0, Vec::len);
    if !embedding_names.is_empty()
        && backend.dimensions() != 0
        && returned_dimensions != backend.dimensions()
    {
        return report_without_backend(
            entities,
            facts,
            policy,
            SemanticValidationStatus::Failed,
            format!(
                "embedding backend '{}' declared {} dimensions but returned {returned_dimensions}",
                backend.id(),
                backend.dimensions()
            ),
        );
    }

    let mut batch = SemanticValidationBatch {
        report: requested_report(entities, facts, policy, Some(backend.id().to_string())),
        embedding_model: Some(backend.id().to_string()),
        embedding_names,
        embedding_vectors,
    };
    if let Some(error) = policy_error {
        mark_requested(
            &mut batch.report,
            policy,
            SemanticValidationStatus::Failed,
            format!("semantic validation policy is invalid ({error})"),
        );
        return batch;
    }
    // Disabling advisory checks must not disable the pre-existing semantic
    // search index. We still prepare the one model batch for post-write
    // storage, but skip every validation query and report `Disabled`.
    if !policy.any_enabled() {
        return batch;
    }
    let dimensions = batch.embedding_vectors.first().map_or(0, Vec::len);
    let partitions = match store.entity_embedding_partitions(tenant).await {
        Ok(partitions) => partitions,
        Err(error) => {
            mark_requested(
                &mut batch.report,
                policy,
                SemanticValidationStatus::Failed,
                format!("reading the entity embedding inventory failed ({error:#})"),
            );
            return batch;
        }
    };
    let matching: Vec<_> = partitions
        .iter()
        .filter(|partition| partition.model.as_deref() == Some(backend.id()))
        .collect();
    let missing_partition_message = matching.is_empty().then(|| {
        if partitions.is_empty() {
            "the graph contains no entity embeddings; this write is unvalidated".to_string()
        } else {
            format!(
                "the graph contains no embeddings from '{}'; legacy or other-model vectors were not compared",
                backend.id()
            )
        }
    });
    if !matching.is_empty()
        && matching
            .iter()
            .any(|partition| partition.dimensions != dimensions)
    {
        mark_requested(
            &mut batch.report,
            policy,
            SemanticValidationStatus::Failed,
            format!(
                "the graph has '{}' embeddings with dimensions incompatible with the proposal's {dimensions}",
                backend.id()
            ),
        );
        return batch;
    }
    let coverage = match store
        .entity_geometry_coverage(tenant, backend.id(), dimensions)
        .await
    {
        Ok(coverage) => coverage,
        Err(error) => {
            mark_requested(
                &mut batch.report,
                policy,
                SemanticValidationStatus::Failed,
                format!("reading entity embedding coverage failed ({error:#})"),
            );
            return batch;
        }
    };

    let vectors_by_name = |name: &str| {
        name_to_index
            .get(&canonical_name(name))
            .and_then(|index| batch.embedding_vectors.get(*index))
    };
    if policy.near_duplicate.enabled {
        batch.report.near_duplicates = check_near_duplicates(
            store,
            entities,
            tenant,
            backend.id(),
            &vectors_by_name,
            &policy.near_duplicate,
        )
        .await;
    }
    if policy.typing.enabled {
        batch.report.typing = check_typing(
            store,
            entities,
            tenant,
            backend.id(),
            &vectors_by_name,
            &policy.typing,
        )
        .await;
    }
    if policy.triple_plausibility.enabled {
        batch.report.triple_plausibility = check_triples(
            store,
            facts,
            tenant,
            backend.id(),
            &vectors_by_name,
            &policy.triple_plausibility,
        )
        .await;
    }
    // Proposal-vs-proposal collisions are still useful when the existing
    // graph has no compatible partition, so checks run before this downgrade
    // and retain any findings. The overall graph-backed judgement remains
    // honestly `Unavailable` and can never claim a pass.
    if let Some(message) = missing_partition_message {
        mark_partial_coverage(&mut batch.report, policy, &message);
    } else if coverage.compatible_embeddings < coverage.entities {
        let message = format!(
            "only {}/{} stored graph entities have compatible '{}' vectors; partial geometry cannot pose as complete validation",
            coverage.compatible_embeddings,
            coverage.entities,
            backend.id()
        );
        mark_partial_coverage(&mut batch.report, policy, &message);
    }
    if policy.near_duplicate.enabled
        && coverage.compatible_embeddings > policy.near_duplicate.maximum_neighbors_per_entity
    {
        mark_unavailable(
            &mut batch.report.near_duplicates,
            &format!(
                "near-duplicate retrieval is bounded to {} neighbors across {} compatible entities; findings are valid but a clean result is not exhaustive",
                policy.near_duplicate.maximum_neighbors_per_entity, coverage.compatible_embeddings
            ),
        );
    }
    batch
}

fn mark_unavailable<T>(check: &mut SemanticCheckReport<T>, message: &str) {
    if check.status == SemanticValidationStatus::Applied {
        check.status = SemanticValidationStatus::Unavailable;
        check.passed = None;
    }
    if check.status == SemanticValidationStatus::Unavailable {
        match &mut check.message {
            Some(existing) => {
                existing.push_str("; ");
                existing.push_str(message);
            }
            None => check.message = Some(message.to_string()),
        }
    }
}

fn mark_partial_coverage(
    report: &mut SemanticValidationReport,
    policy: &SemanticValidationPolicy,
    message: &str,
) {
    if policy.near_duplicate.enabled {
        mark_unavailable(&mut report.near_duplicates, message);
    }
    if policy.typing.enabled {
        mark_unavailable(&mut report.typing, message);
    }
    if policy.triple_plausibility.enabled {
        mark_unavailable(&mut report.triple_plausibility, message);
    }
}

async fn check_near_duplicates<'a>(
    store: &ProvenanceStore,
    entities: &[SemanticEntityProposal],
    tenant: &str,
    model: &str,
    vector_for: &impl Fn(&str) -> Option<&'a Vec<f32>>,
    policy: &NearDuplicatePolicy,
) -> SemanticCheckReport<NearDuplicateFinding> {
    let probes: Vec<_> = entities
        .iter()
        .enumerate()
        .filter_map(|(probe_id, entity)| {
            vector_for(&entity.name).map(|vector| EntityGeometryProbe {
                probe_id,
                name: entity.name.clone(),
                storage_label: Some(entity.storage_label.clone()),
                vector: vector.clone(),
            })
        })
        .collect();
    let stored = match store
        .entity_geometry_neighbors(
            &probes,
            tenant,
            model,
            f64::from(policy.maximum_cosine_distance),
            policy.maximum_neighbors_per_entity,
        )
        .await
    {
        Ok(neighbors) => neighbors,
        Err(error) => {
            return unfinished_check(
                entities.len(),
                SemanticValidationStatus::Failed,
                format!("near-duplicate geometry query failed ({error:#})"),
            );
        }
    };

    let mut findings = Vec::new();
    let mut seen = HashSet::new();
    for left in 0..entities.len() {
        for right in (left + 1)..entities.len() {
            let a = &entities[left];
            let b = &entities[right];
            if same_identity(a, b) {
                continue;
            }
            let Some((av, bv)) = vector_for(&a.name).zip(vector_for(&b.name)) else {
                continue;
            };
            let cosine_distance = (1.0 - cosine_similarity(av, bv)).clamp(0.0, 2.0);
            let edit_distance = normalized_edit_distance(&a.name, &b.name);
            if cosine_distance <= policy.maximum_cosine_distance
                && edit_distance <= policy.maximum_normalized_edit_distance
            {
                findings.push(NearDuplicateFinding {
                    proposed_name: a.name.clone(),
                    proposed_type: a.entity_type.clone(),
                    proposed_storage_label: a.storage_label.clone(),
                    colliding_name: b.name.clone(),
                    colliding_type: b.entity_type.clone(),
                    colliding_storage_label: b.storage_label.clone(),
                    cosine_distance,
                    normalized_edit_distance: edit_distance,
                    source: CollisionSource::Proposal,
                });
            }
        }
    }
    for neighbor in stored {
        let Some(proposed) = entities.get(neighbor.probe_id) else {
            continue;
        };
        let same_stored_identity = proposed.name == neighbor.name
            && proposed.storage_label == neighbor.storage_label
            && proposed.class_iri.as_deref() == neighbor.class_iri.as_deref()
            && Some(proposed.entity_type.as_str()) == neighbor.entity_type.as_deref();
        if same_stored_identity {
            continue;
        }
        let edit_distance = normalized_edit_distance(&proposed.name, &neighbor.name);
        if edit_distance > policy.maximum_normalized_edit_distance {
            continue;
        }
        let key = format!(
            "{}\u{0}{}\u{0}{}\u{0}{}",
            proposed.name,
            proposed.entity_type,
            neighbor.name,
            neighbor.entity_type.as_deref().unwrap_or("unclassified")
        );
        if seen.insert(key) {
            findings.push(NearDuplicateFinding {
                proposed_name: proposed.name.clone(),
                proposed_type: proposed.entity_type.clone(),
                proposed_storage_label: proposed.storage_label.clone(),
                colliding_name: neighbor.name,
                colliding_type: neighbor
                    .entity_type
                    .unwrap_or_else(|| "unclassified".to_string()),
                colliding_storage_label: neighbor.storage_label,
                cosine_distance: neighbor.distance as f32,
                normalized_edit_distance: edit_distance,
                source: CollisionSource::StoredGraph,
            });
        }
    }
    findings.sort_by(|a, b| a.cosine_distance.total_cmp(&b.cosine_distance));
    findings.truncate(policy.maximum_findings);
    let evaluated = entities
        .iter()
        .filter(|entity| vector_for(&entity.name).is_some())
        .count();
    if evaluated == entities.len() {
        applied_check(entities.len(), entities.len(), findings)
    } else {
        SemanticCheckReport {
            status: SemanticValidationStatus::Unavailable,
            candidates: entities.len(),
            evaluated,
            passed: None,
            findings,
            message: Some(
                "one or more proposed instances lacked an embedding vector and could not be compared for near-duplicate collisions"
                    .to_string(),
            ),
        }
    }
}

async fn check_typing<'a>(
    store: &ProvenanceStore,
    entities: &[SemanticEntityProposal],
    tenant: &str,
    model: &str,
    vector_for: &impl Fn(&str) -> Option<&'a Vec<f32>>,
    policy: &TypingPolicy,
) -> SemanticCheckReport<TypingFinding> {
    // Every proposed entity is a typing candidate. A legacy/fallback write
    // with no declared class IRI is not silently removed from the denominator:
    // it makes this check `Unavailable`, because that entity's type was not
    // geometrically validated.
    let candidates: Vec<_> = entities.iter().enumerate().collect();
    let probes: Vec<_> = candidates
        .iter()
        .filter_map(|(probe_id, entity)| {
            vector_for(&entity.name).map(|vector| EntityGeometryProbe {
                probe_id: *probe_id,
                name: entity.name.clone(),
                storage_label: Some(entity.storage_label.clone()),
                vector: vector.clone(),
            })
        })
        .collect();
    let regions = match store
        .class_region_distances(&probes, tenant, model, policy.neighbors_per_class)
        .await
    {
        Ok(regions) => regions,
        Err(error) => {
            return unfinished_check(
                candidates.len(),
                SemanticValidationStatus::Failed,
                format!("typing geometry query failed ({error:#})"),
            );
        }
    };
    let mut by_probe: HashMap<usize, Vec<&ClassRegionDistance>> = HashMap::new();
    for region in &regions {
        by_probe.entry(region.probe_id).or_default().push(region);
    }

    let mut evaluated = 0;
    let mut findings = Vec::new();
    for (probe_id, entity) in candidates {
        let Some(class_iri) = entity.class_iri.as_deref() else {
            continue;
        };
        let Some(regions) = by_probe.get(&probe_id) else {
            continue;
        };
        let assigned = regions.iter().copied().find(|region| {
            region.class_iri == class_iri && region.exemplars >= policy.minimum_class_examples
        });
        let alternative = regions
            .iter()
            .copied()
            .filter(|region| {
                region.class_iri != class_iri && region.exemplars >= policy.minimum_class_examples
            })
            .min_by(|a, b| a.mean_distance.total_cmp(&b.mean_distance));
        let Some((assigned, alternative)) = assigned.zip(alternative) else {
            continue;
        };
        evaluated += 1;
        let assigned_distance = assigned.mean_distance as f32;
        let nearer_distance = alternative.mean_distance as f32;
        let advantage = assigned_distance - nearer_distance;
        if assigned_distance >= policy.minimum_assigned_class_distance
            && nearer_distance <= policy.maximum_nearer_class_distance
            && advantage >= policy.minimum_distance_advantage
        {
            findings.push(TypingFinding {
                name: entity.name.clone(),
                assigned_class: entity.entity_type.clone(),
                assigned_class_iri: class_iri.to_string(),
                assigned_distance,
                assigned_examples: assigned.exemplars,
                nearer_class: alternative
                    .entity_type
                    .clone()
                    .unwrap_or_else(|| alternative.class_iri.clone()),
                nearer_class_iri: alternative.class_iri.clone(),
                nearer_distance,
                nearer_examples: alternative.exemplars,
                distance_advantage: advantage,
            });
        }
    }
    findings.sort_by(|a, b| b.distance_advantage.total_cmp(&a.distance_advantage));
    findings.truncate(policy.maximum_findings);
    if evaluated == entities.len() {
        applied_check(evaluated, evaluated, findings)
    } else {
        SemanticCheckReport {
            status: SemanticValidationStatus::Unavailable,
            candidates: entities.len(),
            evaluated,
            passed: None,
            findings,
            message: Some(
                "one or more proposed instances lacked a declared class IRI or sufficiently populated assigned and alternative class regions"
                    .to_string(),
            ),
        }
    }
}

async fn check_triples<'a>(
    store: &ProvenanceStore,
    facts: &[LocalFact],
    tenant: &str,
    model: &str,
    vector_for: &impl Fn(&str) -> Option<&'a Vec<f32>>,
    policy: &TriplePlausibilityPolicy,
) -> SemanticCheckReport<TriplePlausibilityFinding> {
    let candidates: Vec<_> = facts
        .iter()
        .enumerate()
        .filter(|(_, fact)| triple_candidate(fact, policy))
        .collect();
    let probes: Vec<_> = candidates
        .iter()
        .filter_map(|(probe_id, fact)| {
            vector_for(&fact.subject).zip(vector_for(&fact.object)).map(
                |(subject_vector, object_vector)| TripleGeometryProbe {
                    probe_id: *probe_id,
                    predicate: fact.predicate.clone(),
                    subject_vector: subject_vector.clone(),
                    object_vector: object_vector.clone(),
                },
            )
        })
        .collect();
    let neighbors = match store
        .triple_geometry_neighbors(
            &probes,
            tenant,
            model,
            f64::from(policy.maximum_neighbor_distance),
            policy.maximum_neighbors_per_triple,
        )
        .await
    {
        Ok(neighbors) => neighbors,
        Err(error) => {
            return unfinished_check(
                candidates.len(),
                SemanticValidationStatus::Failed,
                format!("triple plausibility query failed ({error:#})"),
            );
        }
    };
    let mut by_probe: HashMap<usize, Vec<prism_provenance::TripleGeometryNeighbor>> =
        HashMap::new();
    for neighbor in neighbors {
        by_probe
            .entry(neighbor.probe_id)
            .or_default()
            .push(neighbor);
    }
    let mut evaluated = 0;
    let mut findings = Vec::new();
    for (probe_id, fact) in candidates {
        let Some(proposed_value) = fact.value.filter(|value| value.is_finite()) else {
            // Public callers can construct a `LocalFact` without passing
            // through extraction validation. Keep the malformed numeric fact
            // in the candidate denominator but do not let NaN/inf propagate
            // into a false Applied/pass judgement.
            continue;
        };
        let Some(neighbors) = by_probe.get(&probe_id) else {
            continue;
        };
        let mut evidence = Vec::new();
        for neighbor in neighbors {
            let Some(prior) = neighbor.value else {
                continue;
            };
            if !units_comparable(fact.unit.as_deref(), neighbor.unit.as_deref()) {
                continue;
            }
            let scale = proposed_value
                .abs()
                .max(prior.abs())
                .max(policy.numeric_scale_floor);
            let relative_error = (proposed_value - prior).abs() / scale;
            let numeric_agreement =
                (1.0 - relative_error / policy.numeric_relative_tolerance).clamp(0.0, 1.0);
            let stored_confidence_was_fallback = neighbor.confidence.is_none();
            let stored_confidence = neighbor
                .confidence
                .unwrap_or(policy.fallback_graph_confidence)
                .clamp(0.0, 1.0);
            let geometric_similarity = (1.0 - neighbor.distance).clamp(0.0, 1.0);
            evidence.push(TriplePriorEvidence {
                subject: neighbor.subject.clone(),
                object: neighbor.object.clone(),
                value: neighbor.value,
                unit: neighbor.unit.clone(),
                stored_confidence,
                stored_confidence_was_fallback,
                subject_distance: neighbor.subject_distance as f32,
                object_distance: neighbor.object_distance as f32,
                geometric_similarity,
                numeric_agreement,
                prior_contribution: geometric_similarity * numeric_agreement * stored_confidence,
            });
        }
        if evidence.len() < policy.minimum_prior_examples {
            continue;
        }
        evaluated += 1;
        let valid_extractor_confidence = fact
            .confidence
            .filter(|confidence| confidence.is_finite() && (0.0..=1.0).contains(confidence));
        let extractor_confidence_was_fallback = valid_extractor_confidence.is_none();
        let extractor_confidence = valid_extractor_confidence
            .unwrap_or(policy.fallback_extractor_confidence)
            .clamp(0.0, 1.0);
        let graph_prior = evidence
            .iter()
            .map(|item| item.prior_contribution)
            .sum::<f64>()
            / evidence.len() as f64;
        let combined_score = policy.extractor_confidence_weight * extractor_confidence
            + (1.0 - policy.extractor_confidence_weight) * graph_prior;
        if combined_score < policy.minimum_combined_score {
            findings.push(TriplePlausibilityFinding {
                subject: fact.subject.clone(),
                predicate: fact.predicate.clone(),
                object: fact.object.clone(),
                proposed_value: fact.value,
                extractor_confidence,
                extractor_confidence_was_fallback,
                graph_prior,
                combined_score,
                prior_examples: evidence.len(),
                evidence,
            });
        }
    }
    findings.sort_by(|a, b| a.combined_score.total_cmp(&b.combined_score));
    findings.truncate(policy.maximum_findings);
    let candidate_count = facts
        .iter()
        .filter(|fact| triple_candidate(fact, policy))
        .count();
    if evaluated == candidate_count {
        applied_check(candidate_count, evaluated, findings)
    } else {
        SemanticCheckReport {
            status: SemanticValidationStatus::Unavailable,
            candidates: candidate_count,
            evaluated,
            passed: None,
            findings,
            message: Some(format!(
                "one or more eligible composition triples lacked {} compatible same-predicate priors",
                policy.minimum_prior_examples
            )),
        }
    }
}

fn triple_candidate(fact: &LocalFact, policy: &TriplePlausibilityPolicy) -> bool {
    fact.value.is_some()
        && fact.kind.as_deref().is_some_and(|kind| {
            policy
                .eligible_fact_kinds
                .iter()
                .any(|eligible| eligible.eq_ignore_ascii_case(kind))
        })
}

fn units_comparable(proposed: Option<&str>, stored: Option<&str>) -> bool {
    match (proposed, stored) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        (None, None) => true,
        _ => false,
    }
}

fn requested_report(
    entities: &[SemanticEntityProposal],
    facts: &[LocalFact],
    policy: &SemanticValidationPolicy,
    backend: Option<String>,
) -> SemanticValidationReport {
    SemanticValidationReport {
        policy: policy.clone(),
        backend,
        near_duplicates: initial_check(policy.near_duplicate.enabled, entities.len()),
        typing: initial_check(policy.typing.enabled, entities.len()),
        triple_plausibility: initial_check(
            policy.triple_plausibility.enabled,
            facts
                .iter()
                .filter(|fact| triple_candidate(fact, &policy.triple_plausibility))
                .count(),
        ),
    }
}

fn initial_check<T>(enabled: bool, candidates: usize) -> SemanticCheckReport<T> {
    if enabled {
        unfinished_check(
            candidates,
            SemanticValidationStatus::Unavailable,
            "semantic validation has not been applied".to_string(),
        )
    } else {
        unfinished_check(
            candidates,
            SemanticValidationStatus::Disabled,
            "this semantic check was disabled by policy".to_string(),
        )
    }
}

fn report_without_backend(
    entities: &[SemanticEntityProposal],
    facts: &[LocalFact],
    policy: &SemanticValidationPolicy,
    status: SemanticValidationStatus,
    message: String,
) -> SemanticValidationBatch {
    let mut report = requested_report(entities, facts, policy, None);
    mark_requested(&mut report, policy, status, message);
    SemanticValidationBatch {
        report,
        embedding_model: None,
        embedding_names: Vec::new(),
        embedding_vectors: Vec::new(),
    }
}

fn mark_requested(
    report: &mut SemanticValidationReport,
    policy: &SemanticValidationPolicy,
    status: SemanticValidationStatus,
    message: String,
) {
    if policy.near_duplicate.enabled {
        set_unfinished(&mut report.near_duplicates, status, message.clone());
    }
    if policy.typing.enabled {
        set_unfinished(&mut report.typing, status, message.clone());
    }
    if policy.triple_plausibility.enabled {
        set_unfinished(&mut report.triple_plausibility, status, message);
    }
}

fn set_unfinished<T>(
    report: &mut SemanticCheckReport<T>,
    status: SemanticValidationStatus,
    message: String,
) {
    report.status = status;
    report.evaluated = 0;
    report.passed = None;
    report.findings.clear();
    report.message = Some(message);
}

fn unfinished_check<T>(
    candidates: usize,
    status: SemanticValidationStatus,
    message: String,
) -> SemanticCheckReport<T> {
    SemanticCheckReport {
        status,
        candidates,
        evaluated: 0,
        passed: None,
        findings: Vec::new(),
        message: Some(message),
    }
}

fn applied_check<T>(
    candidates: usize,
    evaluated: usize,
    findings: Vec<T>,
) -> SemanticCheckReport<T> {
    SemanticCheckReport {
        status: SemanticValidationStatus::Applied,
        candidates,
        evaluated,
        passed: Some(findings.is_empty()),
        findings,
        message: None,
    }
}

fn distinct_embedding_names(
    entities: &[SemanticEntityProposal],
    facts: &[LocalFact],
) -> (Vec<String>, HashMap<String, usize>) {
    let mut names = Vec::new();
    let mut indexes = HashMap::new();
    for name in entities.iter().map(|entity| entity.name.as_str()).chain(
        facts
            .iter()
            .flat_map(|fact| [fact.subject.as_str(), fact.object.as_str()]),
    ) {
        let key = canonical_name(name);
        if let std::collections::hash_map::Entry::Vacant(slot) = indexes.entry(key) {
            slot.insert(names.len());
            names.push(name.to_string());
        }
    }
    (names, indexes)
}

fn validate_vectors(names: &[String], vectors: &[Vec<f32>]) -> Result<(), String> {
    if names.len() != vectors.len() {
        return Err(format!(
            "returned {} vectors for {} names",
            vectors.len(),
            names.len()
        ));
    }
    let Some(dimensions) = vectors.first().map(Vec::len) else {
        return Ok(());
    };
    if dimensions == 0 {
        return Err("returned zero-dimensional vectors".to_string());
    }
    for (index, vector) in vectors.iter().enumerate() {
        if vector.len() != dimensions {
            return Err(format!(
                "vector {index} has {} dimensions, expected {dimensions}",
                vector.len()
            ));
        }
        if vector.iter().any(|component| !component.is_finite()) {
            return Err(format!("vector {index} contains a non-finite component"));
        }
        if vector.iter().all(|component| *component == 0.0) {
            return Err(format!("vector {index} is the zero vector"));
        }
    }
    Ok(())
}

fn same_identity(a: &SemanticEntityProposal, b: &SemanticEntityProposal) -> bool {
    a.name == b.name
        && a.storage_label == b.storage_label
        && a.class_iri == b.class_iri
        && a.entity_type == b.entity_type
}

fn canonical_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn lexical_form(name: &str) -> Vec<char> {
    name.chars()
        .flat_map(char::to_lowercase)
        .filter(|ch| ch.is_alphanumeric())
        .collect()
}

fn normalized_edit_distance(a: &str, b: &str) -> f32 {
    let a = lexical_form(a);
    let b = lexical_form(b);
    let denominator = a.len().max(b.len());
    if denominator == 0 {
        return 0.0;
    }
    levenshtein(&a, &b) as f32 / denominator as f32
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (row, left) in a.iter().enumerate() {
        current[0] = row + 1;
        for (column, right) in b.iter().enumerate() {
            current[column + 1] = (current[column] + 1)
                .min(previous[column + 1] + 1)
                .min(previous[column] + usize::from(left != right));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use prism_provenance::{ClassifiedNode, LocalProvenance};

    const TEST_MODEL: &str = "test:semantic-v1";

    struct CountingEmbed {
        calls: AtomicUsize,
    }

    impl CountingEmbed {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl EmbedBackend for CountingEmbed {
        async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(texts.iter().map(|text| test_vector(text)).collect())
        }

        fn dimensions(&self) -> usize {
            3
        }

        fn id(&self) -> &str {
            TEST_MODEL
        }
    }

    fn test_vector(text: &str) -> Vec<f32> {
        if text.trim().starts_with("Ti-6Al-4V") {
            return vec![1.0, 0.0, 0.0];
        }
        match text.trim() {
            "alloy-a" | "alloy-b" | "alloy-c" | "mystery powder" => {
                vec![1.0, 0.0, 0.0]
            }
            "process-a" | "process-b" | "process-c" | "Ti" => vec![0.0, 1.0, 0.0],
            "Al" => vec![0.0, 0.0, 1.0],
            "V" => vec![0.7, 0.7, 0.0],
            _ => vec![0.5, 0.5, 0.5],
        }
    }

    fn provenance() -> LocalProvenance {
        LocalProvenance {
            activity_id: "semantic-test-activity".into(),
            agent_id: "semantic-test".into(),
            agent_kind: "SoftwareAgent".into(),
            source_entity_id: "semantic-test-source".into(),
            source_kind: "Document".into(),
            tenant: "local".into(),
            started_at: "2026-08-11T00:00:00Z".into(),
            ended_at: "2026-08-11T00:00:00Z".into(),
            locality: "local".into(),
            origin_source_id: None,
        }
    }

    async fn open_store() -> (tempfile::TempDir, ProvenanceStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::open(&dir.path().join("semantic.db"))
            .await
            .unwrap();
        (dir, store)
    }

    async fn seed_class_entity(
        store: &ProvenanceStore,
        name: &str,
        entity_type: &str,
        class_iri: &str,
    ) {
        store
            .write_classified_entity(
                name,
                ClassifiedNode {
                    entity_type,
                    storage_label: entity_type,
                    class_iri,
                },
                None,
                "local",
            )
            .await
            .unwrap();
        store
            .store_precomputed_name_embeddings(
                &[name.to_string()],
                &[test_vector(name)],
                "local",
                TEST_MODEL,
            )
            .await
            .unwrap();
    }

    fn proposal(name: &str, entity_type: &str, class_iri: Option<&str>) -> SemanticEntityProposal {
        SemanticEntityProposal {
            name: name.to_string(),
            entity_type: entity_type.to_string(),
            storage_label: entity_type.to_string(),
            class_iri: class_iri.map(str::to_string),
        }
    }

    #[test]
    fn policy_defaults_are_valid() {
        SemanticValidationPolicy::default().validate().unwrap();
    }

    #[test]
    fn trivial_lexical_variants_are_close_but_distinct_names_are_preserved() {
        assert_eq!(normalized_edit_distance("Ti", "Ti "), 0.0);
        assert_eq!(normalized_edit_distance("LPBF", "lpbf"), 0.0);
        assert!(normalized_edit_distance("Ti", "Al") > 0.20);
    }

    #[test]
    fn unavailable_never_claims_passed() {
        let report = report_without_backend(
            &[SemanticEntityProposal {
                name: "Ti".into(),
                entity_type: "Element".into(),
                storage_label: "Element".into(),
                class_iri: Some("urn:class:element".into()),
            }],
            &[],
            &SemanticValidationPolicy::default(),
            SemanticValidationStatus::Unavailable,
            "no vectors".into(),
        )
        .report;
        assert_eq!(
            report.near_duplicates.status,
            SemanticValidationStatus::Unavailable
        );
        assert_eq!(report.near_duplicates.evaluated, 0);
        assert_eq!(report.near_duplicates.passed, None);
        assert_eq!(report.typing.passed, None);
        assert_eq!(report.triple_plausibility.passed, None);
    }

    #[tokio::test]
    async fn ontology_class_variants_are_reported_before_builder_merging() {
        let backend = CountingEmbed::new();
        let labels = vec![
            OntologyLabelProposal {
                label: "Heat Treatment".into(),
                kind: "class".into(),
            },
            OntologyLabelProposal {
                label: "HeatTreatment".into(),
                kind: "class".into(),
            },
        ];

        let report = validate_ontology_labels_with_backend(
            &labels,
            &NearDuplicatePolicy::default(),
            Some(&backend),
        )
        .await;

        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            report.near_duplicates.status,
            SemanticValidationStatus::Applied
        );
        assert_eq!(report.proposals, labels);
        assert!(report.near_duplicates.passed == Some(false));
        assert!(report.near_duplicates.findings.iter().any(|finding| {
            finding.proposed_name == "Heat Treatment" && finding.colliding_name == "HeatTreatment"
        }));
    }

    #[test]
    fn legacy_ontology_report_is_explicitly_unavailable() {
        let report = OntologySemanticValidationReport::default();
        assert_eq!(
            report.near_duplicates.status,
            SemanticValidationStatus::Unavailable
        );
        assert_eq!(report.near_duplicates.evaluated, 0);
        assert_eq!(report.near_duplicates.passed, None);
    }

    #[tokio::test]
    async fn trivial_variant_is_reported_as_collision_in_one_embedding_batch() {
        let (_dir, store) = open_store().await;
        let backend = CountingEmbed::new();
        let entities = vec![
            proposal("Ti", "Element", Some("https://example.org/Element")),
            proposal("Ti ", "Element", Some("https://example.org/Element")),
        ];

        let batch = validate_write_with_backend(
            &store,
            &entities,
            &[],
            "local",
            &SemanticValidationPolicy::default(),
            Some(&backend),
        )
        .await;

        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            batch.report.near_duplicates.status,
            SemanticValidationStatus::Unavailable,
            "the proposal collision is measurable, but an empty graph still cannot pose as validated"
        );
        assert_eq!(batch.report.near_duplicates.passed, None);
        let collision = batch
            .report
            .near_duplicates
            .findings
            .iter()
            .find(|finding| finding.source == CollisionSource::Proposal)
            .expect("Ti and its whitespace variant must be reported");
        assert_eq!(collision.proposed_name, "Ti");
        assert_eq!(collision.colliding_name, "Ti ");
        assert_eq!(collision.cosine_distance, 0.0);
        assert_eq!(collision.normalized_edit_distance, 0.0);
    }

    #[tokio::test]
    async fn disabling_checks_still_prepares_the_existing_search_index_batch() {
        let (_dir, store) = open_store().await;
        let backend = CountingEmbed::new();
        let policy = SemanticValidationPolicy {
            near_duplicate: NearDuplicatePolicy {
                enabled: false,
                ..NearDuplicatePolicy::default()
            },
            typing: TypingPolicy {
                enabled: false,
                ..TypingPolicy::default()
            },
            triple_plausibility: TriplePlausibilityPolicy {
                enabled: false,
                ..TriplePlausibilityPolicy::default()
            },
        };

        let batch = validate_write_with_backend(
            &store,
            &[proposal(
                "Ti",
                "Element",
                Some("https://example.org/Element"),
            )],
            &[],
            "local",
            &policy,
            Some(&backend),
        )
        .await;

        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        assert_eq!(batch.embedding_names(), ["Ti"]);
        assert_eq!(batch.embedding_vectors().len(), 1);
        assert_eq!(
            batch.report.near_duplicates.status,
            SemanticValidationStatus::Disabled
        );
        assert_eq!(batch.report.near_duplicates.passed, None);
        assert_eq!(
            batch.report.typing.status,
            SemanticValidationStatus::Disabled
        );
        assert_eq!(
            batch.report.triple_plausibility.status,
            SemanticValidationStatus::Disabled
        );
    }

    #[tokio::test]
    async fn mistyped_instance_reports_the_nearer_class() {
        let (_dir, store) = open_store().await;
        for name in ["alloy-a", "alloy-b", "alloy-c"] {
            seed_class_entity(&store, name, "Alloy", "https://example.org/Alloy").await;
        }
        for name in ["process-a", "process-b", "process-c"] {
            seed_class_entity(&store, name, "Process", "https://example.org/Process").await;
        }
        let backend = CountingEmbed::new();
        let entities = vec![proposal(
            "mystery powder",
            "Process",
            Some("https://example.org/Process"),
        )];

        let batch = validate_write_with_backend(
            &store,
            &entities,
            &[],
            "local",
            &SemanticValidationPolicy::default(),
            Some(&backend),
        )
        .await;

        assert_eq!(
            batch.report.typing.status,
            SemanticValidationStatus::Applied
        );
        let finding = batch
            .report
            .typing
            .findings
            .first()
            .expect("the obviously wrong Process assignment must be reported");
        assert_eq!(finding.assigned_class, "Process");
        assert_eq!(finding.nearer_class, "Alloy");
        assert!(finding.assigned_distance > finding.nearer_distance);
        assert_eq!(finding.assigned_examples, 3);
        assert_eq!(finding.nearer_examples, 3);
    }

    #[tokio::test]
    async fn absent_embeddings_are_unavailable_and_the_write_still_succeeds() {
        let (_dir, store) = open_store().await;
        let fact = LocalFact {
            subject: "Ti-6Al-4V".into(),
            predicate: "HAS_PHASE".into(),
            object: "alpha-beta".into(),
            value: None,
            unit: None,
            confidence: Some(0.8),
            kind: Some("phase".into()),
        };
        let entities = vec![
            proposal("Ti-6Al-4V", "Alloy", None),
            proposal("alpha-beta", "Phase", None),
        ];

        let batch = validate_write_with_backend(
            &store,
            &entities,
            std::slice::from_ref(&fact),
            "local",
            &SemanticValidationPolicy::default(),
            None,
        )
        .await;
        for (status, evaluated, passed) in [
            (
                batch.report.near_duplicates.status,
                batch.report.near_duplicates.evaluated,
                batch.report.near_duplicates.passed,
            ),
            (
                batch.report.typing.status,
                batch.report.typing.evaluated,
                batch.report.typing.passed,
            ),
            (
                batch.report.triple_plausibility.status,
                batch.report.triple_plausibility.evaluated,
                batch.report.triple_plausibility.passed,
            ),
        ] {
            assert_eq!(status, SemanticValidationStatus::Unavailable);
            assert_eq!(evaluated, 0);
            assert_eq!(passed, None);
        }

        store.write_fact(&fact, &provenance()).await.unwrap();
        let written = store
            .recall_with_context("Ti-6Al-4V", "local", 10)
            .await
            .unwrap();
        assert_eq!(written.len(), 1, "unavailable geometry blocked the write");
        assert_eq!(written[0].object, "alpha-beta");
    }

    #[tokio::test]
    async fn extractor_confidence_changes_the_fused_triple_judgement() {
        let (_dir, store) = open_store().await;
        let prior = LocalFact {
            subject: "Ti-6Al-4V".into(),
            predicate: "CONTAINS".into(),
            object: "Ti".into(),
            value: Some(0.90),
            unit: None,
            confidence: Some(0.8),
            kind: Some("contains".into()),
        };
        store.write_fact(&prior, &provenance()).await.unwrap();
        let names = vec!["Ti-6Al-4V".to_string(), "Ti".to_string()];
        let vectors: Vec<_> = names.iter().map(|name| test_vector(name)).collect();
        store
            .store_precomputed_name_embeddings(&names, &vectors, "local", TEST_MODEL)
            .await
            .unwrap();

        let proposed = |confidence| LocalFact {
            value: Some(0.10),
            confidence: Some(confidence),
            ..prior.clone()
        };
        let facts = vec![proposed(0.20), proposed(0.90)];
        let policy = SemanticValidationPolicy {
            near_duplicate: NearDuplicatePolicy {
                enabled: false,
                ..NearDuplicatePolicy::default()
            },
            typing: TypingPolicy {
                enabled: false,
                ..TypingPolicy::default()
            },
            triple_plausibility: TriplePlausibilityPolicy {
                minimum_prior_examples: 1,
                minimum_combined_score: 0.40,
                ..TriplePlausibilityPolicy::default()
            },
        };
        let backend = CountingEmbed::new();

        let batch = validate_write_with_backend(
            &store,
            &[
                proposal("Ti-6Al-4V", "Alloy", None),
                proposal("Ti", "Element", None),
            ],
            &facts,
            "local",
            &policy,
            Some(&backend),
        )
        .await;

        assert_eq!(
            batch.report.triple_plausibility.status,
            SemanticValidationStatus::Applied
        );
        assert_eq!(batch.report.triple_plausibility.findings.len(), 1);
        let finding = &batch.report.triple_plausibility.findings[0];
        assert_eq!(finding.extractor_confidence, 0.20);
        assert!(finding.combined_score < policy.triple_plausibility.minimum_combined_score);
        assert!(
            policy.triple_plausibility.extractor_confidence_weight * 0.90
                >= policy.triple_plausibility.minimum_combined_score,
            "the high-confidence proposal must cross the same fixed prior/threshold"
        );
    }

    #[tokio::test]
    async fn non_finite_triple_value_cannot_pose_as_applied_or_passed() {
        let (_dir, store) = open_store().await;
        let prior = LocalFact {
            subject: "Ti-6Al-4V".into(),
            predicate: "CONTAINS".into(),
            object: "Ti".into(),
            value: Some(0.90),
            unit: None,
            confidence: Some(0.8),
            kind: Some("contains".into()),
        };
        store.write_fact(&prior, &provenance()).await.unwrap();
        let names = vec!["Ti-6Al-4V".to_string(), "Ti".to_string()];
        let vectors: Vec<_> = names.iter().map(|name| test_vector(name)).collect();
        store
            .store_precomputed_name_embeddings(&names, &vectors, "local", TEST_MODEL)
            .await
            .unwrap();

        let policy = SemanticValidationPolicy {
            near_duplicate: NearDuplicatePolicy {
                enabled: false,
                ..NearDuplicatePolicy::default()
            },
            typing: TypingPolicy {
                enabled: false,
                ..TypingPolicy::default()
            },
            triple_plausibility: TriplePlausibilityPolicy {
                minimum_prior_examples: 1,
                ..TriplePlausibilityPolicy::default()
            },
        };
        let malformed = LocalFact {
            value: Some(f64::NAN),
            ..prior
        };

        let batch = validate_write_with_backend(
            &store,
            &[
                proposal("Ti-6Al-4V", "Alloy", None),
                proposal("Ti", "Element", None),
            ],
            &[malformed],
            "local",
            &policy,
            Some(&CountingEmbed::new()),
        )
        .await;

        assert_eq!(
            batch.report.triple_plausibility.status,
            SemanticValidationStatus::Unavailable
        );
        assert_eq!(batch.report.triple_plausibility.candidates, 1);
        assert_eq!(batch.report.triple_plausibility.evaluated, 0);
        assert_eq!(batch.report.triple_plausibility.passed, None);
    }

    #[tokio::test]
    async fn swapped_composition_passes_arithmetic_but_geometry_reports_without_mutation() {
        let (_dir, store) = open_store().await;
        let correct = [("Ti", 0.90), ("Al", 0.06), ("V", 0.04)];
        let reference_alloys = [
            "Ti-6Al-4V",
            "Ti-6Al-4V reference A",
            "Ti-6Al-4V reference B",
        ];
        for subject in reference_alloys {
            for (element, fraction) in correct {
                store
                    .write_fact(
                        &LocalFact {
                            subject: subject.into(),
                            predicate: "CONTAINS".into(),
                            object: element.into(),
                            value: Some(fraction),
                            unit: None,
                            confidence: Some(0.8),
                            kind: Some("contains".into()),
                        },
                        &provenance(),
                    )
                    .await
                    .unwrap();
            }
        }
        let stored_names: Vec<String> = [
            "Ti-6Al-4V",
            "Ti-6Al-4V reference A",
            "Ti-6Al-4V reference B",
            "Ti",
            "Al",
            "V",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let stored_vectors: Vec<Vec<f32>> =
            stored_names.iter().map(|name| test_vector(name)).collect();
        store
            .store_precomputed_name_embeddings(&stored_names, &stored_vectors, "local", TEST_MODEL)
            .await
            .unwrap();

        let extracted = crate::EntitySet {
            entities: vec![
                crate::Entity {
                    entity_type: "Alloy".into(),
                    name: "Ti-6Al-4V".into(),
                    properties: serde_json::json!({}),
                },
                crate::Entity {
                    entity_type: "Element".into(),
                    name: "Ti".into(),
                    properties: serde_json::json!({}),
                },
                crate::Entity {
                    entity_type: "Element".into(),
                    name: "Al".into(),
                    properties: serde_json::json!({}),
                },
                crate::Entity {
                    entity_type: "Element".into(),
                    name: "V".into(),
                    properties: serde_json::json!({}),
                },
            ],
            relationships: [("Ti", 0.06), ("Al", 0.84), ("V", 0.10)]
                .into_iter()
                .map(|(element, weight)| crate::Relationship {
                    from: "Ti-6Al-4V".into(),
                    rel_type: "CONTAINS".into(),
                    to: element.into(),
                    weight: Some(weight),
                    order: None,
                    value: None,
                    unit: None,
                    confidence: None,
                })
                .collect(),
        };
        let structural =
            crate::graph_validation::validate_graph(&crate::ontologies::EmmoOntology, &extracted);
        assert!(
            structural.passed,
            "the swapped fractions must evade sum-only arithmetic: {:?}",
            structural.issues
        );
        let (facts, dropped) = crate::local_facts::to_local_facts(&extracted);
        assert!(dropped.is_empty());
        let entities: Vec<_> = extracted
            .entities
            .iter()
            .map(|entity| proposal(&entity.name, &entity.entity_type, None))
            .collect();
        let before_facts = serde_json::to_value(
            store
                .recall_with_context("Ti-6Al-4V", "local", 20)
                .await
                .unwrap(),
        )
        .unwrap();
        let before_vectors = store.entity_embedding_count("local").await.unwrap();
        let backend = CountingEmbed::new();

        let batch = validate_write_with_backend(
            &store,
            &entities,
            &facts,
            "local",
            &SemanticValidationPolicy::default(),
            Some(&backend),
        )
        .await;

        assert_eq!(
            batch.report.triple_plausibility.status,
            SemanticValidationStatus::Applied
        );
        assert!(
            batch
                .report
                .triple_plausibility
                .findings
                .iter()
                .any(|finding| finding.object == "Ti" && finding.proposed_value == Some(0.06)),
            "the Ti fraction inversion was not reported: {:?}",
            batch.report.triple_plausibility.findings
        );
        assert!(
            batch
                .report
                .triple_plausibility
                .findings
                .iter()
                .any(|finding| finding.object == "Al" && finding.proposed_value == Some(0.84)),
            "the Al fraction inversion was not reported: {:?}",
            batch.report.triple_plausibility.findings
        );

        let after_facts = serde_json::to_value(
            store
                .recall_with_context("Ti-6Al-4V", "local", 20)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(after_facts, before_facts, "geometry mutated graph facts");
        assert_eq!(
            store.entity_embedding_count("local").await.unwrap(),
            before_vectors,
            "validation stored or removed vectors before the graph write"
        );
    }
}
