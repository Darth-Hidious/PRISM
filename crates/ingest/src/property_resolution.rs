//! The post-extraction ontology resolution ladder for free-text property
//! names.
//!
//! Extraction is deliberately vocabulary-free (in-prompt ontology measurably
//! costs recall), so a measured quantity arrives named in the paper's own
//! words — "crack-growth resistance", "laser absorptivity" — and nothing
//! downstream can identify it. This module binds those names AFTER
//! extraction, against the UNION of loaded ontologies, one rung at a time:
//!
//! 1. **Exact** — the term IS a declared extraction label or `skos:prefLabel`
//!    of some loaded ontology's class. Deterministic.
//! 2. **Normalised** — equal after case/punctuation/hyphenation/CamelCase
//!    folding, with plural-insensitive comparison. Deterministic.
//! 3. **Semantic** — nearest class label over the store's per-ontology
//!    class-label vectors (`vector_distance_cos` inside Turso), bound only
//!    at or above a conservative similarity threshold. Probabilistic, so the
//!    binding records its rung AND its score.
//! 4. **Below threshold** — the term stays free text; a class proposal with
//!    the fact's own citation is queued for HUMAN governance
//!    (`propose_class`'s queue), and the term is recorded unbound WITH its
//!    best score so the threshold can later be tuned from data.
//!
//! No rung discards a fact. Facts are written before resolution runs and
//! keep their free-text spelling; a binding is a separate
//! `(tenant, canonical term)` record (see `prism_provenance::term_binding`)
//! that joins back to facts and entities through the term itself. That is
//! what makes RE-RESOLUTION possible: promote a proposal (or load a richer
//! ontology), re-run the ladder over
//! [`prism_provenance::ProvenanceStore::unbound_term_bindings`], and every
//! fact carrying the term re-binds — no paper is re-read.
//!
//! Domain knowledge never lives here: candidates come exclusively from the
//! loaded ontologies' declarations, and there is no enum of measurement
//! kinds, no property-name table, and no per-domain spelling list.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result, ensure};

use crate::ontologies::OntologySet;
use crate::paper_agent::{OntologyClassProposal, PaperCitation, class_proposal_queue_item};
use prism_provenance::{
    ClassLabelEmbedding, ProvenanceStore, TERM_BINDING_RUNG_EXACT, TERM_BINDING_RUNG_NORMALIZED,
    TERM_BINDING_RUNG_PROPOSED, TERM_BINDING_RUNG_SEMANTIC, TermBinding, canonical_key,
};

/// Conservative starting threshold for rung 3 (cosine similarity, `[0, 1]`
/// scale). Chosen to bind only strong agreement; every score — above AND
/// below — is recorded with its binding so this number can be tuned from
/// measured data instead of guessed twice.
pub const DEFAULT_SEMANTIC_BIND_THRESHOLD: f64 = 0.85;

/// The rung of the ladder that produced a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingRung {
    Exact,
    Normalized,
    Semantic,
    /// Not bound: the term stayed free text and (citation permitting) a
    /// class proposal was queued for governance.
    Proposed,
}

impl BindingRung {
    #[must_use]
    pub fn as_i64(self) -> i64 {
        match self {
            Self::Exact => TERM_BINDING_RUNG_EXACT,
            Self::Normalized => TERM_BINDING_RUNG_NORMALIZED,
            Self::Semantic => TERM_BINDING_RUNG_SEMANTIC,
            Self::Proposed => TERM_BINDING_RUNG_PROPOSED,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Normalized => "normalized",
            Self::Semantic => "semantic",
            Self::Proposed => "proposed",
        }
    }
}

/// One free-text property name to resolve, with the citation of the fact
/// that carried it (rung 4 queues the proposal WITH this citation; without
/// one the unbound record is still written, but nothing can be cited to
/// governance).
#[derive(Debug, Clone)]
pub struct PropertyTerm {
    pub term: String,
    pub citation: Option<PaperCitation>,
}

/// The ladder's outcome for one term.
#[derive(Debug, Clone)]
pub struct PropertyBinding {
    /// The exact source spelling first seen for this term.
    pub term: String,
    /// Canonical key — the durable record's identity.
    pub canonical: String,
    pub class_iri: Option<String>,
    /// Id of the LOADED ontology that declared the bound class.
    pub ontology_id: Option<String>,
    pub rung: BindingRung,
    /// Best cosine similarity observed, recorded for rung 3 AND rung 4.
    pub score: Option<f64>,
    /// Embedding model behind `score`.
    pub model: Option<String>,
    /// Governance queue item id when rung 4 enqueued a proposal.
    pub proposal_item_id: Option<String>,
    /// How many graph entities the bind stamped (`class_iri` filled where it
    /// was NULL — never overwriting a declared classification).
    pub entities_stamped: u64,
    /// Whether the store's binding row now reflects this outcome (`false`
    /// when an equal-or-stronger binding already held; that binding stands).
    pub recorded: bool,
}

/// Collect the property-name terms one STORED fact contributes.
///
/// A name becomes a term only where it is BOTH unbound — the extractor's
/// ontology binding left the slot empty and no loaded ontology declares the
/// label — and property-name shaped. Both CLI ingest planes call this, so
/// the two cannot drift apart on what counts as a bindable term; the
/// callers differ only in where they read the binding slots from.
pub fn property_terms_for_fact(
    ontologies: &OntologySet,
    fact: &prism_provenance::MaterialFact,
    predicate_iri: Option<&str>,
    object_class_iri: Option<&str>,
    citation: Option<PaperCitation>,
    out: &mut Vec<PropertyTerm>,
) {
    // Only a MEASURED quantity carries a property name worth binding: the
    // value and unit together are what make the predicate/object name a
    // quantity rather than prose.
    if !(fact.value.is_some_and(f64::is_finite) && fact.unit.is_some()) {
        return;
    }
    let predicate_names_property =
        predicate_iri.is_none() && ontologies.relation_for_label(&fact.predicate).is_none();
    if predicate_names_property && is_property_name_shaped(&fact.predicate) {
        out.push(PropertyTerm {
            term: fact.predicate.clone(),
            citation: citation.clone(),
        });
    }
    // The object slot of a measured fact holds EITHER the quantity's name
    // ("yield strength") or a restatement of the measurement itself ("485
    // HV30 (highest among the alloys studied)"). The second is a value, not
    // a property name, and turning it into a class proposal puts work on a
    // human reviewer that can never be accepted. Told apart structurally by
    // the fact's OWN number — no vocabulary, no length limit, nothing that
    // could reject a real name like "0.2% proof stress" (whose leading
    // number is not the measured value).
    let object_restates_the_value = fact.value.is_some_and(|value| {
        leading_number(&fact.object).is_some_and(|lead| same_number(lead, value))
    });
    if object_class_iri.is_none()
        && !object_restates_the_value
        && is_property_name_shaped(&fact.object)
    {
        out.push(PropertyTerm {
            term: fact.object.clone(),
            citation,
        });
    }
}

/// The numeric literal a string OPENS with, if any (`"485 HV30 (…)"` → 485,
/// `"0.2% proof stress"` → 0.2, `"yield strength"` → None).
fn leading_number(text: &str) -> Option<f64> {
    let trimmed = text.trim_start();
    let mut end = 0usize;
    let mut seen_digit = false;
    for (index, ch) in trimmed.char_indices() {
        let keep = match ch {
            '0'..='9' => {
                seen_digit = true;
                true
            }
            '+' | '-' => index == 0,
            '.' => seen_digit,
            'e' | 'E' => false,
            _ => false,
        };
        if !keep {
            break;
        }
        end = index + ch.len_utf8();
    }
    if !seen_digit {
        return None;
    }
    trimmed[..end].parse::<f64>().ok().filter(|n| n.is_finite())
}

/// Equal as measurements: relative for large magnitudes, absolute near zero.
fn same_number(left: f64, right: f64) -> bool {
    let difference = (left - right).abs();
    difference <= f64::EPSILON.max(1e-9 * left.abs().max(right.abs()))
}

/// Roll one resolution run up for a command's JSON result: counts per rung
/// plus every binding with its rung, score, and pending proposal id.
#[must_use]
pub fn binding_report(bindings: &[PropertyBinding]) -> serde_json::Value {
    let count = |rung: BindingRung| bindings.iter().filter(|b| b.rung == rung).count();
    serde_json::json!({
        "terms": bindings.len(),
        "exact": count(BindingRung::Exact),
        "normalized": count(BindingRung::Normalized),
        "semantic": count(BindingRung::Semantic),
        "proposed": count(BindingRung::Proposed),
        "proposals_enqueued": bindings
            .iter()
            .filter(|b| b.proposal_item_id.is_some())
            .count(),
        "entities_stamped": bindings.iter().map(|b| b.entities_stamped).sum::<u64>(),
        "threshold": DEFAULT_SEMANTIC_BIND_THRESHOLD,
        "bindings": bindings
            .iter()
            .map(|b| {
                serde_json::json!({
                    "term": b.term,
                    "rung": b.rung.as_str(),
                    "class_iri": b.class_iri,
                    "ontology_id": b.ontology_id,
                    "score": b.score,
                    "model": b.model,
                    "proposal_item_id": b.proposal_item_id,
                    "entities_stamped": b.entities_stamped,
                    "recorded": b.recorded,
                })
            })
            .collect::<Vec<_>>(),
    })
}

/// Structural guard for term selection: a string that is a NUMBER with at
/// most one trailing token ("1100", "950 MPa", "8.19g/cm3") restates a
/// VALUE, not a property name, and must not become a class proposal. A
/// number-led string with a longer tail ("0.2% proof stress") is a
/// legitimate property name and passes. Shape only — no vocabulary.
#[must_use]
pub fn is_property_name_shaped(term: &str) -> bool {
    let trimmed = term.trim();
    if trimmed.is_empty() {
        return false;
    }
    match crate::local_facts::split_leading_number(trimmed) {
        Some((_, tail)) => tail.split_whitespace().count() >= 2,
        None => true,
    }
}

/// Fold a term or label for rung-2 comparison: CamelCase boundaries become
/// spaces, everything non-alphanumeric becomes a space, case folds, and
/// whitespace collapses. `"MetallicMaterial"`, `"metallic-material"`, and
/// `"Metallic  material"` all fold to `"metallic material"`.
#[must_use]
pub fn normalize_term(term: &str) -> String {
    let mut spaced = String::with_capacity(term.len() + 8);
    let mut previous_was_lower = false;
    for ch in term.chars() {
        if ch.is_uppercase() && previous_was_lower {
            spaced.push(' ');
        }
        previous_was_lower = ch.is_lowercase() || ch.is_numeric();
        spaced.push(ch);
    }
    let mut folded = String::with_capacity(spaced.len());
    for ch in spaced.chars() {
        if ch.is_alphanumeric() {
            folded.extend(ch.to_lowercase());
        } else {
            folded.push(' ');
        }
    }
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Plural-insensitive form of an already-normalised term: each word drops
/// ONE trailing `s` when longer than three characters and not ending in
/// `ss`. Both sides of a rung-2 comparison receive the same fold, so
/// `"metallic materials"` meets `"metallic material"` without a
/// language-specific stemmer.
fn singular(normalized: &str) -> String {
    normalized
        .split(' ')
        .map(|word| {
            if word.len() > 3 && word.ends_with('s') && !word.ends_with("ss") {
                &word[..word.len() - 1]
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One candidate label from the union of loaded ontologies. The Vec is
/// built in SET ORDER then declaration order, and the lexical rungs take
/// the FIRST match — that ordering is the deterministic collision rule (the
/// earliest loaded declaration wins).
struct LabelCandidate {
    ontology_id: &'static str,
    class_iri: String,
    label: String,
    normalized: String,
    singular: String,
}

/// Every `(class, label)` pair the union declares, in set order then
/// declaration order. Labels are the class's extraction labels plus its
/// `skos:prefLabel`; the NAVIGABLE declaration is consulted (not just the
/// smaller extraction-facing slice) because binding is post-hoc
/// identification — it widens nothing the validator accepts.
fn label_candidates(ontologies: &OntologySet) -> Vec<LabelCandidate> {
    let mut candidates = Vec::new();
    for ontology in ontologies.all() {
        for class in ontology.ontology_classes() {
            let mut labels: Vec<&str> =
                class.extraction_labels.iter().map(String::as_str).collect();
            if let Some(pref) = class.pref_label.as_deref()
                && !labels.contains(&pref)
            {
                labels.push(pref);
            }
            for label in labels {
                let normalized = normalize_term(label);
                candidates.push(LabelCandidate {
                    ontology_id: ontology.id(),
                    class_iri: class.iri.as_str().to_string(),
                    label: label.to_string(),
                    singular: singular(&normalized),
                    normalized,
                });
            }
        }
    }
    candidates
}

/// Ensure every loaded ontology's class labels are embedded and stored for
/// `backend`'s model, embedding only what the diff says is missing. The
/// text embedded is the label's normalised fold (embedding models read
/// `"metallic material"` better than `"MetallicMaterial"`); the stored row
/// keeps the raw label.
async fn seed_class_label_embeddings(
    store: &ProvenanceStore,
    candidates: &[LabelCandidate],
    backend: &dyn prism_embed::EmbedBackend,
) -> Result<()> {
    let mut by_ontology: HashMap<&'static str, Vec<&LabelCandidate>> = HashMap::new();
    for candidate in candidates {
        by_ontology
            .entry(candidate.ontology_id)
            .or_default()
            .push(candidate);
    }
    // Deterministic ontology order for the seeding passes.
    let mut ontology_ids: Vec<&'static str> = by_ontology.keys().copied().collect();
    ontology_ids.sort_unstable();
    for ontology_id in ontology_ids {
        let candidates = &by_ontology[ontology_id];
        let existing: HashSet<(String, String)> = store
            .class_label_embedding_keys(ontology_id, backend.id())
            .await?
            .into_iter()
            .collect();
        let missing: Vec<&&LabelCandidate> = candidates
            .iter()
            .filter(|candidate| {
                !existing.contains(&(candidate.class_iri.clone(), candidate.label.clone()))
            })
            .collect();
        if missing.is_empty() {
            continue;
        }
        let texts: Vec<String> = missing
            .iter()
            .map(|candidate| candidate.normalized.clone())
            .collect();
        let vectors = backend
            .embed(&texts)
            .await
            .with_context(|| format!("embedding class labels of ontology '{ontology_id}'"))?;
        ensure!(
            vectors.len() == missing.len(),
            "embedding backend returned {} vectors for {} class labels",
            vectors.len(),
            missing.len()
        );
        let entries: Vec<ClassLabelEmbedding> = missing
            .iter()
            .zip(vectors)
            .map(|(candidate, vector)| ClassLabelEmbedding {
                class_iri: candidate.class_iri.clone(),
                label: candidate.label.clone(),
                vector,
            })
            .collect();
        store
            .store_class_label_embeddings(ontology_id, backend.id(), &entries)
            .await?;
    }
    Ok(())
}

/// The nearest stored class label to `vector` under the SET-ORDER collision
/// rule: nearest first, and among exact distance ties the earliest loaded
/// ontology wins (the storage layer measures; the policy lives here).
async fn nearest_for(
    store: &ProvenanceStore,
    vector: &[f32],
    ontologies: &OntologySet,
    model: &str,
) -> Result<Option<prism_provenance::ClassLabelNeighbor>> {
    let set_positions: HashMap<&str, usize> = ontologies
        .all()
        .iter()
        .enumerate()
        .map(|(position, ontology)| (ontology.id(), position))
        .collect();
    let ids: Vec<&str> = ontologies.all().iter().map(|o| o.id()).collect();
    let mut neighbors = store.nearest_class_labels(vector, &ids, model, 8).await?;
    neighbors.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                set_positions
                    .get(a.ontology_id.as_str())
                    .cmp(&set_positions.get(b.ontology_id.as_str()))
            })
            .then_with(|| a.class_iri.cmp(&b.class_iri))
    });
    Ok(neighbors.into_iter().next())
}

/// Run the ladder for `terms` and persist the outcomes.
///
/// Facts are NEVER touched: callers write facts first, then resolve. For
/// every distinct term this records one binding row (rung + score), stamps
/// bound classes onto graph entities whose class is still unknown, and — for
/// rung 4 with a citation — queues one class proposal for human governance.
/// `backend = None` (no embedding model configured) degrades honestly: the
/// lexical rungs still bind, and everything else is recorded unbound with no
/// score.
pub async fn resolve_property_terms(
    store: &ProvenanceStore,
    ontologies: &OntologySet,
    backend: Option<&dyn prism_embed::EmbedBackend>,
    tenant: &str,
    document: &str,
    terms: &[PropertyTerm],
    threshold: f64,
) -> Result<Vec<PropertyBinding>> {
    ensure!(
        threshold.is_finite() && (0.0..=1.0).contains(&threshold),
        "semantic bind threshold must be a finite similarity from 0 to 1, got {threshold}"
    );

    // One entry per canonical spelling, first-seen verbatim, first citation.
    let mut order: Vec<String> = Vec::new();
    let mut distinct: HashMap<String, (String, Option<PaperCitation>)> = HashMap::new();
    for term in terms {
        let trimmed = term.term.trim();
        if trimmed.is_empty() {
            continue;
        }
        let canonical = canonical_key(trimmed);
        match distinct.get_mut(&canonical) {
            None => {
                order.push(canonical.clone());
                distinct.insert(canonical, (trimmed.to_string(), term.citation.clone()));
            }
            Some((_, citation @ None)) => *citation = term.citation.clone(),
            Some(_) => {}
        }
    }
    if order.is_empty() {
        return Ok(Vec::new());
    }

    let candidates = label_candidates(ontologies);

    // Rungs 1 and 2 — deterministic, no store round trips.
    struct Pending {
        canonical: String,
        verbatim: String,
        citation: Option<PaperCitation>,
    }
    let mut outcomes: Vec<PropertyBinding> = Vec::new();
    let mut misses: Vec<Pending> = Vec::new();
    for canonical in order {
        let (verbatim, citation) = distinct.remove(&canonical).expect("inserted above");
        let exact = candidates
            .iter()
            .find(|candidate| candidate.label == verbatim);
        let lexical = exact
            .map(|candidate| (BindingRung::Exact, candidate))
            .or_else(|| {
                let normalized = normalize_term(&verbatim);
                let singular_form = singular(&normalized);
                candidates
                    .iter()
                    .find(|candidate| {
                        candidate.normalized == normalized || candidate.singular == singular_form
                    })
                    .map(|candidate| (BindingRung::Normalized, candidate))
            });
        match lexical {
            Some((rung, candidate)) => outcomes.push(PropertyBinding {
                term: verbatim,
                canonical,
                class_iri: Some(candidate.class_iri.clone()),
                ontology_id: Some(candidate.ontology_id.to_string()),
                rung,
                score: None,
                model: None,
                proposal_item_id: None,
                entities_stamped: 0,
                recorded: false,
            }),
            None => misses.push(Pending {
                canonical,
                verbatim,
                citation,
            }),
        }
    }

    // Rung 3 — semantic, when geometry is available.
    let mut miss_neighbors: Vec<Option<prism_provenance::ClassLabelNeighbor>> =
        vec![None; misses.len()];
    let mut model_id: Option<String> = None;
    if let Some(backend) = backend
        && !misses.is_empty()
    {
        seed_class_label_embeddings(store, &candidates, backend).await?;
        let texts: Vec<String> = misses
            .iter()
            .map(|pending| normalize_term(&pending.verbatim))
            .collect();
        let vectors = backend
            .embed(&texts)
            .await
            .context("embedding free-text property terms")?;
        ensure!(
            vectors.len() == misses.len(),
            "embedding backend returned {} vectors for {} terms",
            vectors.len(),
            misses.len()
        );
        for (slot, vector) in miss_neighbors.iter_mut().zip(&vectors) {
            *slot = nearest_for(store, vector, ontologies, backend.id()).await?;
        }
        model_id = Some(backend.id().to_string());
    }

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0);
    for (pending, neighbor) in misses.into_iter().zip(miss_neighbors) {
        let score = neighbor.as_ref().map(|n| n.similarity);
        let bound = neighbor
            .as_ref()
            .filter(|n| n.similarity >= threshold)
            .cloned();
        match bound {
            Some(neighbor) => outcomes.push(PropertyBinding {
                term: pending.verbatim,
                canonical: pending.canonical,
                class_iri: Some(neighbor.class_iri),
                ontology_id: Some(neighbor.ontology_id),
                rung: BindingRung::Semantic,
                score,
                model: model_id.clone(),
                proposal_item_id: None,
                entities_stamped: 0,
                recorded: false,
            }),
            None => {
                // Rung 4: the term stays free text. With a citation, the
                // extension path is the SAME governance queue propose_class
                // feeds; promotion stays human. Parents are deliberately not
                // auto-suggested — the proposal identity is label+parents,
                // and a geometry-guessed parent would fragment the identity
                // the paper agent's own proposals accumulate under.
                let nearest_note = match (&neighbor, &model_id) {
                    (Some(n), _) => format!(
                        " Nearest loaded class label: {:?} <{}> from ontology '{}' at \
                         similarity {:.3} (threshold {threshold}).",
                        n.label, n.class_iri, n.ontology_id, n.similarity
                    ),
                    (None, Some(_)) => " No stored class-label geometry answered.".to_string(),
                    (None, None) => " No embedding backend was available.".to_string(),
                };
                let proposal_item_id = match &pending.citation {
                    Some(citation) => {
                        let proposal = OntologyClassProposal {
                            label: pending.verbatim.clone(),
                            proposed_iri: None,
                            parent_iris: Vec::new(),
                            description: Some(format!(
                                "Resolution-ladder proposal: the measured property term \
                                 {:?} bound to no loaded ontology class.{nearest_note}",
                                pending.verbatim
                            )),
                            citation: citation.clone(),
                        };
                        let (item, citation_json) =
                            class_proposal_queue_item(&proposal, document, tenant, now_secs);
                        match store
                            .enqueue_ontology_proposal(&item, &citation_json, now_secs)
                            .await?
                        {
                            prism_provenance::OntologyProposalEnqueue::SupersededByDisposition => {
                                None
                            }
                            _ => Some(item.item_id),
                        }
                    }
                    None => None,
                };
                outcomes.push(PropertyBinding {
                    term: pending.verbatim,
                    canonical: pending.canonical,
                    class_iri: None,
                    ontology_id: None,
                    rung: BindingRung::Proposed,
                    score,
                    model: score.and(model_id.clone()),
                    proposal_item_id,
                    entities_stamped: 0,
                    recorded: false,
                });
            }
        }
    }

    // Persist every outcome and stamp the graph for the ones that bound.
    let resolved_at = chrono::Utc::now().to_rfc3339();
    for outcome in &mut outcomes {
        let geometry_consulted =
            matches!(outcome.rung, BindingRung::Semantic | BindingRung::Proposed);
        outcome.recorded = store
            .record_term_binding(&TermBinding {
                tenant: tenant.to_string(),
                term: outcome.canonical.clone(),
                verbatim: outcome.term.clone(),
                class_iri: outcome.class_iri.clone(),
                ontology_id: outcome.ontology_id.clone(),
                rung: outcome.rung.as_i64(),
                score: outcome.score,
                threshold: geometry_consulted.then_some(threshold),
                model: outcome.model.clone(),
                proposal_item_id: outcome.proposal_item_id.clone(),
                resolved_at: resolved_at.clone(),
            })
            .await?;
        if outcome.recorded
            && let Some(class_iri) = &outcome.class_iri
        {
            outcome.entities_stamped = store
                .apply_term_binding_to_entities(tenant, &outcome.canonical, class_iri)
                .await?;
        }
    }
    Ok(outcomes)
}

/// Convenience wrapper for callers that hold `Arc<dyn EmbedBackend>`.
pub async fn resolve_property_terms_arc(
    store: &ProvenanceStore,
    ontologies: &OntologySet,
    backend: Option<&Arc<dyn prism_embed::EmbedBackend>>,
    tenant: &str,
    document: &str,
    terms: &[PropertyTerm],
    threshold: f64,
) -> Result<Vec<PropertyBinding>> {
    resolve_property_terms(
        store,
        ontologies,
        backend.map(|backend| backend.as_ref() as &dyn prism_embed::EmbedBackend),
        tenant,
        document,
        terms,
        threshold,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ontologies::{EmmoOntology, MatKgOntology, Ontology};
    use std::path::PathBuf;

    /// The REAL loaded union the live paper path uses: bundled EMMO primary,
    /// bundled MatKG second. No lookup table is built by the tests — every
    /// candidate label comes from the integrity-checked artifacts.
    fn loaded_set() -> OntologySet {
        OntologySet::new(vec![
            Arc::new(EmmoOntology) as Arc<dyn Ontology>,
            Arc::new(MatKgOntology) as Arc<dyn Ontology>,
        ])
        .expect("built-in ontologies form a valid set")
    }

    /// A measured fact whose object is whatever the model wrote there.
    fn measured(predicate: &str, object: &str, value: f64) -> prism_provenance::MaterialFact {
        prism_provenance::MaterialFact {
            subject: "HfNbTaTiZr".into(),
            predicate: predicate.into(),
            object: object.into(),
            value: Some(value),
            unit: Some(
                prism_provenance::UnitTerm::new("HV30").expect("HV30 is a usable unit spelling"),
            ),
            conditions: Vec::new(),
            confidence: None,
            kind: Some("measurement".into()),
            evidence_class: prism_provenance::EvidenceClass::Research,
            verification: None,
            verification_reason: None,
        }
    }

    /// The object of a measured fact that OPENS with the fact's own number
    /// restates the measurement — it is a value, not a property name, and
    /// must never reach the governance queue as a class proposal. Measured
    /// live: 8 of 21 terms from one paper were strings of this shape.
    #[test]
    fn object_restating_the_measured_value_is_not_a_term() {
        let set = loaded_set();
        let mut terms = Vec::new();
        property_terms_for_fact(
            &set,
            &measured(
                "Vickers hardness",
                "485 HV30 (highest among the alloys studied)",
                485.0,
            ),
            None,
            None,
            None,
            &mut terms,
        );
        let collected: Vec<&str> = terms.iter().map(|t| t.term.as_str()).collect();
        assert_eq!(
            collected,
            vec!["Vickers hardness"],
            "the predicate names the property; the object restates the value"
        );
    }

    /// The guard keys on the fact's OWN number, so a genuine property name
    /// that merely BEGINS with a different number still passes. Without
    /// this, "0.2% proof stress" would be silently discarded.
    #[test]
    fn number_led_property_name_is_still_a_term() {
        let set = loaded_set();
        let mut terms = Vec::new();
        property_terms_for_fact(
            &set,
            &measured("exhibits", "0.2% proof stress", 1100.0),
            None,
            None,
            None,
            &mut terms,
        );
        assert!(
            terms.iter().any(|t| t.term == "0.2% proof stress"),
            "0.2 is not the measured value 1100, so this is a property name: {:?}",
            terms.iter().map(|t| &t.term).collect::<Vec<_>>()
        );
    }

    #[test]
    fn leading_number_reads_only_an_opening_literal() {
        assert_eq!(leading_number("485 HV30 (highest)"), Some(485.0));
        assert_eq!(leading_number("0.2% proof stress"), Some(0.2));
        assert_eq!(leading_number("-3.5 mm shrinkage"), Some(-3.5));
        assert_eq!(leading_number("yield strength"), None);
        assert_eq!(leading_number(""), None);
        // A bare unit prefix is not a number.
        assert_eq!(leading_number("HV30 hardness"), None);
    }

    struct TempDb {
        path: PathBuf,
    }

    impl TempDb {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "prism_property_resolution_test_{}.db",
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

    /// Deterministic geometry fixture. It supplies VECTORS only — candidate
    /// labels, the KNN, the threshold gate, and the records all run through
    /// the real resolver, the real ontology set, and the real store SQL.
    struct FixtureEmbed {
        vectors: std::collections::HashMap<String, Vec<f32>>,
    }

    impl FixtureEmbed {
        fn new(entries: &[(&str, [f32; 4])]) -> Self {
            Self {
                vectors: entries
                    .iter()
                    .map(|(text, vector)| ((*text).to_string(), vector.to_vec()))
                    .collect(),
            }
        }
    }

    #[async_trait::async_trait]
    impl prism_embed::EmbedBackend for FixtureEmbed {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|text| {
                    self.vectors
                        .get(text)
                        .cloned()
                        .unwrap_or_else(|| vec![0.0, 0.0, 1.0, 0.0])
                })
                .collect())
        }
        fn dimensions(&self) -> usize {
            4
        }
        fn id(&self) -> &str {
            "test:fixture"
        }
    }

    fn term(text: &str) -> PropertyTerm {
        PropertyTerm {
            term: text.into(),
            citation: None,
        }
    }

    fn citation() -> PaperCitation {
        PaperCitation {
            source_revision_id: "ab".repeat(32),
            from_line: 12,
            to_line: 14,
            quoted_text: "the crack-growth resistance reached almost 950 kJ m^-2".into(),
        }
    }

    async fn store() -> (TempDb, ProvenanceStore) {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        (db, store)
    }

    /// Rung 1 against the real union: an EMMO `skos:prefLabel` that is NOT
    /// an extraction label binds exactly, and a term only MatKG declares
    /// binds THROUGH the union with its declaring ontology recorded — the
    /// primary alone must not be the candidate universe.
    #[tokio::test]
    async fn exact_labels_bind_across_the_union_and_name_their_ontology() {
        let (_db, store) = store().await;
        let set = loaded_set();
        let emmo = &set.all()[0];
        assert!(
            emmo.class_for_label("MetallicMaterial").is_none(),
            "premise: MetallicMaterial is navigable-only, not an extraction label"
        );

        let bindings = resolve_property_terms(
            &store,
            &set,
            None,
            "local",
            "doc-1",
            &[term("MetallicMaterial"), term("Descriptor")],
            DEFAULT_SEMANTIC_BIND_THRESHOLD,
        )
        .await
        .unwrap();

        assert_eq!(bindings.len(), 2);
        let metallic = &bindings[0];
        assert_eq!(metallic.rung, BindingRung::Exact);
        assert_eq!(metallic.ontology_id.as_deref(), Some("emmo"));
        assert!(
            metallic
                .class_iri
                .as_deref()
                .is_some_and(|iri| iri.starts_with("https://w3id.org/emmo#")),
            "{metallic:?}"
        );
        assert_eq!(metallic.score, None, "deterministic rungs carry no score");

        let descriptor = &bindings[1];
        assert_eq!(descriptor.rung, BindingRung::Exact);
        assert_eq!(descriptor.ontology_id.as_deref(), Some("matkg"));
        assert_eq!(
            descriptor.class_iri.as_deref(),
            Some("https://marc27.com/ontology/matkg#Descriptor")
        );

        // The rows are durable and carry the rung.
        let row = store
            .term_binding("local", "metallicmaterial")
            .await
            .unwrap()
            .expect("binding row persisted");
        assert_eq!(row.rung, prism_provenance::TERM_BINDING_RUNG_EXACT);
        assert_eq!(row.ontology_id.as_deref(), Some("emmo"));
    }

    /// A label declared by BOTH loaded ontologies resolves to the EARLIEST
    /// loaded declaration — the set-order collision rule, not alphabet luck.
    #[tokio::test]
    async fn label_collisions_resolve_in_set_order() {
        let (_db, store) = store().await;
        let set = loaded_set();
        let (declaring, decl) = set.class_for_label("Property").expect("Property resolves");
        assert_eq!(declaring.id(), "emmo", "premise: EMMO is loaded first");

        let bindings = resolve_property_terms(
            &store,
            &set,
            None,
            "local",
            "doc-1",
            &[term("Property")],
            DEFAULT_SEMANTIC_BIND_THRESHOLD,
        )
        .await
        .unwrap();
        assert_eq!(bindings[0].ontology_id.as_deref(), Some("emmo"));
        assert_eq!(bindings[0].class_iri.as_deref(), Some(decl.iri.as_str()));
    }

    /// Rung 2: case, hyphenation, CamelCase, and plural spelling all fold
    /// onto the declared label without a domain spelling table.
    #[tokio::test]
    async fn normalised_spellings_bind_to_the_declared_class() {
        let (_db, store) = store().await;
        let set = loaded_set();

        let bindings = resolve_property_terms(
            &store,
            &set,
            None,
            "local",
            "doc-1",
            &[
                term("metallic-materials"),
                term("CHEMICAL ELEMENT"),
                term("phases of matter"),
            ],
            DEFAULT_SEMANTIC_BIND_THRESHOLD,
        )
        .await
        .unwrap();

        for binding in &bindings {
            assert_eq!(
                binding.rung,
                BindingRung::Normalized,
                "expected a rung-2 bind: {binding:?}"
            );
            assert_eq!(binding.ontology_id.as_deref(), Some("emmo"), "{binding:?}");
        }
        let row = store
            .term_binding("local", "metallic-materials")
            .await
            .unwrap()
            .expect("row persisted under the canonical spelling");
        assert_eq!(row.rung, prism_provenance::TERM_BINDING_RUNG_NORMALIZED);
        assert_eq!(row.verbatim, "metallic-materials");
    }

    /// Rung 3 drives the REAL path end to end: labels of the real union are
    /// seeded through the store, the term embeds, Turso's
    /// `vector_distance_cos` ranks, the threshold gates, the row records
    /// rung AND score, and the bind stamps a graph entity whose class was
    /// unknown.
    #[tokio::test]
    async fn semantic_bind_records_score_and_stamps_unclassified_entities() {
        let (_db, store) = store().await;
        let set = loaded_set();
        // cos("yield strength", "property") = 0.9; every other label sits
        // on an orthogonal fallback vector.
        let backend = FixtureEmbed::new(&[
            ("property", [1.0, 0.0, 0.0, 0.0]),
            ("yield strength", [0.9, 0.435_889_9, 0.0, 0.0]),
        ]);

        // The property node a measurement wrote, class unknown.
        store
            .write_extracted_entity("yield strength", "Entity", None, "local")
            .await
            .unwrap();

        let bindings = resolve_property_terms(
            &store,
            &set,
            Some(&backend),
            "local",
            "doc-1",
            &[term("yield strength")],
            0.85,
        )
        .await
        .unwrap();

        let binding = &bindings[0];
        assert_eq!(binding.rung, BindingRung::Semantic);
        let expected_iri = set.class_for_label("Property").unwrap().1.iri.as_str();
        assert_eq!(binding.class_iri.as_deref(), Some(expected_iri));
        assert_eq!(binding.ontology_id.as_deref(), Some("emmo"));
        let score = binding.score.expect("a semantic bind records its score");
        assert!((score - 0.9).abs() < 1e-5, "score {score} is the cosine");
        assert_eq!(binding.model.as_deref(), Some("test:fixture"));
        assert_eq!(binding.entities_stamped, 1, "the unknown node was stamped");

        let row = store
            .term_binding("local", "yield strength")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.rung, prism_provenance::TERM_BINDING_RUNG_SEMANTIC);
        assert!((row.score.unwrap() - 0.9).abs() < 1e-5);
        assert_eq!(row.threshold, Some(0.85));
        assert_eq!(row.model.as_deref(), Some("test:fixture"));

        // The union's labels were seeded exactly once per (ontology, model).
        assert!(
            !store
                .class_label_embedding_keys("emmo", "test:fixture")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !store
                .class_label_embedding_keys("matkg", "test:fixture")
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// Rung 4, complete: the fact written BEFORE resolution is still in the
    /// graph afterwards, the term is recorded unbound WITH its
    /// below-threshold score, and one class proposal with the fact's own
    /// citation sits in the SAME governance queue `propose_class` feeds —
    /// promotion stays human.
    #[tokio::test]
    async fn below_threshold_keeps_the_fact_and_queues_a_cited_proposal() {
        let (_db, store) = store().await;
        let set = loaded_set();
        let backend = FixtureEmbed::new(&[
            ("property", [1.0, 0.0, 0.0, 0.0]),
            ("crack growth resistance", [0.4, 0.916_515_1, 0.0, 0.0]),
        ]);

        // The fact, written first — resolution must never discard it.
        let prov = prism_provenance::LocalProvenance {
            activity_id: "activity-1".into(),
            agent_id: "test-agent".into(),
            agent_kind: "SoftwareAgent".into(),
            source_entity_id: "doc-1".into(),
            source_kind: "Document".into(),
            tenant: "local".into(),
            started_at: "2026-08-24T00:00:00Z".into(),
            ended_at: "2026-08-24T00:00:00Z".into(),
            locality: "local".into(),
            origin_source_id: None,
        };
        store.record_activity(&prov).await.unwrap();
        store
            .write_fact(
                &prism_provenance::LocalFact {
                    subject: "34CrNiMo6 steel".into(),
                    predicate: "crack-growth resistance".into(),
                    object: "crack-growth resistance".into(),
                    value: Some(950.0),
                    unit: Some("kJ/m^2".into()),
                    confidence: Some(0.8),
                    kind: None,
                },
                &prov,
            )
            .await
            .unwrap();

        let bindings = resolve_property_terms(
            &store,
            &set,
            Some(&backend),
            "local",
            "doc-1",
            &[PropertyTerm {
                term: "crack-growth resistance".into(),
                citation: Some(citation()),
            }],
            0.85,
        )
        .await
        .unwrap();

        let binding = &bindings[0];
        assert_eq!(binding.rung, BindingRung::Proposed);
        assert_eq!(binding.class_iri, None);
        let score = binding
            .score
            .expect("the below-threshold score is recorded, not discarded");
        assert!((score - 0.4).abs() < 1e-5, "score {score}");
        let item_id = binding
            .proposal_item_id
            .as_deref()
            .expect("a cited rung-4 term queues a proposal");

        // The fact survived, free-text spelling intact.
        let neighbors = store
            .get_neighbors("34CrNiMo6 steel", None, "local", 10)
            .await
            .unwrap();
        assert!(
            neighbors
                .edges
                .iter()
                .any(|edge| edge.rel_type == "crack-growth resistance"),
            "the fact must still be in the graph: {:?}",
            neighbors.edges
        );

        // The proposal is real, cited, and pending for a HUMAN.
        let pending = store.pending_ontology_proposals(10).await.unwrap();
        let (item, sightings) = pending
            .iter()
            .find(|(item, _)| item.item_id == item_id)
            .expect("the ladder's proposal is queued");
        assert_eq!(item.kind, "class");
        assert_eq!(item.label, "crack-growth resistance");
        assert_eq!(*sightings, 1);

        // And the durable record supports re-resolution: the term lists as
        // unbound with its score, threshold, and pending proposal id.
        let unbound = store.unbound_term_bindings("local").await.unwrap();
        assert_eq!(unbound.len(), 1, "{unbound:?}");
        assert_eq!(unbound[0].term, "crack-growth resistance");
        assert!((unbound[0].score.unwrap() - 0.4).abs() < 1e-5);
        assert_eq!(unbound[0].threshold, Some(0.85));
        assert_eq!(unbound[0].proposal_item_id.as_deref(), Some(item_id));
    }

    /// Without an embedding backend the ladder degrades honestly: lexical
    /// misses are recorded unbound with NO score (silence, not a guess), and
    /// an uncited term queues nothing while still leaving the re-resolution
    /// record.
    #[tokio::test]
    async fn no_backend_records_unbound_terms_without_scores_or_proposals() {
        let (_db, store) = store().await;
        let bindings = resolve_property_terms(
            &store,
            &loaded_set(),
            None,
            "local",
            "doc-1",
            &[term("laser absorptivity")],
            DEFAULT_SEMANTIC_BIND_THRESHOLD,
        )
        .await
        .unwrap();
        assert_eq!(bindings[0].rung, BindingRung::Proposed);
        assert_eq!(bindings[0].score, None);
        assert_eq!(bindings[0].proposal_item_id, None);
        assert!(
            store
                .pending_ontology_proposals(10)
                .await
                .unwrap()
                .is_empty()
        );
        let unbound = store.unbound_term_bindings("local").await.unwrap();
        assert_eq!(unbound.len(), 1);
        assert_eq!(unbound[0].score, None);
    }

    /// A bind never clobbers an extraction-declared classification: only
    /// NULL `class_iri` rows are stamped.
    #[tokio::test]
    async fn binds_never_overwrite_declared_classifications() {
        let (_db, store) = store().await;
        let set = loaded_set();
        store
            .write_classified_entity(
                "Property",
                prism_provenance::ClassifiedNode {
                    entity_type: "Property",
                    storage_label: "Property",
                    class_iri: "https://example.test/AlreadyDeclared",
                },
                None,
                "local",
            )
            .await
            .unwrap();

        let bindings = resolve_property_terms(
            &store,
            &set,
            None,
            "local",
            "doc-1",
            &[term("Property")],
            DEFAULT_SEMANTIC_BIND_THRESHOLD,
        )
        .await
        .unwrap();
        assert_eq!(
            bindings[0].rung,
            BindingRung::Exact,
            "the bind itself lands"
        );
        assert_eq!(
            bindings[0].entities_stamped, 0,
            "the declared node was not touched"
        );
    }

    /// The value-shape guard: number-with-unit strings restate the value and
    /// are not property names; number-led NAMES with a longer tail pass.
    #[test]
    fn value_shaped_strings_are_not_property_names() {
        assert!(!is_property_name_shaped("1100"));
        assert!(!is_property_name_shaped("950 MPa"));
        assert!(!is_property_name_shaped("8.19g/cm3"));
        assert!(!is_property_name_shaped("   "));
        assert!(is_property_name_shaped("0.2% proof stress"));
        assert!(is_property_name_shaped("yield strength"));
        assert!(is_property_name_shaped("almost 950 kJ m^-2 of resistance"));
    }

    #[test]
    fn normalization_folds_camel_case_punctuation_and_case() {
        assert_eq!(normalize_term("MetallicMaterial"), "metallic material");
        assert_eq!(normalize_term("metallic-material"), "metallic material");
        assert_eq!(normalize_term("METALLIC  MATERIAL"), "metallic material");
        assert_eq!(
            normalize_term("crack-growth resistance"),
            "crack growth resistance"
        );
        assert_eq!(singular("metallic materials"), "metallic material");
        assert_eq!(
            singular("hardness"),
            "hardness",
            "'ss' words are not clipped"
        );
        assert_eq!(singular("gas"), "gas", "short words are not clipped");
    }
}
