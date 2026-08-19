// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! End-to-end induction pipeline tests against a mock OpenAI-compatible LLM.
//!
//! The design premise under test: the model is SMALL and WILL emit malformed
//! JSON, duplicate classes and hierarchy cycles — the pipeline must survive
//! all three without ever repairing a FINISHED artifact silently.

use std::path::Path;

use prism_ingest::LlmConfig;
use prism_ingest::induction::{self, InductionConfig, corpus::load_corpus, ttl, validate};
use prism_ingest::llm::LlmClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// An OpenAI-compatible /chat/completions responder that replays canned
/// model outputs in request order.
struct ScriptedModel {
    responses: Vec<String>,
    calls: std::sync::atomic::AtomicUsize,
}

impl Respond for ScriptedModel {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let i = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let content = self
            .responses
            .get(i.min(self.responses.len().saturating_sub(1)))
            .cloned()
            .unwrap_or_default();
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": content}, "finish_reason": "stop"}]
        }))
    }
}

async fn scripted_server(responses: Vec<String>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ScriptedModel {
            responses,
            calls: std::sync::atomic::AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    server
}

fn client_for(server: &MockServer) -> LlmClient {
    LlmClient::new(LlmConfig {
        base_url: server.uri(),
        model: "scripted-test-model".into(),
        ..Default::default()
    })
}

fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).unwrap();
}

/// Doc 1 answers well; doc 2 returns prose-wrapped JSON (recoverable); doc 3
/// returns garbage twice (one retry) and is counted failed. The run
/// SURVIVES, the artifact records the failure, and the merged draft is
/// valid.
#[tokio::test]
async fn induction_survives_malformed_json_and_counts_the_failure() {
    let corpus_dir = tempfile::tempdir().unwrap();
    write(corpus_dir.path(), "a.md", "Alloys have properties.");
    write(corpus_dir.path(), "b.md", "Heat treatment is a process.");
    write(
        corpus_dir.path(),
        "c.md",
        "This document defeats the model.",
    );
    let corpus = load_corpus(corpus_dir.path()).unwrap();

    let server = scripted_server(vec![
        // a.md — clean proposal.
        r#"{"classes": [{"label": "Alloy", "definition": "A metallic mixture.", "parent": "Material"},
                        {"label": "Material", "definition": "Physical substance.", "parent": null}],
            "relations": [{"label": "has property", "definition": "links", "domain": "Alloy", "range": "Property"}]}"#
            .into(),
        // b.md — the JSON is wrapped in prose (recoverable by span extraction).
        r#"Sure! Here is the ontology you asked for:
           {"classes": [{"label": "Heat Treatment", "definition": "Thermal processing step.", "parent": "Process"},
                        {"label": "Process", "definition": "", "parent": null}],
            "relations": []}
           Hope this helps!"#
            .into(),
        // c.md — pure garbage, twice (first attempt + retry).
        "I am a small model and I refuse to emit JSON today.".into(),
        "STILL not json {{{".into(),
    ])
    .await;

    let ontology = induction::induce(
        &client_for(&server),
        &corpus,
        &InductionConfig::new("survival-test").unwrap(),
        &mut |_| {},
    )
    .await
    .expect("two of three documents succeeded — the run must survive");

    assert_eq!(ontology.provenance.documents_total, 3);
    assert_eq!(
        ontology.provenance.documents_failed, 1,
        "the defeated document is COUNTED, not hidden"
    );
    // "Property" was referenced by a relation but never proposed as a class:
    // declared by reference, recorded on the class.
    let property = ontology
        .classes
        .iter()
        .find(|c| c.label == "Property")
        .expect("referenced class is declared, not dropped");
    assert!(property.declared_by_reference);
    // The merged draft passes the strict validator.
    assert_eq!(validate::validate(&ontology), Vec::new());
    // And serialises to a parseable artifact.
    let parsed = ttl::parse_turtle(&ttl::to_turtle(&ontology)).unwrap();
    assert_eq!(parsed.classes.len(), ontology.classes.len());
}

/// Two documents propose the same class under different spellings, with
/// conflicting parents and a relation domain/range conflict: duplicates
/// merge (first wins deterministically), the conflict is RECORDED in the
/// artifact's provenance, and the result validates.
#[tokio::test]
async fn duplicate_classes_merge_and_conflicts_are_recorded() {
    let corpus_dir = tempfile::tempdir().unwrap();
    write(corpus_dir.path(), "a.md", "doc one");
    write(corpus_dir.path(), "b.md", "doc two");
    let corpus = load_corpus(corpus_dir.path()).unwrap();

    let server = scripted_server(vec![
        r#"{"classes": [{"label": "Heat Treatment", "definition": "Thermal step.", "parent": null},
                        {"label": "Alloy", "definition": "", "parent": null}],
            "relations": [{"label": "processed by", "definition": "", "domain": "Alloy", "range": "Heat Treatment"}]}"#
            .into(),
        // Same concepts, different spellings + a conflicting relation shape.
        r#"{"classes": [{"label": "HeatTreatment", "definition": "Another def.", "parent": null},
                        {"label": "alloy", "definition": "Metallic mixture.", "parent": null}],
            "relations": [{"label": "PROCESSED_BY", "definition": "", "domain": "Heat Treatment", "range": "Alloy"}]}"#
            .into(),
    ])
    .await;

    let ontology = induction::induce(
        &client_for(&server),
        &corpus,
        &InductionConfig::new("merge-test").unwrap(),
        &mut |_| {},
    )
    .await
    .unwrap();

    let heat: Vec<_> = ontology
        .classes
        .iter()
        .filter(|c| induction::normalize_label(&c.label) == "heat treatment")
        .collect();
    assert_eq!(heat.len(), 1, "one class, however the model spelt it");
    assert_eq!(heat[0].label, "Heat Treatment", "first surface form wins");
    let alloy = ontology
        .classes
        .iter()
        .find(|c| c.label == "Alloy")
        .unwrap();
    assert_eq!(
        alloy.definition, "Metallic mixture.",
        "an empty first definition is filled by a later one"
    );
    assert_eq!(
        ontology.relations.len(),
        1,
        "'processed by' == 'PROCESSED_BY'"
    );
    assert!(
        ontology
            .provenance
            .merge_notes
            .iter()
            .any(|n| n.contains("processed by")),
        "the domain/range conflict is recorded, not silent: {:?}",
        ontology.provenance.merge_notes
    );
    assert_eq!(validate::validate(&ontology), Vec::new());
}

/// The model proposes a subClassOf cycle across documents. The BUILDER
/// breaks it deterministically and records the dropped link in provenance;
/// the finished draft validates clean.
#[tokio::test]
async fn model_emitted_cycles_are_broken_and_recorded() {
    let corpus_dir = tempfile::tempdir().unwrap();
    write(corpus_dir.path(), "a.md", "doc one");
    let corpus = load_corpus(corpus_dir.path()).unwrap();

    let server = scripted_server(vec![
        r#"{"classes": [{"label": "Alloy", "definition": "", "parent": "Material"},
                        {"label": "Material", "definition": "", "parent": "Alloy"}],
            "relations": []}"#
            .into(),
    ])
    .await;

    let ontology = induction::induce(
        &client_for(&server),
        &corpus,
        &InductionConfig::new("cycle-test").unwrap(),
        &mut |_| {},
    )
    .await
    .unwrap();

    assert_eq!(
        ontology.provenance.dropped_parent_links,
        vec!["Alloy -> Material".to_string()],
        "the lexicographically first member of the cycle loses its parent link, and \
         the drop is RECORDED"
    );
    let alloy = ontology
        .classes
        .iter()
        .find(|c| c.label == "Alloy")
        .unwrap();
    assert_eq!(alloy.parent, None);
    let material = ontology
        .classes
        .iter()
        .find(|c| c.label == "Material")
        .unwrap();
    assert_eq!(
        material.parent.as_deref(),
        Some("Alloy"),
        "the other link survives"
    );
    assert_eq!(validate::validate(&ontology), Vec::new());
    // The dropped link survives the artifact round trip.
    let parsed = ttl::parse_turtle(&ttl::to_turtle(&ontology)).unwrap();
    assert_eq!(
        parsed.provenance.dropped_parent_links,
        ontology.provenance.dropped_parent_links
    );
}

/// Every document fails ⇒ the run fails LOUDLY. An empty ontology must
/// never come out of a corpus the model could not read.
#[tokio::test]
async fn all_documents_failing_fails_the_run() {
    let corpus_dir = tempfile::tempdir().unwrap();
    write(corpus_dir.path(), "a.md", "doc one");
    let corpus = load_corpus(corpus_dir.path()).unwrap();

    let server = scripted_server(vec!["never json".into(), "never json".into()]).await;

    let err = induction::induce(
        &client_for(&server),
        &corpus,
        &InductionConfig::new("allfail-test").unwrap(),
        &mut |_| {},
    )
    .await
    .expect_err("a run with zero usable documents must fail");
    let msg = format!("{err:#}");
    assert!(msg.contains("all 1 document(s) failed"), "{msg}");
}

/// The production file gate: an artifact someone hand-edited into
/// invalidity (duplicate prefLabel, undeclared range, subClassOf cycle) is
/// REJECTED by `load_validated` — the same gate `ontology validate`,
/// `promote_artifact` and `register_induced_from_path` all dispatch
/// through — with every violation named. Never repaired.
#[tokio::test]
async fn invalid_artifact_is_rejected_through_the_production_file_gate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("invalid.ttl");
    std::fs::write(
        &path,
        r#"
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        @prefix owl: <http://www.w3.org/2002/07/owl#> .
        @prefix skos: <http://www.w3.org/2004/02/skos/core#> .
        @prefix prism: <https://prism.marc27.com/ontology/meta#> .
        @prefix : <https://prism.marc27.com/ontology/badmodel#> .

        <https://prism.marc27.com/ontology/badmodel> a owl:Ontology ;
            prism:domain "badmodel" ;
            prism:status "draft" .

        :Alloy a owl:Class ;
            skos:prefLabel "Alloy"@en ;
            rdfs:subClassOf :Material .

        :Material a owl:Class ;
            skos:prefLabel "Material"@en ;
            rdfs:subClassOf :Alloy .

        :Alloy2 a owl:Class ;
            skos:prefLabel "alloy"@en .

        :hasProperty a owl:ObjectProperty ;
            skos:prefLabel "has property"@en ;
            rdfs:domain :Alloy ;
            rdfs:range :Ghost .
        "#,
    )
    .unwrap();

    let err = induction::load_validated(&path).expect_err("invalid artifact must be rejected");
    let msg = format!("{err:#}");
    assert!(msg.contains("REJECTED"), "{msg}");
    assert!(msg.contains("duplicate_class_label"), "{msg}");
    assert!(msg.contains("subclass_cycle"), "{msg}");
    assert!(msg.contains("undeclared_range"), "{msg}");
    assert!(msg.contains("Ghost"), "violations name the offender: {msg}");
}

/// Determinism: the same scripted responses over the same corpus produce
/// byte-identical artifacts up to the created_at timestamp — and the run's
/// attribution triple (model, prompt version, corpus hash) is stamped in.
#[tokio::test]
async fn same_corpus_same_responses_same_artifact_modulo_timestamp() {
    let corpus_dir = tempfile::tempdir().unwrap();
    write(corpus_dir.path(), "a.md", "Alloys.");
    let corpus = load_corpus(corpus_dir.path()).unwrap();

    let canned =
        r#"{"classes": [{"label": "Alloy", "definition": "d", "parent": null}], "relations": []}"#;
    let mut artifacts = Vec::new();
    for _ in 0..2 {
        let server = scripted_server(vec![canned.into()]).await;
        let mut ontology = induction::induce(
            &client_for(&server),
            &corpus,
            &InductionConfig::new("determinism-test").unwrap(),
            &mut |_| {},
        )
        .await
        .unwrap();
        ontology.provenance.created_at = "2026-08-09T00:00:00Z".into(); // the one varying field
        artifacts.push(ttl::to_turtle(&ontology));
    }
    assert_eq!(artifacts[0], artifacts[1]);
    assert!(artifacts[0].contains("prism:model \"scripted-test-model\""));
    // CONTRACT CHANGE: PROMPT_VERSION 2 made the prompt domain-abstract
    // (metallurgy examples replaced by placeholders). PROMPT_VERSION 3
    // changed what a prompt CARRIES: a document longer than one window is
    // read as several parts, and the already-known labels restated to the
    // model are sampled across the whole tree instead of its alphabetical
    // head. Artifacts induced after that bump stamp "3".
    assert!(artifacts[0].contains("prism:promptVersion \"3\""));
    assert!(artifacts[0].contains(&format!("prism:corpusHash \"{}\"", corpus.hash)));
    assert!(
        artifacts[0].contains("prism:status \"draft\""),
        "a fresh induction is a DRAFT"
    );
}
