//! THE dehardcoding acceptance test.
//!
//! The standing rule this serves: **the ontology IS the product.** PRISM
//! sells the ability to extend an ontology per customer, and the acceptance
//! criterion for every "is it really out of Rust?" question is literal:
//! *could a promoted non-materials ontology — legal, pharma, financial —
//! govern extraction with ZERO Rust edits?*
//!
//! This test IS that criterion. A synthetic LEGAL ontology (arbitrary
//! vocabulary chosen deliberately: no alloy, element, phase or property word
//! appears anywhere in it — it is a stand-in, not a domain PRISM supports)
//! goes through the production path end to end:
//!
//! 1. **Induction artifact → promotion → registration** — the artifact is
//!    the ONLY domain input; no Rust type, table or literal knows these
//!    words.
//! 2. **Extraction** — `to_local_facts` maps relationships through the
//!    ontology's own declared relations (`prism:factKind`), yielding TYPED
//!    facts with attributable values.
//! 3. **Graph write** — the same shape-resolving call the ingest pipeline
//!    makes, producing a correctly-typed graph: the object node carries the
//!    ontology's declared class, the typed edge is the ontology's relation
//!    token, and the measurement shape reifies.
//!
//! If any of the six hardcoding defects this work removed ever returns — a
//! kind→(class, edge) table in the store, a materials-shaped prompt, a Rust
//! default for eligible fact kinds, an English-materials menu, materials
//! routing doctrine, or a closed domain registry — this test is where the
//! answer flips from "yes" to "no".

use prism_ingest::induction::{
    self, InducedClass, InducedFactKind, InducedOntology, InducedRelation, InductionProvenance,
    OntologyStatus,
};
use prism_ingest::local_facts::to_local_facts;
use prism_provenance::{
    ClassifiedFactNodes, ClassifiedNode, EvidenceClass, FactPayload, LocalProvenance,
    OntologyClassification, ProvenanceStore,
};
use serde_json::json;

/// A deliberately non-materials ontology: case law. `Case`/`Verdict` are the
/// subject classes; `Damages Amount` is the quantity class; `awards` is the
/// measurement-carrying relation. No materials vocabulary anywhere.
fn legal_ontology() -> InducedOntology {
    InducedOntology {
        domain: "acceptance-legal".into(),
        status: OntologyStatus::Draft,
        classes: vec![
            InducedClass {
                label: "Case".into(),
                definition: "A decided legal matter.".into(),
                parent: None,
                aligned_iri: None,
                declared_by_reference: false,
                sign_domain: None,
            },
            InducedClass {
                label: "Verdict".into(),
                definition: "The finding a court returns.".into(),
                parent: Some("Case".into()),
                aligned_iri: None,
                declared_by_reference: false,
                sign_domain: None,
            },
            InducedClass {
                label: "Damages Amount".into(),
                definition: "A monetary quantum awarded.".into(),
                parent: None,
                aligned_iri: None,
                declared_by_reference: false,
                // Money is never negative here.
                sign_domain: Some(prism_provenance::QuantitySignDomain::NonNegative),
            },
        ],
        relations: vec![InducedRelation {
            label: "awards".into(),
            definition: "A verdict awards a damages amount.".into(),
            domain: "Verdict".into(),
            range: "Damages Amount".into(),
            aligned_iri: None,
            // The artifact's OWN statement that this relation carries a
            // measured quantity — the store's typed `measurement` shape.
            fact_kind: Some(InducedFactKind::Measurement),
        }],
        provenance: InductionProvenance {
            corpus_hash: "sha256:c0ffee00".into(),
            prompt_version: induction::PROMPT_VERSION.into(),
            ..Default::default()
        },
    }
}

fn provenance() -> LocalProvenance {
    LocalProvenance {
        activity_id: "act-acceptance-legal".into(),
        agent_id: "acceptance-test".into(),
        agent_kind: "SoftwareAgent".into(),
        source_entity_id: "corpus/cases.jsonl".into(),
        source_kind: "Document".into(),
        tenant: "local@acceptance-legal".into(),
        started_at: "2026-08-09T00:00:00Z".into(),
        ended_at: "2026-08-09T00:00:01Z".into(),
        locality: "local".into(),
        origin_source_id: None,
    }
}

#[tokio::test]
async fn a_non_materials_ontology_governs_extraction_to_a_typed_graph() {
    // ── 1. Artifact → promotion → registration, the only domain input ──
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legal.ttl");
    induction::ttl::write_artifact(&path, &legal_ontology()).unwrap();
    induction::ttl::promote_artifact(&path).unwrap();
    let ontology = induction::register::load_induced_from_path(&path).unwrap();
    assert_eq!(ontology.id(), "acceptance-legal");
    // The extraction vocabulary is the artifact's own, minted by slug —
    // nothing here consulted a Rust domain list.
    let class_labels: Vec<&str> = ontology
        .classes()
        .iter()
        .flat_map(|class| class.extraction_labels.iter())
        .map(String::as_str)
        .collect();
    assert_eq!(
        class_labels,
        ["Case", "DamagesAmount", "Verdict"],
        "extraction vocabulary is the artifact's"
    );

    // ── 2. Extraction: typed facts with attributable values ──
    // The extractor emits the ontology's OWN labels/tokens. Zero materials
    // words, zero Rust literals.
    let entity_set: prism_ingest::EntitySet = prism_ingest::EntitySet {
        entities: vec![
            prism_ingest::Entity {
                entity_type: "Verdict".into(),
                name: "Haden v. Orlite".into(),
                properties: json!({}),
            },
            prism_ingest::Entity {
                entity_type: "DamagesAmount".into(),
                name: "general damages".into(),
                properties: json!({}),
            },
        ],
        relationships: vec![prism_ingest::Relationship {
            from: "Haden v. Orlite".into(),
            rel_type: "AWARDS".into(),
            to: "general damages".into(),
            weight: None,
            order: None,
            value: Some(250000.0),
            unit: Some("USD".into()),
            confidence: Some(0.9),
        }],
    };
    let (facts, dropped) = to_local_facts(&entity_set, ontology.as_ref());
    assert!(dropped.is_empty(), "{dropped:?}");
    assert_eq!(facts.len(), 1);
    let fact = &facts[0];
    assert_eq!(
        fact.kind.as_deref(),
        Some("measurement"),
        "the declared awards relation must yield a TYPED measurement fact"
    );
    assert_eq!(fact.value, Some(250000.0));
    assert_eq!(fact.to_local_fact().unit.as_deref(), Some("USD"));

    // ── 3. Graph write: correctly-typed nodes and edges ──
    let store = ProvenanceStore::open(&dir.path().join("legal.db"))
        .await
        .unwrap();
    let classification = OntologyClassification {
        version_iri: ontology.version_iri().as_str(),
        artifact_sha256: ontology.artifact_sha256(),
    };
    let verdict_class = ontology.class_for_label("Verdict").unwrap();
    let damages_class = ontology.class_for_label("DamagesAmount").unwrap();
    let nodes = ClassifiedFactNodes {
        subject: ClassifiedNode {
            entity_type: "Verdict",
            storage_label: ontology.storage_label("Verdict").unwrap(),
            class_iri: verdict_class.iri.as_str(),
        },
        object: ClassifiedNode {
            entity_type: "DamagesAmount",
            storage_label: ontology.storage_label("DamagesAmount").unwrap(),
            class_iri: damages_class.iri.as_str(),
        },
    };
    // The graph shape is resolved from the ONTOLOGY's declaration — the
    // exact call the ingest pipeline makes per fact.
    let shape = fact
        .kind
        .as_deref()
        .and_then(|kind| ontology.fact_graph_shape(kind))
        .expect("the artifact's measurement declaration carries a shape");
    assert_eq!(shape.object_storage_label, "DamagesAmount");
    assert_eq!(shape.edge_rel_type, "AWARDS");

    store
        .write_classified_fact_with_evidence(
            fact,
            &provenance(),
            EvidenceClass::Research,
            nodes,
            classification,
            Some(shape),
        )
        .await
        .unwrap();

    // The subject node carries the ontology's declared class, verbatim.
    let subject = store
        .graph_search("Haden v. Orlite", "local@acceptance-legal", 10)
        .await
        .unwrap()
        .into_iter()
        .find(|node| node.name == "Haden v. Orlite")
        .expect("the subject is queryable");
    assert_eq!(subject.label, "Verdict");
    assert_eq!(subject.entity_type, "Verdict");
    assert!(
        subject
            .class_iri
            .as_deref()
            .is_some_and(|iri| iri.contains("acceptance-legal")),
        "the node carries the artifact's own class IRI: {:?}",
        subject.class_iri
    );

    // The typed edge is the ontology's relation token, and the reified
    // measurement node carries the awarded value.
    let traversal = store
        .get_neighbors("Haden v. Orlite", None, "local@acceptance-legal", 10)
        .await
        .unwrap();
    assert!(
        traversal.edges.iter().any(|edge| edge.rel_type == "AWARDS"),
        "the typed edge is the ontology's own relation token: {:?}",
        traversal.edges
    );
    let measurement = store
        .graph_search("general damages", "local@acceptance-legal", 10)
        .await
        .unwrap()
        .into_iter()
        .find(|node| node.name == "general damages")
        .expect("the object is queryable");
    assert_eq!(measurement.label, "DamagesAmount");
    assert!(
        traversal
            .nodes
            .iter()
            .any(|node| node.name.starts_with("meas_")),
        "the measurement shape reifies through the store's audit node"
    );

    // And the recall path serves the typed value back.
    let recalled = store
        .recall_with_context("general damages", "local@acceptance-legal", 10)
        .await
        .unwrap();
    assert!(
        recalled.iter().any(|fact| fact.value == Some(250000.0)),
        "the typed value round-trips: {recalled:?}"
    );
}
