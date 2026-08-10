//! What KIND of thing is this? — deciding an extracted entity's ontology class.
//!
//! Until now the document path never asked. Every fact's subject was written
//! under a hardcoded `Matter` (`emmo.rs`, `EntityWrite::legacy("Matter")`) and
//! its object's label was inferred from the fact's `kind`, so a knowledge graph
//! built from a NASA rocket-engine paper recorded `Laser Powder Bed Fusion` as
//! a MATERIAL. The tabular and MatKG paths have always classified their
//! entities through [`ProvenanceStore::write_classified_fact_with_evidence`];
//! the document path had not, and nothing made the two agree.
//!
//! # The model classifies; priors are evidence, not instructions
//!
//! The class is not derived by code from the fact's shape, and it is not looked
//! up in a table that overrides the model. Both were tried and both are wrong:
//!
//! - Deriving it from position (subject ⇒ `Matter`) is what produced the bug.
//! - Overriding the model from a corpus table is worse than it sounds. MatKG,
//!   the largest materials prior available, labels `selective laser melting` a
//!   SymmetryPhaseLabel in 310 of 310 mentions and `laser powder bed fusion` an
//!   Application in 64 of 64. Applying that verbatim would make the graph less
//!   correct, not more.
//!
//! So the prior goes into the PROMPT as evidence, and the model decides.
//! Measured on `gemma-4-12b` with the MatKG evidence above in context, it
//! answered `Manufacturing (a process)` and explained itself: *"It is an
//! additive manufacturing process, overriding the MatKG 'Application' label."*
//! A capable model arbitrates a noisy corpus; a lookup table cannot, and the
//! override is recorded rather than silent.
//!
//! # Cost
//!
//! ONE call per document, over the document's distinct entity names — not one
//! per entity, and not one per fact. At a million papers the difference between
//! per-document and per-entity is the difference between feasible and not.

use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result};
use prism_llm::LlmClient;
use serde::Deserialize;

use crate::ontologies::Ontology;

/// An entity's class, resolved against the ACTIVE ontology.
///
/// Owned rather than borrowed because it is built from model output and then
/// handed to the store per fact; `ClassifiedNode` borrows from it at the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityClass {
    /// What extraction declared, e.g. `"Manufacturing"`. Kept distinct from
    /// the storage label so a synonym is not erased by the write.
    pub entity_type: String,
    /// The label this is persisted under (part of the entity key).
    pub storage_label: String,
    /// Canonical vocabulary identity.
    pub class_iri: String,
}

/// What a corpus has previously called this entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorEvidence {
    /// Human label of the class the corpus most often assigned.
    pub label: String,
    /// Mentions carrying that label, and mentions in total. `seen` == `total`
    /// means the corpus is unanimous, which is NOT the same as being right —
    /// MatKG's NER assigns one label per string and repeats it.
    pub seen: u32,
    pub total: u32,
}

/// A source of prior evidence about entity classes.
///
/// An adapter surface like every other plane here: MatKG is one prior, a
/// domain's own curated corpus is another, and a deployment may register its
/// own without PRISM knowing anything about it.
pub trait ClassPrior: Send + Sync {
    /// Stable machine id, recorded so a classification can name the evidence
    /// that informed it.
    fn id(&self) -> &str;

    /// What this prior has seen for `name`, or `None` if it has never seen it.
    /// Absence is itself evidence and is stated to the model as such.
    fn lookup(&self, name: &str) -> Option<PriorEvidence>;
}

#[derive(Deserialize)]
struct ClassificationReply {
    #[serde(default)]
    classifications: Vec<OneClassification>,
}

#[derive(Deserialize)]
struct OneClassification {
    #[serde(default)]
    term: String,
    #[serde(default)]
    class: String,
}

/// Classify every distinct entity name in one call.
///
/// Returns only the entities the model classified into a class the ACTIVE
/// ontology actually declares. An unrecognised class is dropped rather than
/// invented into the vocabulary — the caller falls back to the store's
/// established shape for those, so a model that answers nonsense degrades to
/// today's behaviour instead of corrupting the class space.
pub async fn classify_entities(
    llm: &LlmClient,
    ontology: &dyn Ontology,
    names: &[String],
    prior: Option<&dyn ClassPrior>,
) -> Result<HashMap<String, EntityClass>> {
    let names: Vec<&String> = names.iter().collect::<BTreeSet<_>>().into_iter().collect();
    if names.is_empty() {
        return Ok(HashMap::new());
    }

    let prompt = build_prompt(ontology, &names, prior);
    let raw = llm
        .generate_json(&prompt)
        .await
        .context("classifying extracted entities")?;
    let reply: ClassificationReply = serde_json::from_str(strip_fence(&raw))
        .with_context(|| format!("parsing entity classification reply: {raw:.400}"))?;

    let mut out = HashMap::new();
    for item in reply.classifications {
        let Some(class) = resolve(ontology, &item.class) else {
            // Not a class this ontology declares. Reported by the caller as a
            // fact written under the default shape, never as a new class.
            tracing::debug!(
                term = %item.term,
                class = %item.class,
                "model proposed a class the active ontology does not declare",
            );
            continue;
        };
        // Match back to the exact extracted name; the model is asked to echo
        // terms verbatim, but case and surrounding space are not identity.
        if let Some(name) = names
            .iter()
            .find(|n| n.trim().eq_ignore_ascii_case(item.term.trim()))
        {
            out.insert((*name).clone(), class);
        }
    }
    Ok(out)
}

/// Resolve a model-proposed class name to a declared class of this ontology.
///
/// Accepts the class's own `pref_label` and any of its curated
/// `extraction_labels`, so the vocabulary the prompt advertises and the
/// vocabulary the resolver accepts cannot drift apart — they are read from the
/// same declaration.
fn resolve(ontology: &dyn Ontology, proposed: &str) -> Option<EntityClass> {
    // Models append parentheticals ("Manufacturing (a process)"); the class
    // name is the part before it.
    let proposed = proposed.split('(').next().unwrap_or(proposed).trim();
    if proposed.is_empty() {
        return None;
    }
    for class in ontology.classes() {
        let matches = class
            .pref_label
            .as_deref()
            .is_some_and(|l| l.eq_ignore_ascii_case(proposed))
            || class
                .extraction_labels
                .iter()
                .any(|l| l.eq_ignore_ascii_case(proposed));
        if !matches {
            continue;
        }
        // The EXTRACTION label is the name the rest of the pipeline speaks,
        // and the only key `storage_label` accepts. Using the prefLabel here
        // resolved `MetallicMaterial` to no storage label and would have
        // minted it as a NEW label in the entity key space — a silent
        // vocabulary fork, in the identity half of the key.
        let entity_type = class
            .extraction_labels
            .iter()
            .find(|l| l.eq_ignore_ascii_case(proposed))
            .or_else(|| class.extraction_labels.first())
            .cloned()
            .or_else(|| class.pref_label.clone())?;
        let storage_label = ontology
            .storage_label(&entity_type)
            .unwrap_or(&entity_type)
            .to_string();
        return Some(EntityClass {
            entity_type,
            storage_label,
            class_iri: class.iri.as_str().to_string(),
        });
    }
    None
}

fn build_prompt(
    ontology: &dyn Ontology,
    names: &[&String],
    prior: Option<&dyn ClassPrior>,
) -> String {
    // The allowed vocabulary comes from the ACTIVE ontology's declarations, so
    // swapping the ontology swaps what the model may answer — no class name is
    // written into this prompt.
    let vocabulary: Vec<String> = ontology
        .classes()
        .iter()
        .filter_map(|c| {
            c.extraction_labels
                .first()
                .cloned()
                .or_else(|| c.pref_label.clone())
        })
        .collect();

    let mut lines = vec![
        "You are classifying materials-science terms into an ontology.".to_string(),
        String::new(),
        format!(
            "Allowed classes (use these EXACT names): {}",
            vocabulary.join(", ")
        ),
        String::new(),
    ];

    if prior.is_some() {
        lines.push(
            "Some terms carry EVIDENCE: how that exact string was labelled by named-entity \
             recognition across a large literature corpus. This evidence is automatically \
             generated and is often WRONG for specialised subfields — weigh it against what \
             you know the term actually is, and override it when you know better."
                .to_string(),
        );
        lines.push(String::new());
    }

    lines.push("Terms:".to_string());
    for name in names {
        match prior.and_then(|p| p.lookup(name)) {
            Some(evidence) => lines.push(format!(
                "- {name}   [corpus evidence: {} in {}/{} mentions]",
                evidence.label, evidence.seen, evidence.total
            )),
            None if prior.is_some() => lines.push(format!("- {name}   [corpus evidence: none]")),
            None => lines.push(format!("- {name}")),
        }
    }

    lines.push(String::new());
    lines.push(
        "Reply with ONLY this JSON, one entry per term, echoing each term verbatim:\n\
         {\"classifications\":[{\"term\":\"...\",\"class\":\"...\"}]}"
            .to_string(),
    );
    lines.join("\n")
}

/// Strip a ```json fence if the model wrapped its reply in one.
fn strip_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    rest.trim().trim_end_matches("```").trim()
}

/// A [`ClassPrior`] backed by a corpus already loaded into the graph.
///
/// PRE-FETCHED on construction, one lookup per distinct name, then answered
/// from memory. Two reasons: [`ClassPrior::lookup`] stays synchronous and
/// trivially testable, and a million-paper run does one bounded round of
/// queries per DOCUMENT rather than a store round-trip per entity per fact.
///
/// The corpus is whatever tenant is named — `local@matkg` for MatKG, loaded by
/// [`crate::matkg::load`]. Nothing here knows it is MatKG.
pub struct GraphPrior {
    id: String,
    seen: HashMap<String, PriorEvidence>,
}

impl GraphPrior {
    /// Look up every name in `tenant` and hold the answers.
    ///
    /// An entity the corpus has never seen is simply absent, and absence is
    /// reported to the model as evidence in its own right — "this term is not
    /// in the corpus" is a real signal about a novel material.
    pub async fn fetch(
        store: &prism_provenance::ProvenanceStore,
        tenant: &str,
        names: &[String],
    ) -> Result<Self> {
        let mut seen = HashMap::new();
        for name in names {
            let hits = store
                .graph_search_scoped(name, &[tenant], 8)
                .await
                .with_context(|| format!("looking up '{name}' in the '{tenant}' prior"))?;
            // graph_search matches a substring; only an EXACT name is evidence
            // about THIS entity. A prior that answered about a different
            // material would be worse than no prior.
            let Some(hit) = hits
                .iter()
                .find(|h| h.name.trim().eq_ignore_ascii_case(name.trim()))
            else {
                continue;
            };
            let total = hits
                .iter()
                .filter(|h| h.name.trim().eq_ignore_ascii_case(name.trim()))
                .count() as u32;
            seen.insert(
                name.clone(),
                PriorEvidence {
                    label: hit.entity_type.clone(),
                    seen: total,
                    total,
                },
            );
        }
        Ok(Self {
            id: tenant.to_string(),
            seen,
        })
    }

    /// How many of the requested names the corpus knew. Reported so a run can
    /// say "the prior covered 12 of 30 terms" instead of implying it informed
    /// every classification.
    pub fn coverage(&self) -> usize {
        self.seen.len()
    }
}

impl ClassPrior for GraphPrior {
    fn id(&self) -> &str {
        &self.id
    }

    fn lookup(&self, name: &str) -> Option<PriorEvidence> {
        self.seen.get(name).cloned().or_else(|| {
            self.seen
                .iter()
                .find(|(k, _)| k.trim().eq_ignore_ascii_case(name.trim()))
                .map(|(_, v)| v.clone())
        })
    }
}

#[cfg(test)]
mod tests {

    /// The storage label is IDENTITY — it is part of `entity_key` — so it must
    /// come from the ontology's own mapping, keyed by the EXTRACTION label.
    /// Resolving it from the prefLabel instead returned nothing for
    /// `MetallicMaterial` and would have written `MetallicMaterial` into the
    /// key space as a new label, forking the vocabulary silently.
    #[test]
    fn the_storage_label_comes_from_the_ontology_never_from_the_class_name() {
        let ontology = emmo();
        for class in ontology.classes() {
            let Some(extraction) = class.extraction_labels.first() else {
                continue;
            };
            let resolved = resolve(ontology.as_ref(), extraction)
                .unwrap_or_else(|| panic!("'{extraction}' must resolve"));
            assert_eq!(
                &resolved.entity_type, extraction,
                "extraction label is kept"
            );
            let declared = ontology
                .storage_label(extraction)
                .unwrap_or(extraction.as_str());
            assert_eq!(
                resolved.storage_label, declared,
                "'{extraction}' must persist under the ontology's declared label",
            );
        }
        // The concrete case that was wrong: an alloy stores as Matter.
        let alloy = resolve(ontology.as_ref(), "Alloy").expect("Alloy is declared");
        assert_eq!(alloy.entity_type, "Alloy");
        assert_eq!(
            alloy.storage_label,
            ontology.storage_label("Alloy").unwrap_or("Alloy"),
        );
        assert_ne!(
            alloy.storage_label, "MetallicMaterial",
            "the class NAME must never become a storage label",
        );
    }
    use super::*;
    use crate::ontologies;

    fn emmo() -> std::sync::Arc<dyn Ontology> {
        ontologies::active(Some(ontologies::DEFAULT_ONTOLOGY_ID)).expect("built-in EMMO")
    }

    struct Fake(&'static [(&'static str, &'static str, u32)]);
    impl ClassPrior for Fake {
        fn id(&self) -> &str {
            "fake"
        }
        fn lookup(&self, name: &str) -> Option<PriorEvidence> {
            self.0
                .iter()
                .find(|(n, _, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, label, seen)| PriorEvidence {
                    label: (*label).to_string(),
                    seen: *seen,
                    total: *seen,
                })
        }
    }

    /// The prompt's vocabulary is READ FROM the active ontology, so a model
    /// can never be invited to answer a class the store cannot persist. This
    /// is the property that keeps prompt and validator from drifting — the
    /// defect that made every phase fact in every store non-compliant.
    #[test]
    fn the_prompt_offers_exactly_the_ontologys_own_classes() {
        let ontology = emmo();
        let names = ["Ti-6Al-4V".to_string()];
        let refs: Vec<&String> = names.iter().collect();
        let prompt = build_prompt(ontology.as_ref(), &refs, None);

        for class in ontology.classes() {
            let Some(label) = class
                .extraction_labels
                .first()
                .cloned()
                .or_else(|| class.pref_label.clone())
            else {
                continue;
            };
            assert!(
                prompt.contains(&label),
                "the prompt must advertise declared class '{label}'",
            );
        }
        assert!(prompt.contains("Ti-6Al-4V"));
    }

    /// Prior evidence reaches the model as EVIDENCE, with the explicit licence
    /// to override it. Measured need: MatKG calls `laser powder bed fusion` an
    /// Application 64/64 times, and the correct answer is a process.
    #[test]
    fn prior_evidence_is_offered_with_permission_to_override() {
        let ontology = emmo();
        let names = [
            "laser powder bed fusion".to_string(),
            "Unheardof-9000".to_string(),
        ];
        let refs: Vec<&String> = names.iter().collect();
        let prior = Fake(&[("laser powder bed fusion", "Application", 64)]);
        let prompt = build_prompt(ontology.as_ref(), &refs, Some(&prior));

        assert!(prompt.contains("Application in 64/64 mentions"), "{prompt}");
        assert!(
            prompt.contains("override it when you know better"),
            "{prompt}"
        );
        // Absence is stated, not silently omitted.
        assert!(
            prompt.contains("Unheardof-9000   [corpus evidence: none]"),
            "{prompt}"
        );
    }

    /// A class the ontology does not declare is DROPPED, never minted. A model
    /// answering nonsense must degrade to the store's established shape, not
    /// invent vocabulary.
    #[test]
    fn an_undeclared_class_is_refused() {
        let ontology = emmo();
        assert!(resolve(ontology.as_ref(), "Spaceship").is_none());
        assert!(resolve(ontology.as_ref(), "").is_none());
    }

    /// Every declared class resolves BY ITS OWN ADVERTISED NAME — the exact
    /// round trip the prompt promises. Derived from the ontology, so adding a
    /// class cannot leave the resolver behind.
    #[test]
    fn every_advertised_class_resolves_back() {
        let ontology = emmo();
        for class in ontology.classes() {
            let Some(label) = class
                .extraction_labels
                .first()
                .cloned()
                .or_else(|| class.pref_label.clone())
            else {
                continue;
            };
            let resolved = resolve(ontology.as_ref(), &label)
                .unwrap_or_else(|| panic!("advertised class '{label}' must resolve"));
            assert_eq!(resolved.class_iri, class.iri.as_str());
            assert!(!resolved.storage_label.is_empty());
        }
    }

    /// Models append parentheticals; the class name is what precedes them.
    #[test]
    fn a_parenthesised_answer_still_resolves() {
        let ontology = emmo();
        let plain = resolve(ontology.as_ref(), "Process").expect("Process is declared");
        let noisy =
            resolve(ontology.as_ref(), "Process (a manufacturing step)").expect("same class");
        assert_eq!(plain, noisy);
    }

    #[test]
    fn fenced_json_is_accepted() {
        assert_eq!(strip_fence("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_fence("  {\"a\":1}  "), "{\"a\":1}");
    }
}
