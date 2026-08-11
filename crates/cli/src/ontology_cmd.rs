// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! `prism ontology` — LLM-driven ontology induction: corpus → TTL artifact.
//!
//! Ontologies are text, not code: the vocabulary itself is PRODUCED by a
//! model reading a corpus, emitted as a versioned Turtle artifact with
//! PRISM-namespace IRIs, provenance (model, prompt version, corpus hash),
//! and an explicit draft/accepted status. A fresh induction is always a
//! DRAFT; `promote` is the deliberate act that makes it eligible to govern
//! extraction (registration itself happens at load time through
//! `prism_ingest::induction::register` — the process-wide ontology
//! registry, not a parallel one).

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::Subcommand;
use prism_ingest::induction::{self, InductionConfig, align, corpus::load_corpus, ttl, validate};

#[derive(Debug, Subcommand)]
pub enum OntologyCommands {
    /// Induce a domain ontology from a corpus (a text/markdown/CSV file or
    /// a directory of them) and write it as a DRAFT TTL artifact.
    Induce {
        /// Corpus file or directory.
        corpus: PathBuf,
        /// Domain id, e.g. "alloys" or "polymers" — lowercase, becomes the
        /// ontology's registry id and its IRI namespace segment.
        #[arg(long)]
        domain: String,
        /// Output artifact path. Default: ./ontology-<domain>.ttl
        #[arg(long)]
        output: Option<PathBuf>,
        /// Reference vocabularies (.ttl file or directory) to align
        /// prefLabels against; matches become skos:exactMatch, everything
        /// else is recorded as unmatched.
        #[arg(long)]
        align: Vec<PathBuf>,
        /// Override LLM base URL (otherwise uses prism.toml / `prism use`).
        #[arg(long)]
        llm_url: Option<String>,
        /// Override LLM model.
        #[arg(long)]
        model: Option<String>,
        /// API key for authenticated LLM providers. Also reads LLM_API_KEY.
        #[arg(long, env = "LLM_API_KEY")]
        api_key: Option<String>,
    },
    /// Parse and validate an ontology artifact; exits non-zero listing the
    /// specific violations if it fails.
    Validate {
        /// Path to the TTL artifact.
        path: PathBuf,
    },
    /// Promote a DRAFT artifact to ACCEPTED, in place — the deliberate act
    /// that makes it eligible to govern extraction. Validation runs first.
    Promote {
        /// Path to the TTL artifact.
        path: PathBuf,
    },
}

pub async fn handle(command: OntologyCommands, project_root: &Path) -> Result<()> {
    match command {
        OntologyCommands::Induce {
            corpus,
            domain,
            output,
            align: align_sources,
            llm_url,
            model,
            api_key,
        } => {
            induce(
                &corpus,
                &domain,
                output.as_deref(),
                &align_sources,
                project_root,
                llm_url.as_deref(),
                model.as_deref(),
                api_key.as_deref(),
            )
            .await
        }
        OntologyCommands::Validate { path } => {
            let ontology = induction::load_validated(&path)?;
            println!(
                "OK: {} — status {}, {} classes, {} relations (model {}, prompt v{}, corpus {}; semantic {})",
                path.display(),
                ontology.status.as_str(),
                ontology.classes.len(),
                ontology.relations.len(),
                ontology.provenance.model,
                ontology.provenance.prompt_version,
                ontology.provenance.corpus_hash,
                ontology
                    .provenance
                    .semantic_validation
                    .near_duplicates
                    .status
                    .as_str(),
            );
            Ok(())
        }
        OntologyCommands::Promote { path } => {
            let ontology = ttl::promote_artifact(&path)?;
            println!(
                "PROMOTED: {} — ontology '{}' is now ACCEPTED (promoted at {})",
                path.display(),
                ontology.domain,
                ontology
                    .provenance
                    .promoted_at
                    .as_deref()
                    .unwrap_or("unknown"),
            );
            println!(
                "  semantic near-duplicate check: {}",
                ontology
                    .provenance
                    .semantic_validation
                    .near_duplicates
                    .status
                    .as_str()
            );
            println!(
                "It is now eligible for registration as an extraction vocabulary \
                 (select it with [ontology] id = \"{}\" once loaded).",
                ontology.domain
            );
            Ok(())
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn induce(
    corpus_path: &Path,
    domain: &str,
    output: Option<&Path>,
    align_sources: &[PathBuf],
    project_root: &Path,
    llm_url: Option<&str>,
    model: Option<&str>,
    api_key: Option<&str>,
) -> Result<()> {
    let config = InductionConfig::new(domain)?;

    // Alignment sources load FIRST: a typo'd reference path must fail before
    // any model spend, not after.
    let alignment = if align_sources.is_empty() {
        None
    } else {
        Some(align::load_alignment(align_sources)?)
    };

    let corpus = load_corpus(corpus_path)?;
    println!(
        "Corpus: {} document(s), hash {}",
        corpus.docs.len(),
        corpus.hash
    );

    let llm_config = crate::build_llm_config(project_root, llm_url, model, api_key)?;
    let client = prism_ingest::llm::LlmClient::new(llm_config);
    // Deliberately NO LlmClient::health_check here: it probes
    // `{base}/v1/models`, while generate_json takes the base URL at face
    // value — so against Ollama's documented `…/v1` base the health check
    // 404s on `/v1/v1/models` even though extraction works. Induction's
    // all-documents-failed error carries the real transport error instead.
    println!(
        "Inducing '{domain}' ontology with model {} …",
        client.config().model
    );

    let mut ontology = induction::induce(&client, &corpus, &config).await?;

    if let Some(index) = &alignment {
        let outcome = align::align(&mut ontology, index);
        println!(
            "Alignment: {} matched, {} unmatched (index: {} labels from {} source file(s))",
            outcome.matched,
            outcome.unmatched.len(),
            index.len(),
            index.sources.len()
        );
    }

    // Retain and measure the RAW model surfaces before the builder's
    // established lexical merge can hide variants such as `HeatTreatment`
    // and `Heat Treatment`. This is report-only: neither findings nor an
    // unavailable backend can alter or block the draft artifact.
    let semantic_labels = ontology.provenance.semantic_validation.proposals.clone();
    ontology.provenance.semantic_validation =
        prism_ingest::semantic_validation::validate_ontology_labels_best_effort(
            &semantic_labels,
            &config.semantic_validation,
        )
        .await;

    // The strict gate: a generated ontology that fails validation is
    // rejected LOUDLY with the specific violations — no artifact is written.
    let violations = validate::validate(&ontology);
    if !violations.is_empty() {
        let mut msg = format!(
            "induced ontology '{domain}' REJECTED: {} violation(s) — no artifact written:",
            violations.len()
        );
        for v in &violations {
            msg.push_str(&format!("\n  [{}] {}", v.rule, v.message));
        }
        bail!(msg);
    }

    let default_path = PathBuf::from(format!("ontology-{domain}.ttl"));
    let out_path = output.unwrap_or(&default_path);
    ttl::write_artifact(out_path, &ontology)?;

    let p = &ontology.provenance;
    println!(
        "DRAFT artifact written: {} — {} classes, {} relations",
        out_path.display(),
        ontology.classes.len(),
        ontology.relations.len()
    );
    println!(
        "  provenance: model {}, prompt v{}, {}/{} document(s) used, corpus {}",
        p.model,
        p.prompt_version,
        p.documents_total - p.documents_failed,
        p.documents_total,
        p.corpus_hash
    );
    println!(
        "  semantic near-duplicate check: {} ({} raw labels, {} finding(s))",
        p.semantic_validation.near_duplicates.status.as_str(),
        p.semantic_validation.near_duplicates.candidates,
        p.semantic_validation.near_duplicates.findings.len(),
    );
    if let Some(message) = &p.semantic_validation.near_duplicates.message {
        println!("    {message}");
    }
    for finding in &p.semantic_validation.near_duplicates.findings {
        println!(
            "    collision: {} {:?} vs {} {:?} (cosine distance {:.4}, edit {:.4})",
            finding.proposed_type,
            finding.proposed_name,
            finding.colliding_type,
            finding.colliding_name,
            finding.cosine_distance,
            finding.normalized_edit_distance,
        );
    }
    if p.documents_failed > 0 {
        println!(
            "  {} document(s) produced no usable proposal",
            p.documents_failed
        );
    }
    if p.malformed_items > 0 {
        println!(
            "  {} malformed class/relation item(s) dropped",
            p.malformed_items
        );
    }
    for link in &p.dropped_parent_links {
        println!("  dropped cycle-creating parent link: {link}");
    }
    for note in &p.merge_notes {
        println!("  merge note: {note}");
    }
    let unmatched: Vec<&str> = ontology
        .classes
        .iter()
        .filter(|c| c.aligned_iri.is_none())
        .map(|c| c.label.as_str())
        .collect();
    if alignment.is_some() && !unmatched.is_empty() {
        println!("  unmatched (pending alignment): {}", unmatched.join(", "));
    }
    println!(
        "This is a DRAFT — review it, then promote deliberately: \
         prism ontology promote {}",
        out_path.display()
    );
    Ok(())
}
