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

use anyhow::{Context, Result, bail};
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
        /// Characters of a document fed per prompt. A paper is read as
        /// successive overlapping windows of this size.
        #[arg(long, default_value_t = 4000)]
        max_doc_chars: usize,
        /// Cap on windows read per document. 0 reads the whole paper; 1 reads
        /// only its opening window.
        #[arg(long, default_value_t = 0)]
        max_windows: usize,
    },
    /// Parse and validate an ontology artifact; exits non-zero listing the
    /// specific violations if it fails.
    Validate {
        /// Path to the TTL artifact.
        path: PathBuf,
    },
    /// Promote a DRAFT artifact to ACCEPTED, in place, and install it in the
    /// project catalog for later ingest processes. Validation runs first.
    Promote {
        /// Path to the TTL artifact.
        path: PathBuf,
    },
    /// List registered ontologies: the builtins plus every artifact in the
    /// project catalog (`.prism/ontologies/`). A broken catalog artifact is
    /// a named failure, not a silently missing row. The LIST half of the
    /// standard plugin contract for the ontology plane (same inventory as
    /// `prism plugins list`, ontology section, and the TUI/agent routes).
    List,
    /// Review the ontology-extension proposals the paper reader queued:
    /// list them with their citations, accept them into a DRAFT artifact
    /// (which then goes through the normal `promote` gate), or reject them
    /// (permanently, for that identity).
    Proposals {
        #[command(subcommand)]
        command: ProposalCommands,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProposalCommands {
    /// List pending proposals with their citation counts. The review work
    /// queue, oldest first.
    List {
        /// Maximum proposals to show.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Output JSON (the same shape the agent tool consumes).
        #[arg(long)]
        json: bool,
    },
    /// Show one proposal in full: its content and every citation that backs
    /// it (document, lines, quoted text).
    Show { item_id: String },
    /// Accept proposals into a DRAFT ontology artifact. NEVER promotes: the
    /// draft then goes through the ordinary deliberate gate
    /// (`prism ontology promote <artifact>`), exactly like an induction.
    Accept {
        /// Proposal ids to accept (from `proposals list`).
        item_ids: Vec<String>,
        /// Domain id for a NEW artifact (required when no --output artifact
        /// exists to extend).
        #[arg(long)]
        domain: Option<String>,
        /// Artifact to write. When the path holds an existing PRISM artifact
        /// it is EXTENDED (existing declarations preserved); otherwise a new
        /// artifact is created. Default: ontology-<domain>-candidate.ttl.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Who decided, for the ledger (`human:<id>` or `agent:<model>`).
        #[arg(long, default_value = "human:cli")]
        by: String,
        /// Why (recorded in the disposition ledger).
        #[arg(long)]
        reason: Option<String>,
    },
    /// Reject proposals. FINAL for the identity: a rejected proposal is
    /// never re-queued by later ingests.
    Reject {
        /// Proposal ids to reject.
        item_ids: Vec<String>,
        /// Why (required — a rejection without a recorded reason is unauditable).
        #[arg(long)]
        reason: String,
        /// Who decided, for the ledger.
        #[arg(long, default_value = "human:cli")]
        by: String,
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
            max_doc_chars,
            max_windows,
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
                max_doc_chars,
                max_windows,
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
            let installed = install_promoted_artifact(project_root, &ontology)?;
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
                "INSTALLED: {} — future local and paper ingest processes load it when \
                 [ontology] id = \"{}\".",
                installed.display(),
                ontology.domain,
            );
            Ok(())
        }
        OntologyCommands::List => list(project_root),
        OntologyCommands::Proposals { command } => proposals(command, project_root).await,
    }
}

// ── Ontology extension proposal governance ─────────────────────────────

/// Where the governance queue lives — the same store every ingest path
/// writes (`~/.prism/provenance.db`).
fn proposal_store_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let _ = home;
    // Honours $PRISM_PROVENANCE_DB — pointing the ontology tools at a chosen
    // corpus used to open the DEFAULT store and collide with the node's lock.
    Ok(prism_provenance::store_path())
}

async fn proposals(command: ProposalCommands, project_root: &Path) -> Result<()> {
    let store = prism_provenance::ProvenanceStore::open(&proposal_store_path()?).await?;
    match command {
        ProposalCommands::List { limit, json } => {
            let pending = store.pending_ontology_proposals(limit as i64).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&pending_proposals_json(pending, limit))?
                );
            } else {
                if pending.is_empty() {
                    println!(
                        "no pending ontology proposals (queue empty — ingest a paper to grow one)"
                    );
                    return Ok(());
                }
                println!("pending ontology proposals (oldest first):");
                for (item, sightings) in pending {
                    println!(
                        "  [{}] {} — {} citation(s) (first: {})",
                        item.kind, item.item_id, sightings, item.document
                    );
                    println!(
                        "      inspect: prism ontology proposals show {:?}",
                        item.item_id
                    );
                }
            }
            Ok(())
        }
        ProposalCommands::Show { item_id } => {
            let item = store
                .ontology_proposal_by_id(&item_id)
                .await?
                .with_context(|| format!("no pending proposal {item_id:?}"))?;
            let sightings = store.ontology_proposal_sightings(&item_id).await?;
            let decided = store.ontology_proposal_dispositions(&item_id).await?;
            println!("{} [{}]", item.label, item.kind);
            println!(
                "  content: {}",
                serde_json::to_string_pretty(
                    &serde_json::from_str::<serde_json::Value>(&item.proposal_json)
                        .context("stored proposal_json is not valid JSON")?
                )?
            );
            println!("  citations ({}):", sightings.len());
            for sighting in &sightings {
                let citation: serde_json::Value = serde_json::from_str(&sighting.citation_json)
                    .context("stored citation_json is not valid JSON")?;
                println!(
                    "    {} lines {}-{}: {:?}",
                    sighting.document,
                    citation["from_line"]
                        .as_u64()
                        .map_or_else(|| "?".into(), |v| v.to_string()),
                    citation["to_line"]
                        .as_u64()
                        .map_or_else(|| "?".into(), |v| v.to_string()),
                    citation["quoted_text"].as_str().unwrap_or("")
                );
            }
            if !decided.is_empty() {
                // Unreachable while the item is queued (a decision removes it
                // from the queue), but sightings remain readable by id — say
                // what happened instead of pretending it is pending.
                println!("  dispositions:");
                for d in &decided {
                    println!(
                        "    {} by {} at {}: {}",
                        d.outcome, d.dispositioner, d.decided_at, d.reason
                    );
                }
            }
            Ok(())
        }
        ProposalCommands::Accept {
            item_ids,
            domain,
            output,
            by,
            reason,
        } => {
            if item_ids.is_empty() {
                bail!("accept requires at least one proposal id (see `proposals list`)");
            }
            let artifact =
                accept_proposals(&store, &item_ids, domain.as_deref(), output, project_root)
                    .await?;
            let decided_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs_f64())
                .unwrap_or(0.0);
            for item_id in &item_ids {
                record_disposition(
                    &store,
                    item_id,
                    "accepted",
                    Some(artifact.display().to_string()),
                    reason.clone().unwrap_or_else(|| {
                        "accepted into a draft artifact; promotion is a separate deliberate act"
                            .to_string()
                    }),
                    &by,
                    decided_at,
                )
                .await?;
            }
            println!(
                "ACCEPTED: {} proposal(s) → DRAFT artifact {}",
                item_ids.len(),
                artifact.display()
            );
            println!(
                "  next: review the artifact, then `prism ontology promote {}` — acceptance does NOT promote",
                artifact.display()
            );
            Ok(())
        }
        ProposalCommands::Reject {
            item_ids,
            reason,
            by,
        } => {
            if item_ids.is_empty() {
                bail!("reject requires at least one proposal id (see `proposals list`)");
            }
            if reason.trim().is_empty() {
                bail!(
                    "reject requires --reason: a rejection without a recorded reason is unauditable"
                );
            }
            let decided_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs_f64())
                .unwrap_or(0.0);
            for item_id in &item_ids {
                record_disposition(
                    &store,
                    item_id,
                    "rejected",
                    None,
                    reason.clone(),
                    &by,
                    decided_at,
                )
                .await?;
            }
            println!(
                "REJECTED: {} proposal(s) — these identities are never re-queued by later ingests",
                item_ids.len()
            );
            Ok(())
        }
    }
}

/// The `proposals list --json` payload, factored out so the review surface's
/// machine shape is testable without capturing stdout.
fn pending_proposals_json(
    pending: Vec<(prism_provenance::OntologyProposalItem, i64)>,
    limit: usize,
) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = pending
        .into_iter()
        .map(|(item, sightings)| {
            serde_json::json!({
                "item_id": item.item_id,
                "kind": item.kind,
                "label": item.label,
                "document": item.document,
                "sightings": sightings,
                "proposal": serde_json::from_str::<serde_json::Value>(
                    &item.proposal_json
                ).unwrap_or(serde_json::Value::Null),
            })
        })
        .collect();
    serde_json::json!({ "pending": rows, "limit": limit })
}

async fn record_disposition(
    store: &prism_provenance::ProvenanceStore,
    item_id: &str,
    outcome: &str,
    artifact_path: Option<String>,
    reason: String,
    by: &str,
    decided_at: f64,
) -> Result<()> {
    let item = store
        .ontology_proposal_by_id(item_id)
        .await?
        .with_context(|| format!("no pending proposal {item_id:?}"))?;
    store
        .record_ontology_proposal_disposition(&prism_provenance::OntologyProposalDisposition {
            item_id: item.item_id,
            document: item.document,
            kind: item.kind,
            label: item.label,
            outcome: outcome.to_string(),
            artifact_path,
            reason,
            dispositioner: by.to_string(),
            decided_at,
        })
        .await
}

/// Accept proposals into a DRAFT artifact and return the artifact path.
///
/// This feeds the EXISTING promotion path — the artifact is exactly what
/// `prism ontology induce` produces (same writer, same validation, same
/// draft status), so `prism ontology promote` and registration work on it
/// unchanged. Acceptance itself never promotes: a freshly accepted class is
/// still a proposal until someone deliberately runs the promotion gate.
///
/// When `output` names an existing PRISM artifact the draft EXTENDS it
/// (every existing declaration is preserved verbatim); otherwise a new
/// artifact is built from the proposals alone, with parent/endpoint classes
/// declared by reference — the artifact format's supported way to name a
/// class from another vocabulary.
async fn accept_proposals(
    store: &prism_provenance::ProvenanceStore,
    item_ids: &[String],
    domain: Option<&str>,
    output: Option<PathBuf>,
    project_root: &Path,
) -> Result<PathBuf> {
    use prism_ingest::induction::{
        InducedOntology, ModelProposal, OntologyBuilder, OntologyStatus, ProposedClass,
        ProposedRelation,
    };
    use sha2::Digest as _;

    let mut items = Vec::with_capacity(item_ids.len());
    for item_id in item_ids {
        items.push(
            store
                .ontology_proposal_by_id(item_id)
                .await?
                .with_context(|| format!("no pending proposal {item_id:?}"))?,
        );
    }

    // The proposals reference classes of the ontology they were read
    // against. Resolve those IRIs to labels through the ACTIVE project
    // ontology — if the active ontology changed since the proposal was
    // made, the unresolvable IRIs are named loudly rather than dropped.
    let active = prism_ingest::ontologies::active_for_project_config(project_root)?;
    let label_of = |iri: &str| -> Option<String> {
        active
            .ontology_classes()
            .iter()
            .find(|class| class.iri.as_str() == iri)
            .and_then(|class| class.pref_label.clone())
    };

    let mut unresolved = Vec::new();
    let mut classes = Vec::new();
    let mut relations = Vec::new();
    let mut extra_parent_notes = Vec::new();
    for item in &items {
        let content: serde_json::Value = serde_json::from_str(&item.proposal_json)
            .with_context(|| format!("proposal {} content is not valid JSON", item.item_id))?;
        if item.kind == "class" {
            let parent_iris: Vec<String> = content["parent_iris"]
                .as_array()
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| entry.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let mut parents = Vec::with_capacity(parent_iris.len());
            for iri in &parent_iris {
                match label_of(iri) {
                    Some(label) => parents.push(label),
                    None => unresolved.push(format!("{}: parent {iri}", item.item_id)),
                }
            }
            // The artifact's class shape carries ONE parent; additional
            // proposed parents are recorded as provenance notes, never
            // silently dropped.
            let parent = parents.first().cloned();
            if parents.len() > 1 {
                extra_parent_notes.push(format!(
                    "class '{}': kept parent '{}', recorded (not asserted) additional \
                     proposed parents: {}",
                    content["label"].as_str().unwrap_or(""),
                    parents[0],
                    parents[1..].join(", ")
                ));
            }
            classes.push(ProposedClass {
                label: content["label"].as_str().unwrap_or_default().to_string(),
                definition: content["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                parent,
            });
        } else {
            let (Some(source), Some(target)) = (
                content["source_class_iri"].as_str(),
                content["target_class_iri"].as_str(),
            ) else {
                bail!("relation proposal {} has no endpoint IRIs", item.item_id);
            };
            let (Some(domain_label), Some(range_label)) = (label_of(source), label_of(target))
            else {
                unresolved.push(format!("{}: endpoints {source} / {target}", item.item_id));
                continue;
            };
            relations.push(ProposedRelation {
                label: content["label"].as_str().unwrap_or_default().to_string(),
                definition: content["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                domain: domain_label,
                range: range_label,
            });
        }
    }
    if !unresolved.is_empty() {
        bail!(
            "cannot accept: the active ontology no longer declares classes these \
             proposals reference (re-read them against the current ontology): {}",
            unresolved.join("; ")
        );
    }

    // The model's suggested IRIs ride as skos:exactMatch candidates so the
    // alignment step of a later governance pass can adopt or refuse them.
    let proposed_iris: Vec<(String, String)> = items
        .iter()
        .filter_map(|item| {
            let content: serde_json::Value = serde_json::from_str(&item.proposal_json).ok()?;
            let label = content["label"].as_str()?.to_string();
            let iri = content["proposed_iri"].as_str()?.to_string();
            Some((label, iri))
        })
        .collect();

    let output_path = match (&output, domain) {
        (Some(path), _) => path.clone(),
        (None, Some(domain)) => PathBuf::from(format!("ontology-{domain}-candidate.ttl")),
        (None, None) => {
            bail!(
                "accept needs --domain (for a new artifact) or --output naming an \
                 existing artifact to extend"
            )
        }
    };

    let mut provenance = prism_ingest::induction::InductionProvenance {
        model: "ontology-proposal-governance".to_string(),
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ..Default::default()
    };
    // Attributable identity: the artifact differs with the proposal set.
    // Hash the sorted ids so the value is a real digest, matching the
    // `sha256:<hex>` contract the field documents.
    let mut ids = item_ids.to_vec();
    ids.sort();
    let ids_joined = ids.join("\n");
    provenance.corpus_hash = format!(
        "sha256:{}",
        sha2::Sha256::digest(ids_joined.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    provenance.merge_notes.extend(extra_parent_notes);

    let mut ontology = if output_path.exists() {
        let mut existing = induction::load_validated(&output_path)?;
        if let Some(domain) = domain
            && domain != existing.domain
        {
            bail!(
                "--domain {domain:?} conflicts with the existing artifact's domain {:?}",
                existing.domain
            );
        }
        // Extending an ACCEPTED artifact produces a DRAFT of the same
        // vocabulary: the accepted original keeps governing writes until
        // the extended draft is deliberately promoted again.
        existing.status = OntologyStatus::Draft;
        existing.provenance.promoted_at = None;
        existing
    } else {
        let domain = domain.context("--domain is required for a new artifact")?;
        InducedOntology {
            domain: domain.to_string(),
            status: OntologyStatus::Draft,
            classes: Vec::new(),
            relations: Vec::new(),
            provenance: Default::default(),
        }
    };

    // Absorb through the SAME builder machinery induction uses, so
    // duplicate merging, referential closure and cycle-breaking behave
    // identically for governed extensions and induced drafts.
    let mut builder = OntologyBuilder::new(&ontology.domain)?;
    builder.absorb(ModelProposal { classes, relations });
    let absorbed = builder.finish(Default::default());
    for class in absorbed.classes {
        if !ontology.classes.iter().any(|existing| {
            prism_ingest::induction::normalize_label(&existing.label)
                == prism_ingest::induction::normalize_label(&class.label)
        }) {
            ontology.classes.push(class);
        }
    }
    for relation in absorbed.relations {
        if !ontology.relations.iter().any(|existing| {
            prism_ingest::induction::normalize_label(&existing.label)
                == prism_ingest::induction::normalize_label(&relation.label)
        }) {
            ontology.relations.push(relation);
        }
    }
    // Carry the builder's structural notes (cycle drops, merges) so nothing
    // the builder decided is invisible in the artifact.
    provenance
        .merge_notes
        .extend(absorbed.provenance.merge_notes);
    provenance
        .dropped_parent_links
        .extend(absorbed.provenance.dropped_parent_links);
    provenance.malformed_items += absorbed.provenance.malformed_items;

    // Attach the model's proposed IRIs as alignment candidates.
    for (label, iri) in proposed_iris {
        for class in &mut ontology.classes {
            if prism_ingest::induction::normalize_label(&class.label)
                == prism_ingest::induction::normalize_label(&label)
                && class.aligned_iri.is_none()
                && !class.declared_by_reference
            {
                class.aligned_iri = Some(iri);
                break;
            }
        }
    }

    ontology.provenance = provenance;
    // Deterministic artifact order, same as a fresh induction.
    ontology
        .classes
        .sort_by_key(|c| prism_ingest::induction::class_slug(&c.label).unwrap_or_default());
    ontology
        .relations
        .sort_by_key(|r| prism_ingest::induction::relation_slug(&r.label).unwrap_or_default());

    // The strict gate, identical to induce: an artifact that fails
    // validation is refused loudly and NOTHING is written or dispositioned.
    let violations = prism_ingest::induction::validate::validate(&ontology);
    if !violations.is_empty() {
        let mut msg =
            "accepting these proposals would produce an INVALID artifact — nothing written:\n"
                .to_string();
        for v in &violations {
            msg.push_str(&format!("  [{}] {}\n", v.rule, v.message));
        }
        bail!(msg.trim_end().to_string());
    }

    ttl::write_artifact(&output_path, &ontology)?;
    Ok(output_path)
}

/// LIST half of the standard plugin contract for the ontology plane.
fn list(project_root: &Path) -> Result<()> {
    for line in collect_listing(project_root) {
        println!("{line}");
    }
    Ok(())
}

/// The listing rows, factored out of [`list`] so the contract (builtins
/// always present; a broken artifact is a named failure, never a missing
/// row) is testable without capturing stdout.
fn collect_listing(project_root: &Path) -> Vec<String> {
    let mut lines = vec!["registered ontologies:".to_string()];
    let registry = prism_ingest::ontologies::OntologyRegistry::builtin();
    for id in registry.ids() {
        lines.push(format!("  builtin: {id}"));
    }
    let catalog = project_root.join(prism_ingest::ontologies::PROJECT_ONTOLOGY_DIR);
    let mut found_any = false;
    if let Ok(entries) = std::fs::read_dir(&catalog) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ttl") {
                continue;
            }
            found_any = true;
            match induction::load_validated(&path) {
                Ok(ontology) => lines.push(format!(
                    "  project: {} — status {}, {} classes, {} relations ({})",
                    ontology.domain,
                    ontology.status.as_str(),
                    ontology.classes.len(),
                    ontology.relations.len(),
                    path.display()
                )),
                Err(error) => lines.push(format!("  FAILED: {} — {error:#}", path.display())),
            }
        }
    }
    if !found_any {
        lines.push(format!(
            "  (no project artifacts in {} — create one with `prism ontology induce`)",
            catalog.display()
        ));
    }
    lines
}

/// Materialise a promoted ontology in the project-local catalog used by
/// later ingest processes. `promote_artifact` has already canonicalised and
/// validated the source; writing the parsed object through the same TTL
/// writer preserves that canonical materialisation and uses its atomic
/// temp-file/rename path.
pub(crate) fn install_promoted_artifact(
    project_root: &Path,
    ontology: &induction::InducedOntology,
) -> Result<PathBuf> {
    let installed =
        prism_ingest::ontologies::project_ontology_artifact_path(project_root, &ontology.domain)?;
    let directory = installed
        .parent()
        .expect("the project ontology artifact path always has a parent");
    std::fs::create_dir_all(directory).with_context(|| {
        format!(
            "cannot create project ontology catalog {}",
            directory.display()
        )
    })?;
    ttl::write_artifact(&installed, ontology)?;
    Ok(installed)
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
    max_doc_chars: usize,
    max_windows: usize,
) -> Result<()> {
    let mut config = InductionConfig::new(domain)?;
    if max_doc_chars > 0 {
        config.max_doc_chars = max_doc_chars;
    }
    config.max_windows_per_doc = max_windows;

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

    // Print every window as it lands. A run over a real corpus is hundreds of
    // LLM calls and tens of minutes; without this the only two observable
    // states are "still going" and "finished", which makes a wedged run
    // indistinguishable from a working one until it is far too late.
    let started = std::time::Instant::now();
    let mut last_doc = 0usize;
    let mut on_progress = |p: induction::InductionProgress<'_>| {
        if p.doc_index != last_doc {
            last_doc = p.doc_index;
            println!("  [{}/{}] {}", p.doc_index, p.doc_total, p.document);
        }
        println!(
            "      window {}/{} {} — {} class(es), {} relation(s), {:.0}s elapsed",
            p.window,
            p.window_total,
            if p.absorbed { "ok" } else { "UNUSABLE" },
            p.classes,
            p.relations,
            started.elapsed().as_secs_f64(),
        );
        use std::io::Write;
        let _ = std::io::stdout().flush();
    };
    let mut ontology = induction::induce(&client, &corpus, &config, &mut on_progress).await?;

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
    // How much of each paper actually reached the model. A corpus read only
    // in part must never look like a corpus read whole.
    if p.windows_attempted > 0 {
        println!(
            "  read: {}/{} window(s) of the corpus absorbed{}",
            p.windows_read,
            p.windows_attempted,
            if p.windows_read < p.windows_attempted {
                " — the rest overflowed the model's context or returned unusable JSON"
            } else {
                ""
            }
        );
    }
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

#[cfg(test)]
mod tests {
    use prism_ingest::induction::{
        InducedClass, InducedOntology, InductionProvenance, OntologyStatus,
    };
    use prism_ingest::ontologies::OntologyRegistry;

    use super::*;

    fn draft_ontology(domain: &str) -> InducedOntology {
        InducedOntology {
            domain: domain.to_string(),
            status: OntologyStatus::Draft,
            classes: vec![InducedClass {
                label: "Active Ingredient".to_string(),
                definition: "A concept declared by the supplied ontology.".to_string(),
                parent: None,
                aligned_iri: None,
                declared_by_reference: false,
                sign_domain: None,
            }],
            relations: Vec::new(),
            provenance: InductionProvenance {
                corpus_hash: "sha256:project-catalog-test".to_string(),
                prompt_version: "test".to_string(),
                ..InductionProvenance::default()
            },
        }
    }

    /// CONTRACT CHANGE: promotion used to affect only the current artifact
    /// file and a later process could not resolve its configured id. The
    /// promoted bytes are now installed in the project catalog, and two
    /// independent registries (standing in for two CLI processes) can load
    /// the same vocabulary solely from `[ontology] id` plus project state.
    #[test]
    fn promotion_persists_for_a_fresh_registry_and_configured_id() {
        // CONTRACT CHANGE: promotion is now durable project state, not an
        // in-memory eligibility message that disappears with the CLI process.
        let project = tempfile::tempdir().expect("project tempdir");
        let source = project.path().join("reviewed-candidate.ttl");
        ttl::write_artifact(&source, &draft_ontology("pharma-custom"))
            .expect("write draft ontology");

        let promoted = ttl::promote_artifact(&source).expect("promote reviewed ontology");
        let installed = install_promoted_artifact(project.path(), &promoted)
            .expect("install promoted artifact in project catalog");
        assert_eq!(
            installed,
            project.path().join(".prism/ontologies/pharma-custom.ttl")
        );
        assert_eq!(
            std::fs::read(&source).expect("read promoted source"),
            std::fs::read(&installed).expect("read installed artifact"),
            "the catalog must preserve the exact promoted materialisation"
        );

        std::fs::write(
            project.path().join(".prism/prism.toml"),
            "[ontology]\nid = \"pharma-custom\"\n",
        )
        .expect("write project config");
        let configured_id = prism_core::config::NodeConfig::load(Some(project.path()))
            .ontology
            .id;

        let expected_sha = {
            let mut promotion_process = OntologyRegistry::builtin();
            let first = promotion_process
                .load_project(project.path(), &configured_id)
                .expect("promotion process can load installed ontology");
            first.artifact_sha256().to_string()
        };

        let mut later_ingest_process = OntologyRegistry::builtin();
        let reloaded = later_ingest_process
            .load_project(project.path(), &configured_id)
            .expect("fresh ingest process reloads configured ontology");
        assert_eq!(reloaded.id(), "pharma-custom");
        assert_eq!(reloaded.artifact_sha256(), expected_sha);
        assert_eq!(
            reloaded
                .class_for_label("ActiveIngredient")
                .and_then(|class| class.pref_label.as_deref()),
            Some("Active Ingredient")
        );
    }
    /// STANDARD PLUGIN CONTRACT — LIST rule, ontology plane: builtins are
    /// always listed, a promoted project artifact appears with its status,
    /// and a BROKEN artifact is a named failure, never a missing row.
    #[test]
    fn ontology_listing_names_broken_artifacts_and_promoted_ones() {
        let project = tempfile::tempdir().expect("project tempdir");
        let catalog = project.path().join(".prism/ontologies");
        std::fs::create_dir_all(&catalog).expect("catalog dir");
        std::fs::write(catalog.join("broken.ttl"), "not turtle @ ; ;").expect("broken artifact");

        let lines = collect_listing(project.path());
        assert!(lines.iter().any(|l| l.contains("builtin: emmo")));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("FAILED") && l.contains("broken.ttl")),
            "{lines:?}"
        );

        // A promoted artifact joins the listing with its domain and status.
        let source = project.path().join("candidate.ttl");
        ttl::write_artifact(&source, &draft_ontology("pharma-listed"))
            .expect("write draft ontology");
        let promoted = ttl::promote_artifact(&source).expect("promote");
        install_promoted_artifact(project.path(), &promoted).expect("install");
        let lines = collect_listing(project.path());
        assert!(
            lines
                .iter()
                .any(|l| l.contains("project: pharma-listed") && l.contains("accepted")),
            "{lines:?}"
        );
    }

    /// Restores `$HOME` on drop. Only construct while holding
    /// `boot_checks::ENV_LOCK` — the variable is process-global.
    struct HomeRestore(Option<std::ffi::OsString>);

    impl HomeRestore {
        fn isolated(home: &Path) -> Self {
            let guard = Self(std::env::var_os("HOME"));
            unsafe { std::env::set_var("HOME", home) };
            guard
        }
    }

    impl Drop for HomeRestore {
        fn drop(&mut self) {
            unsafe {
                match self.0.take() {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    /// The governance loop, closed end-to-end through the PRODUCTION CLI
    /// handlers: a queued proposal with its citation is listed, accepted
    /// into a DRAFT artifact that the EXISTING promotion gate accepts, and
    /// the decision survives in the ledger; a rejected identity is never
    /// re-queued. Nothing here builds its own registry or store — the
    /// handlers derive everything from `$HOME` and the project root, which
    /// is exactly what a real invocation does.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn proposal_governance_roundtrip_through_the_cli_surface() {
        let _guard = crate::boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // A project whose ACTIVE ontology is a promoted induced artifact —
        // so the proposals' parent/endpoint IRIs resolve through the real
        // `active_for_project_config` path the accept handler uses.
        let project = tempfile::tempdir().expect("project tempdir");
        let neutral = InducedClass {
            label: "NeutralEntity".to_string(),
            definition: "A neutral test concept.".to_string(),
            parent: None,
            aligned_iri: None,
            declared_by_reference: false,
            sign_domain: None,
        };
        let mut draft = draft_ontology("gov-active");
        draft.classes = vec![neutral];
        let source = project.path().join("gov-active-candidate.ttl");
        ttl::write_artifact(&source, &draft).expect("write draft");
        let promoted = ttl::promote_artifact(&source).expect("promote");
        install_promoted_artifact(project.path(), &promoted).expect("install");
        std::fs::create_dir_all(project.path().join(".prism")).expect("project .prism");
        std::fs::write(
            project.path().join(".prism/prism.toml"),
            "[ontology]\nid = \"gov-active\"\n",
        )
        .expect("project config");
        let root = project.path().to_path_buf();

        // An isolated HOME so the governance store is a temp database.
        let home = tempfile::tempdir().expect("home tempdir");
        std::fs::create_dir_all(home.path().join(".prism")).expect("home .prism");
        let _restore_home = HomeRestore::isolated(home.path());

        let store = prism_provenance::ProvenanceStore::open(&proposal_store_path().unwrap())
            .await
            .expect("open governance store");

        // Enqueue exactly what an ingest would: the production queue-item
        // builders with a real citation.
        let parent_iri = "https://prism.marc27.com/ontology/gov-active#NeutralEntity".to_string();
        let class_proposal = prism_ingest::paper_agent::OntologyClassProposal {
            label: "Feedstock Powder".to_string(),
            proposed_iri: None,
            parent_iris: vec![parent_iri.clone()],
            description: Some("Powder fed into a process.".to_string()),
            citation: prism_ingest::paper_agent::PaperCitation {
                source_revision_id: "ab".repeat(32),
                from_line: 2,
                to_line: 2,
                quoted_text: "feedstock powders were sieved before use".to_string(),
            },
        };
        let relation_proposal = prism_ingest::paper_agent::OntologyRelationProposal {
            label: "processed from powder".to_string(),
            proposed_iri: None,
            source_class_iri: parent_iri.clone(),
            target_class_iri: parent_iri,
            description: None,
            citation: prism_ingest::paper_agent::PaperCitation {
                source_revision_id: "cd".repeat(32),
                from_line: 3,
                to_line: 3,
                quoted_text: "parts were processed from powder feedstock".to_string(),
            },
        };
        let (class_item, class_citation) = prism_ingest::paper_agent::class_proposal_queue_item(
            &class_proposal,
            "paper.pdf",
            "local",
            1.0,
        );
        let (relation_item, relation_citation) =
            prism_ingest::paper_agent::relation_proposal_queue_item(
                &relation_proposal,
                "paper.pdf",
                "local",
                2.0,
            );
        store
            .enqueue_ontology_proposal(&class_item, &class_citation, 1.0)
            .await
            .unwrap();
        store
            .enqueue_ontology_proposal(&relation_item, &relation_citation, 2.0)
            .await
            .unwrap();

        // LIST — the review surface's work queue, through the same row
        // builder the JSON output prints.
        let listed =
            pending_proposals_json(store.pending_ontology_proposals(10).await.unwrap(), 10);
        assert_eq!(listed["pending"].as_array().unwrap().len(), 2);
        let class_row = listed["pending"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"] == "class")
            .expect("the class proposal is listed");
        assert_eq!(class_row["sightings"], 1);
        assert_eq!(
            class_row["proposal"]["parent_iris"][0],
            class_proposal.parent_iris[0]
        );

        // ACCEPT the class proposal through the production handler.
        let artifact = project.path().join("gov-extension-candidate.ttl");
        proposals(
            ProposalCommands::Accept {
                item_ids: vec![class_item.item_id.clone()],
                domain: Some("gov-extension".to_string()),
                output: Some(artifact.clone()),
                by: "human:reviewer".to_string(),
                reason: None,
            },
            &root,
        )
        .await
        .expect("accept the class proposal");

        // The artifact is a DRAFT an ordinary promotion gate accepts, with
        // the new class and the parent declared by reference — acceptance
        // fed the EXISTING path, it did not promote anything.
        let accepted = induction::load_validated(&artifact).expect("accepted artifact parses");
        assert_eq!(accepted.status.as_str(), "draft");
        assert!(
            accepted
                .classes
                .iter()
                .any(|c| c.label == "Feedstock Powder"
                    && c.parent.as_deref() == Some("NeutralEntity")),
            "{:?}",
            accepted.classes
        );
        let promoted_extension =
            ttl::promote_artifact(&artifact).expect("the existing gate accepts the artifact");
        assert_eq!(promoted_extension.status.as_str(), "accepted");

        // The decision survives in the ledger; the queue no longer holds it.
        assert!(store.pending_ontology_proposals(10).await.unwrap().len() == 1);
        let ledger = store
            .ontology_proposal_dispositions(&class_item.item_id)
            .await
            .unwrap();
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].outcome, "accepted");
        assert_eq!(ledger[0].dispositioner, "human:reviewer");
        assert_eq!(
            ledger[0].artifact_path.as_deref(),
            Some(artifact.display().to_string().as_str())
        );
        // The citation survived the decision.
        assert_eq!(
            store
                .ontology_proposal_sightings(&class_item.item_id)
                .await
                .unwrap()
                .len(),
            1
        );

        // REJECT the relation proposal through the production handler.
        proposals(
            ProposalCommands::Reject {
                item_ids: vec![relation_item.item_id.clone()],
                reason: "expressible with the existing vocabulary".to_string(),
                by: "human:reviewer".to_string(),
            },
            &root,
        )
        .await
        .expect("reject the relation proposal");
        assert!(
            store
                .pending_ontology_proposals(10)
                .await
                .unwrap()
                .is_empty()
        );

        // A rejected identity is never re-queued: the next ingest of the
        // same proposal is suppressed, loudly.
        assert_eq!(
            store
                .enqueue_ontology_proposal(&relation_item, &relation_citation, 3.0)
                .await
                .unwrap(),
            prism_provenance::OntologyProposalEnqueue::SupersededByDisposition
        );
    }
}
