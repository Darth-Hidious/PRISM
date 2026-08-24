//! The TTL artifact — the contract between induction and the OWL loader.
//!
//! [`to_turtle`] serialises an [`InducedOntology`] deterministically:
//! prefixes, ontology header (version IRI, status, provenance link),
//! induction activity (model, prompt version, corpus hash, counts, every
//! normalisation the builder performed), then classes and relations sorted
//! by minted local name. Same ontology in, same bytes out.
//!
//! [`parse_turtle`] reconstructs the ontology from any Turtle document that
//! carries the same shapes (via `sophia`), so the artifact — not this
//! crate's structs — is the durable interface.

use anyhow::{Context, Result, anyhow, bail};
use sophia::api::ns::{Namespace, rdf};
use sophia::api::prelude::*;
use sophia::api::term::Term;
use sophia::api::term::matcher::Any;
use sophia::inmem::graph::FastGraph;
use sophia::turtle::parser::turtle;
use std::path::Path;

use super::{
    InducedClass, InducedOntology, InducedRelation, InductionProvenance, OntologyStatus,
    PRISM_META_NS, class_slug, domain_namespace, ontology_iri, relation_slug,
};
use crate::semantic_validation::{OntologySemanticValidationReport, SemanticValidationStatus};

const OWL_NS: &str = "http://www.w3.org/2002/07/owl#";
const SKOS_NS: &str = "http://www.w3.org/2004/02/skos/core#";
const RDFS_NS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const PROV_NS: &str = "http://www.w3.org/ns/prov#";
const DCTERMS_NS: &str = "http://purl.org/dc/terms/";

/// Escape a string for a double-quoted Turtle literal.
fn escape_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn semantic_report_json(report: &OntologySemanticValidationReport) -> String {
    serde_json::to_string(report).unwrap_or_else(|error| {
        let mut fallback = OntologySemanticValidationReport::default();
        fallback.near_duplicates.status = SemanticValidationStatus::Failed;
        fallback.near_duplicates.message = Some(format!(
            "semantic validation report serialization failed ({error})"
        ));
        serde_json::to_string(&fallback).expect("the static failed semantic report must serialize")
    })
}

/// Serialise deterministically. Classes/relations are emitted in their
/// stored order, which [`super::OntologyBuilder::finish`] and
/// [`parse_turtle`] both keep sorted by minted local name.
#[must_use]
pub fn to_turtle(o: &InducedOntology) -> String {
    let base = ontology_iri(&o.domain);
    let ns = domain_namespace(&o.domain);
    let p = &o.provenance;
    let mut out = String::with_capacity(4096);

    out.push_str(&format!(
        "@prefix rdfs: <{RDFS_NS}> .\n\
         @prefix owl: <{OWL_NS}> .\n\
         @prefix skos: <{SKOS_NS}> .\n\
         @prefix dcterms: <{DCTERMS_NS}> .\n\
         @prefix prov: <{PROV_NS}> .\n\
         @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
         @prefix prism: <{PRISM_META_NS}> .\n\
         @prefix : <{ns}> .\n\n"
    ));

    // Ontology header.
    out.push_str(&format!("<{base}> a owl:Ontology ;\n"));
    out.push_str(&format!("    owl:versionIRI <{}> ;\n", o.version_iri()));
    out.push_str(&format!(
        "    rdfs:label \"{}\"@en ;\n",
        escape_literal(&o.domain)
    ));
    out.push_str(&format!(
        "    prism:domain \"{}\" ;\n",
        escape_literal(&o.domain)
    ));
    out.push_str(&format!("    prism:status \"{}\" ;\n", o.status.as_str()));
    if !p.created_at.is_empty() {
        out.push_str(&format!(
            "    dcterms:created \"{}\"^^xsd:dateTime ;\n",
            escape_literal(&p.created_at)
        ));
    }
    if let Some(promoted) = &p.promoted_at {
        out.push_str(&format!(
            "    prism:promotedAt \"{}\"^^xsd:dateTime ;\n",
            escape_literal(promoted)
        ));
    }
    out.push_str("    prov:wasGeneratedBy :inductionRun .\n\n");

    // Induction activity: the full provenance block.
    out.push_str(":inductionRun a prov:Activity ;\n");
    out.push_str("    rdfs:label \"PRISM ontology induction\"@en ;\n");
    out.push_str(&format!(
        "    prism:model \"{}\" ;\n",
        escape_literal(&p.model)
    ));
    out.push_str(&format!(
        "    prism:promptVersion \"{}\" ;\n",
        escape_literal(&p.prompt_version)
    ));
    out.push_str(&format!(
        "    prism:corpusHash \"{}\" ;\n",
        escape_literal(&p.corpus_hash)
    ));
    out.push_str(&format!(
        "    prism:documentsTotal {} ;\n",
        p.documents_total
    ));
    out.push_str(&format!(
        "    prism:documentsFailed {} ;\n",
        p.documents_failed
    ));
    // One line per inherited base: what it was, which version, and its digest,
    // so a grown artifact can never quietly claim its base's classes as its own.
    for seed in &p.seeds {
        out.push_str(&format!(
            "    prism:seed \"{}\" ;\n",
            escape_literal(&format!(
                "{}|{}|{}|{}|{}",
                seed.id, seed.version_iri, seed.artifact_sha256, seed.classes, seed.relations
            ))
        ));
    }
    out.push_str(&format!("    prism:windowsRead {} ;\n", p.windows_read));
    out.push_str(&format!(
        "    prism:windowsAttempted {} ;\n",
        p.windows_attempted
    ));
    out.push_str(&format!("    prism:malformedItems {} ", p.malformed_items));
    out.push_str(&format!(
        ";\n    prism:semanticValidation \"{}\" ",
        escape_literal(&semantic_report_json(&p.semantic_validation))
    ));
    let mut links = p.dropped_parent_links.clone();
    links.sort();
    for link in &links {
        out.push_str(&format!(
            ";\n    prism:droppedParentLink \"{}\" ",
            escape_literal(link)
        ));
    }
    let mut notes = p.merge_notes.clone();
    notes.sort();
    for note in &notes {
        out.push_str(&format!(
            ";\n    prism:mergeNote \"{}\" ",
            escape_literal(note)
        ));
    }
    out.push_str(".\n\n");

    // Classes.
    for class in &o.classes {
        let Some(slug) = class_slug(&class.label) else {
            continue; // unmintable label; validation reports it
        };
        out.push_str(&format!(":{slug} a owl:Class ;\n"));
        out.push_str(&format!(
            "    skos:prefLabel \"{}\"@en ",
            escape_literal(&class.label)
        ));
        if !class.definition.is_empty() {
            out.push_str(&format!(
                ";\n    rdfs:comment \"{}\"@en ",
                escape_literal(&class.definition)
            ));
        }
        if let Some(parent) = &class.parent
            && let Some(pslug) = class_slug(parent)
        {
            out.push_str(&format!(";\n    rdfs:subClassOf :{pslug} "));
        }
        match &class.aligned_iri {
            Some(iri) => out.push_str(&format!(";\n    skos:exactMatch <{iri}> ")),
            None => out.push_str(";\n    prism:alignment \"unmatched\" "),
        }
        if class.declared_by_reference {
            out.push_str(";\n    prism:declaredByReference true ");
        }
        if let Some(sign_domain) = class.sign_domain {
            out.push_str(&format!(
                ";\n    prism:signDomain \"{}\" ",
                super::sign_domain_as_stored(sign_domain)
            ));
        }
        out.push_str(".\n\n");
    }

    // Relations.
    for rel in &o.relations {
        let Some(slug) = relation_slug(&rel.label) else {
            continue;
        };
        out.push_str(&format!(":{slug} a owl:ObjectProperty ;\n"));
        out.push_str(&format!(
            "    skos:prefLabel \"{}\"@en ",
            escape_literal(&rel.label)
        ));
        if !rel.definition.is_empty() {
            out.push_str(&format!(
                ";\n    rdfs:comment \"{}\"@en ",
                escape_literal(&rel.definition)
            ));
        }
        if let Some(dslug) = class_slug(&rel.domain) {
            out.push_str(&format!(";\n    rdfs:domain :{dslug} "));
        }
        if let Some(rslug) = class_slug(&rel.range) {
            out.push_str(&format!(";\n    rdfs:range :{rslug} "));
        }
        match &rel.aligned_iri {
            Some(iri) => out.push_str(&format!(";\n    skos:exactMatch <{iri}> ")),
            None => out.push_str(";\n    prism:alignment \"unmatched\" "),
        }
        // The relation's typed fact shape, when declared. This is what lets a
        // promoted ontology reach typed measurement/phase/processing/contains
        // facts instead of untyped edges — with zero Rust edits.
        if let Some(kind) = rel.fact_kind {
            out.push_str(&format!(
                ";\n    prism:factKind \"{}\" ",
                super::fact_kind_as_stored(kind)
            ));
        }
        out.push_str(".\n\n");
    }

    out
}

/// One literal object of (s, p) in `graph`, if present.
fn literal_of(graph: &FastGraph, s: &impl Term, p: &impl Term) -> Option<String> {
    graph
        .triples_matching([s.borrow_term()], [p.borrow_term()], Any)
        .filter_map(|t| t.ok())
        .find_map(|t| t.o().lexical_form().map(|lex| lex.to_string()))
}

/// One IRI object of (s, p) in `graph`, if present.
fn iri_of(graph: &FastGraph, s: &impl Term, p: &impl Term) -> Option<String> {
    graph
        .triples_matching([s.borrow_term()], [p.borrow_term()], Any)
        .filter_map(|t| t.ok())
        .find_map(|t| t.o().iri().map(|iri| iri.as_str().to_string()))
}

/// All literal objects of (s, p), sorted for determinism.
fn literals_of(graph: &FastGraph, s: &impl Term, p: &impl Term) -> Vec<String> {
    let mut out: Vec<String> = graph
        .triples_matching([s.borrow_term()], [p.borrow_term()], Any)
        .filter_map(|t| {
            t.ok()
                .and_then(|t| t.o().lexical_form().map(|l| l.to_string()))
        })
        .collect();
    out.sort();
    out
}

/// The label a reference IRI resolves to: the referenced class's prefLabel
/// when it is declared in the document, else the IRI's local name — which
/// validation will then flag as undeclared instead of anything silently
/// disappearing.
fn label_for_iri(iri: &str, labels: &std::collections::HashMap<String, String>) -> String {
    labels
        .get(iri)
        .cloned()
        .unwrap_or_else(|| iri.rsplit(['#', '/']).next().unwrap_or(iri).to_string())
}

/// Reconstruct an [`InducedOntology`] from Turtle. Strict where governance
/// depends on it (ontology node, domain, status must be present and
/// well-formed), tolerant where provenance is merely descriptive.
pub fn parse_turtle(ttl: &str) -> Result<InducedOntology> {
    let graph: FastGraph = turtle::parse_str(ttl)
        .collect_triples()
        .map_err(|e| anyhow!("invalid Turtle: {e}"))?;

    let owl = Namespace::new_unchecked(OWL_NS);
    let skos = Namespace::new_unchecked(SKOS_NS);
    let rdfs = Namespace::new_unchecked(RDFS_NS);
    let prov = Namespace::new_unchecked(PROV_NS);
    let dcterms = Namespace::new_unchecked(DCTERMS_NS);
    let prism = Namespace::new_unchecked(PRISM_META_NS);

    let owl_ontology = owl.get_unchecked("Ontology");
    let owl_class = owl.get_unchecked("Class");
    let owl_object_property = owl.get_unchecked("ObjectProperty");
    let prov_activity = prov.get_unchecked("Activity");
    let pref_label = skos.get_unchecked("prefLabel");
    let exact_match = skos.get_unchecked("exactMatch");
    let comment = rdfs.get_unchecked("comment");
    let sub_class_of = rdfs.get_unchecked("subClassOf");
    let rdfs_domain = rdfs.get_unchecked("domain");
    let rdfs_range = rdfs.get_unchecked("range");

    // Ontology node: exactly one expected; strict because status/domain
    // governance hangs off it.
    let mut ontology_nodes: Vec<_> = graph
        .triples_matching(Any, [rdf::type_], [owl_ontology])
        .filter_map(|t| t.ok().map(|t| t.s()))
        .collect();
    if ontology_nodes.len() != 1 {
        bail!(
            "artifact must declare exactly one owl:Ontology node (found {})",
            ontology_nodes.len()
        );
    }
    let onto_node = ontology_nodes.remove(0);

    let domain =
        literal_of(&graph, &onto_node, &prism.get_unchecked("domain")).ok_or_else(|| {
            anyhow!("artifact carries no prism:domain — not a PRISM induction artifact")
        })?;
    super::validate_domain_id(&domain)?;
    let status_str =
        literal_of(&graph, &onto_node, &prism.get_unchecked("status")).ok_or_else(|| {
            anyhow!(
                "artifact carries no prism:status — a PRISM ontology artifact is always \
                 explicitly \"draft\" or \"accepted\", never implicit"
            )
        })?;
    let status = OntologyStatus::from_stored(&status_str)?;

    // Provenance from the (single expected) prov:Activity.
    let activity: Option<_> = graph
        .triples_matching(Any, [rdf::type_], [prov_activity])
        .filter_map(|t| t.ok().map(|t| t.s()))
        .next();
    let mut provenance = InductionProvenance {
        created_at: literal_of(&graph, &onto_node, &dcterms.get_unchecked("created"))
            .unwrap_or_default(),
        promoted_at: literal_of(&graph, &onto_node, &prism.get_unchecked("promotedAt")),
        ..Default::default()
    };
    if let Some(act) = &activity {
        let get = |name: &str| literal_of(&graph, act, &prism.get_unchecked(name));
        provenance.model = get("model").unwrap_or_default();
        provenance.prompt_version = get("promptVersion").unwrap_or_default();
        provenance.corpus_hash = get("corpusHash").unwrap_or_default();
        provenance.documents_total = get("documentsTotal")
            .and_then(|v| v.parse().ok())
            .unwrap_or_default();
        provenance.documents_failed = get("documentsFailed")
            .and_then(|v| v.parse().ok())
            .unwrap_or_default();
        provenance.seeds = literals_of(&graph, act, &prism.get_unchecked("seed"))
            .into_iter()
            .filter_map(|raw| {
                let parts: Vec<&str> = raw.splitn(5, '|').collect();
                if parts.len() != 5 {
                    return None;
                }
                Some(crate::induction::seed::SeedRef {
                    id: parts[0].to_string(),
                    version_iri: parts[1].to_string(),
                    artifact_sha256: parts[2].to_string(),
                    classes: parts[3].parse().unwrap_or_default(),
                    relations: parts[4].parse().unwrap_or_default(),
                })
            })
            .collect();
        provenance.windows_read = get("windowsRead")
            .and_then(|v| v.parse().ok())
            .unwrap_or_default();
        provenance.windows_attempted = get("windowsAttempted")
            .and_then(|v| v.parse().ok())
            .unwrap_or_default();
        provenance.malformed_items = get("malformedItems")
            .and_then(|v| v.parse().ok())
            .unwrap_or_default();
        provenance.dropped_parent_links =
            literals_of(&graph, act, &prism.get_unchecked("droppedParentLink"));
        provenance.merge_notes = literals_of(&graph, act, &prism.get_unchecked("mergeNote"));
        provenance.semantic_validation = match get("semanticValidation") {
            Some(json) => serde_json::from_str(&json)
                .context("prism:semanticValidation is not a valid semantic report")?,
            None => OntologySemanticValidationReport::default(),
        };
    }

    // IRI → prefLabel for every declared class, so subClassOf / domain /
    // range references resolve to labels.
    let class_nodes: Vec<_> = graph
        .triples_matching(Any, [rdf::type_], [owl_class])
        .filter_map(|t| t.ok().map(|t| t.s()))
        .collect();
    let mut labels: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for node in &class_nodes {
        if let (Some(iri), Some(label)) = (
            node.iri().map(|i| i.as_str().to_string()),
            literal_of(&graph, node, &pref_label),
        ) {
            labels.insert(iri, label);
        }
    }

    let declared_by_ref = prism.get_unchecked("declaredByReference");
    let sign_domain_pred = prism.get_unchecked("signDomain");
    let mut classes = Vec::with_capacity(class_nodes.len());
    for node in &class_nodes {
        let label = literal_of(&graph, node, &pref_label).unwrap_or_default();
        let parent = iri_of(&graph, node, &sub_class_of).map(|iri| label_for_iri(&iri, &labels));
        // Governance, not provenance: an unknown sign-domain value is a
        // loud refusal, never a silent read-as-silence.
        let sign_domain = match literal_of(&graph, node, &sign_domain_pred) {
            Some(raw) => Some(super::sign_domain_from_stored(&raw).ok_or_else(|| {
                anyhow!(
                    "class {label:?} carries prism:signDomain {raw:?} — expected \
                         \"non_negative\", \"signed\" or \"unspecified\""
                )
            })?),
            None => None,
        };
        classes.push(InducedClass {
            label,
            definition: literal_of(&graph, node, &comment).unwrap_or_default(),
            parent,
            aligned_iri: iri_of(&graph, node, &exact_match),
            declared_by_reference: literal_of(&graph, node, &declared_by_ref)
                .is_some_and(|v| v == "true"),
            sign_domain,
        });
    }

    let rel_nodes: Vec<_> = graph
        .triples_matching(Any, [rdf::type_], [owl_object_property])
        .filter_map(|t| t.ok().map(|t| t.s()))
        .collect();
    let fact_kind_pred = prism.get_unchecked("factKind");
    let mut relations = Vec::with_capacity(rel_nodes.len());
    for node in &rel_nodes {
        let label = literal_of(&graph, node, &pref_label).unwrap_or_default();
        // Same governance as `signDomain`: an unknown value is a loud
        // refusal. Reading a tampered annotation as "generic edge" would
        // silently downgrade typed facts to untyped ones.
        let fact_kind = match literal_of(&graph, node, &fact_kind_pred) {
            Some(raw) => Some(super::fact_kind_from_stored(&raw).ok_or_else(|| {
                anyhow!(
                    "relation {label:?} carries prism:factKind {raw:?} — expected \
                     \"measurement\", \"phase\", \"processing\" or \"contains\""
                )
            })?),
            None => None,
        };
        relations.push(InducedRelation {
            label,
            definition: literal_of(&graph, node, &comment).unwrap_or_default(),
            domain: iri_of(&graph, node, &rdfs_domain)
                .map(|iri| label_for_iri(&iri, &labels))
                .unwrap_or_default(),
            range: iri_of(&graph, node, &rdfs_range)
                .map(|iri| label_for_iri(&iri, &labels))
                .unwrap_or_default(),
            aligned_iri: iri_of(&graph, node, &exact_match),
            fact_kind,
        });
    }

    // Graph iteration order is arbitrary; the struct's order contract is
    // sorted-by-slug, same as the builder's.
    classes.sort_by_key(|c: &InducedClass| class_slug(&c.label).unwrap_or_default());
    relations.sort_by_key(|r: &InducedRelation| relation_slug(&r.label).unwrap_or_default());

    Ok(InducedOntology {
        domain,
        status,
        classes,
        relations,
        provenance,
    })
}

/// Write the artifact atomically (temp file + rename in the same directory).
pub fn write_artifact(path: &Path, ontology: &InducedOntology) -> Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let tmp = match dir {
        Some(d) => d.join(format!(
            ".{}.tmp",
            path.file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        )),
        None => std::path::PathBuf::from(format!(".{}.tmp", path.display())),
    };
    std::fs::write(&tmp, to_turtle(ontology))
        .with_context(|| format!("cannot write ontology artifact {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| {
        format!(
            "cannot move ontology artifact into place at {}",
            path.display()
        )
    })?;
    Ok(())
}

/// Promote a DRAFT artifact to ACCEPTED, in place. The deliberate act the
/// draft gate requires: validation runs first (an invalid artifact cannot
/// be promoted), a non-draft input is refused (double promotion signals
/// confusion, not intent), and the rewritten artifact records
/// `prism:promotedAt`.
pub fn promote_artifact(path: &Path) -> Result<InducedOntology> {
    let mut ontology = super::load_validated(path)?;
    match ontology.status {
        OntologyStatus::Accepted => bail!(
            "{} is already accepted (promoted at {}); nothing to promote",
            path.display(),
            ontology
                .provenance
                .promoted_at
                .as_deref()
                .unwrap_or("unknown time")
        ),
        OntologyStatus::Draft => {}
    }
    ontology.status = OntologyStatus::Accepted;
    ontology.provenance.promoted_at =
        Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    write_artifact(path, &ontology)?;
    Ok(ontology)
}

#[cfg(test)]
mod tests {
    use super::super::{InductionProvenance, OntologyStatus};
    use super::*;
    use crate::semantic_validation::{
        OntologyLabelProposal, SemanticCheckReport, SemanticValidationStatus,
    };
    use prism_provenance::QuantitySignDomain;

    fn sample() -> InducedOntology {
        InducedOntology {
            domain: "alloys".into(),
            status: OntologyStatus::Draft,
            classes: vec![
                InducedClass {
                    label: "Alloy".into(),
                    definition: "A metallic material of two or more elements.".into(),
                    parent: Some("Material".into()),
                    aligned_iri: Some("https://w3id.org/emmo#EMMO_alloy".into()),
                    declared_by_reference: false,
                    sign_domain: None,
                },
                InducedClass {
                    label: "Heat \"Quench\" Treatment\nStep".into(),
                    definition: "Definition with a backslash \\ and\ttab.".into(),
                    parent: None,
                    aligned_iri: None,
                    declared_by_reference: true,
                    sign_domain: None,
                },
                InducedClass {
                    label: "Material".into(),
                    definition: String::new(),
                    parent: None,
                    aligned_iri: None,
                    declared_by_reference: false,
                    sign_domain: None,
                },
            ],
            relations: vec![InducedRelation {
                label: "has property".into(),
                definition: "Links a material to a measured property.".into(),
                domain: "Alloy".into(),
                range: "Material".into(),
                aligned_iri: None,
                fact_kind: None,
            }],
            provenance: InductionProvenance {
                model: "qwen2.5:3b".into(),
                prompt_version: "1".into(),
                corpus_hash: "sha256:abcdef1234567890".into(),
                documents_total: 3,
                documents_failed: 1,
                windows_read: 5,
                windows_attempted: 7,
                seeds: vec![crate::induction::seed::SeedRef {
                    id: "emmo".into(),
                    version_iri: "https://example.org/emmo/1.0.3".into(),
                    artifact_sha256: "a".repeat(64),
                    classes: 50,
                    relations: 5,
                }],
                malformed_items: 2,
                created_at: "2026-08-09T00:00:00Z".into(),
                promoted_at: None,
                dropped_parent_links: vec!["Material -> Alloy".into()],
                merge_notes: vec!["relation 'has property': kept domain/range".into()],
                semantic_validation: OntologySemanticValidationReport {
                    policy: Default::default(),
                    proposals: vec![OntologyLabelProposal {
                        label: "Alloy".into(),
                        kind: "class".into(),
                    }],
                    backend: Some("test:ontology-v1".into()),
                    near_duplicates: SemanticCheckReport {
                        status: SemanticValidationStatus::Applied,
                        candidates: 1,
                        evaluated: 1,
                        passed: Some(true),
                        findings: Vec::new(),
                        message: None,
                    },
                },
            },
        }
    }

    #[test]
    fn to_turtle_is_deterministic() {
        let o = sample();
        assert_eq!(to_turtle(&o), to_turtle(&o));
    }

    #[test]
    fn roundtrip_preserves_everything_that_governs() {
        let o = sample();
        let ttl = to_turtle(&o);
        let parsed = parse_turtle(&ttl).expect("emitted artifact must parse");
        assert_eq!(parsed.domain, o.domain);
        assert_eq!(parsed.status, o.status);
        assert_eq!(parsed.classes.len(), o.classes.len());
        assert_eq!(parsed.relations.len(), o.relations.len());
        // Labels with quotes/newlines/backslashes survive the round trip.
        let hairy = parsed
            .classes
            .iter()
            .find(|c| c.label.contains("Quench"))
            .expect("hairy label survives");
        assert_eq!(hairy.label, "Heat \"Quench\" Treatment\nStep");
        assert!(hairy.declared_by_reference);
        // Parent resolves back to the declared label.
        let alloy = parsed.classes.iter().find(|c| c.label == "Alloy").unwrap();
        assert_eq!(alloy.parent.as_deref(), Some("Material"));
        assert_eq!(
            alloy.aligned_iri.as_deref(),
            Some("https://w3id.org/emmo#EMMO_alloy")
        );
        // Relation domain/range resolve to labels.
        assert_eq!(parsed.relations[0].domain, "Alloy");
        assert_eq!(parsed.relations[0].range, "Material");
        // Provenance block round-trips.
        assert_eq!(parsed.provenance, o.provenance);
    }

    #[test]
    fn stable_labels_mint_stable_iris() {
        let o = sample();
        let ttl = to_turtle(&o);
        assert!(ttl.contains(":Alloy a owl:Class"), "{ttl}");
        assert!(ttl.contains(":hasProperty a owl:ObjectProperty"), "{ttl}");
        assert!(
            ttl.contains("<https://prism.mirdyne.com/ontology/alloys> a owl:Ontology"),
            "{ttl}"
        );
        // The SEED is part of artifact identity: the same corpus grown onto a
        // different base is a different ontology, and must not claim the same
        // version IRI. `sample()` inherits from emmo, so the digest is present.
        assert!(
            ttl.contains(
                "owl:versionIRI <https://prism.mirdyne.com/ontology/alloys/version/1.abcdef12+"
            ),
            "a seeded artifact carries its seed in the version IRI: {ttl}"
        );

        // Drop the seed and the plain (domain, prompt, corpus) identity returns,
        // so an unseeded run is unaffected by any of this.
        let mut standalone = sample();
        standalone.provenance.seeds.clear();
        assert!(
            to_turtle(&standalone).contains(
                "owl:versionIRI <https://prism.mirdyne.com/ontology/alloys/version/1.abcdef12>"
            ),
            "an unseeded artifact keeps the original version IRI"
        );
    }

    #[test]
    fn unmatched_classes_are_recorded_not_omitted() {
        let ttl = to_turtle(&sample());
        assert!(ttl.contains("prism:alignment \"unmatched\""), "{ttl}");
    }

    #[test]
    fn parse_refuses_missing_status() {
        let ttl = r#"
            @prefix owl: <http://www.w3.org/2002/07/owl#> .
            @prefix prism: <https://prism.mirdyne.com/ontology/meta#> .
            <https://prism.mirdyne.com/ontology/x> a owl:Ontology ;
                prism:domain "x" .
        "#;
        let err = parse_turtle(ttl).unwrap_err();
        assert!(format!("{err:#}").contains("prism:status"), "{err:#}");
    }

    #[test]
    fn parse_refuses_unknown_status() {
        let ttl = r#"
            @prefix owl: <http://www.w3.org/2002/07/owl#> .
            @prefix prism: <https://prism.mirdyne.com/ontology/meta#> .
            <https://prism.mirdyne.com/ontology/x> a owl:Ontology ;
                prism:domain "x" ;
                prism:status "probably-fine" .
        "#;
        let err = parse_turtle(ttl).unwrap_err();
        assert!(format!("{err:#}").contains("probably-fine"), "{err:#}");
    }

    #[test]
    fn legacy_artifact_without_semantic_report_is_explicitly_unavailable() {
        let ttl = r#"
            @prefix owl: <http://www.w3.org/2002/07/owl#> .
            @prefix prism: <https://prism.mirdyne.com/ontology/meta#> .
            <https://prism.mirdyne.com/ontology/x> a owl:Ontology ;
                prism:domain "x" ;
                prism:status "draft" .
        "#;
        let parsed = parse_turtle(ttl).unwrap();
        let semantic = parsed.provenance.semantic_validation.near_duplicates;
        assert_eq!(semantic.status, SemanticValidationStatus::Unavailable);
        assert_eq!(semantic.evaluated, 0);
        assert_eq!(semantic.passed, None);
    }

    #[test]
    fn parse_refuses_garbage() {
        assert!(parse_turtle("this is not turtle {{{").is_err());
    }

    #[test]
    fn promote_flips_draft_to_accepted_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alloys.ttl");
        write_artifact(&path, &sample()).unwrap();

        let promoted = promote_artifact(&path).unwrap();
        assert_eq!(promoted.status, OntologyStatus::Accepted);
        assert!(promoted.provenance.promoted_at.is_some());

        // The file itself now carries the accepted status…
        let reread = parse_turtle(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(reread.status, OntologyStatus::Accepted);

        // …and promoting twice is refused loudly.
        let err = promote_artifact(&path).unwrap_err();
        assert!(format!("{err:#}").contains("already accepted"), "{err:#}");
    }

    #[test]
    fn promote_refuses_an_invalid_artifact() {
        // A draft with a subClassOf cycle must not be promotable — the
        // promotion gate runs the same validator as everything else.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cyclic.ttl");
        let mut o = sample();
        o.classes[2].parent = Some("Alloy".into()); // Material -> Alloy -> Material
        write_artifact(&path, &o).unwrap();
        let err = promote_artifact(&path).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("REJECTED"), "{msg}");
        assert!(msg.contains("subclass_cycle"), "{msg}");
        // And the artifact on disk is untouched — still a draft.
        let reread = parse_turtle(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(reread.status, OntologyStatus::Draft);
    }

    /// The optional `prism:signDomain` annotation round-trips through the
    /// artifact bytes — this is the channel a promoted ontology uses to
    /// supply quantity sign constraints with zero Rust edits.
    #[test]
    fn sign_domain_annotations_round_trip_and_absence_stays_silence() {
        let mut o = sample();
        o.classes[0].sign_domain = Some(QuantitySignDomain::NonNegative);
        o.classes[2].sign_domain = Some(QuantitySignDomain::Signed);
        let ttl = to_turtle(&o);
        assert!(ttl.contains("prism:signDomain \"non_negative\""), "{ttl}");
        assert!(ttl.contains("prism:signDomain \"signed\""), "{ttl}");

        let parsed = parse_turtle(&ttl).expect("emitted artifact must parse");
        let alloy = parsed.classes.iter().find(|c| c.label == "Alloy").unwrap();
        assert_eq!(alloy.sign_domain, Some(QuantitySignDomain::NonNegative));
        let material = parsed
            .classes
            .iter()
            .find(|c| c.label == "Material")
            .unwrap();
        assert_eq!(material.sign_domain, Some(QuantitySignDomain::Signed));
        // A class without the annotation stays silent.
        let quench = parsed
            .classes
            .iter()
            .find(|c| c.label.contains("Quench"))
            .unwrap();
        assert_eq!(quench.sign_domain, None);
    }

    #[test]
    fn an_unknown_sign_domain_value_is_refused_loudly() {
        // Governance, not provenance: a tampered or foreign annotation must
        // fail the parse naming the offending class — never read as silence.
        let mut o = sample();
        o.classes[0].sign_domain = Some(QuantitySignDomain::NonNegative);
        let ttl = to_turtle(&o).replace("non_negative", "always_positive");
        let err = parse_turtle(&ttl).expect_err("an unknown sign domain is a refusal");
        let msg = format!("{err:#}");
        assert!(msg.contains("signDomain"), "{msg}");
        assert!(msg.contains("Alloy"), "{msg}");
    }
}
