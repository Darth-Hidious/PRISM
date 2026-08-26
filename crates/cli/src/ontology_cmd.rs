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
        /// Ontology to GROW, by registry id (`emmo`, `matkg`, a promoted
        /// project ontology) or by path to a TTL artifact. Repeatable, so one
        /// run can span several domains. Defaults to the project's active
        /// ontology; pass `--no-base` for a standalone run.
        #[arg(long = "base")]
        bases: Vec<String>,
        /// Induce standalone, inheriting nothing. The pre-growth behaviour.
        #[arg(long, conflicts_with = "bases")]
        no_base: bool,
        /// Also grow the ontology previously promoted for this `--domain`, so
        /// a second corpus compounds onto the first instead of restarting.
        #[arg(long = "continue")]
        continue_domain: bool,
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
    /// Bind free-text names onto the loaded ontologies and print the result.
    ///
    /// The projection surface: give it the names a source actually used and
    /// it answers with the class each one binds to, by which rung and at
    /// what score. Exactly the ladder the ingest path runs, exposed so a
    /// consumer can ask for the mapping instead of reimplementing one.
    /// Names that bind to nothing come back unbound — never guessed.
    Bind {
        /// File with one name per line. `-` reads standard input.
        names: PathBuf,
        /// Restrict the target to ONE loaded ontology, by registry id.
        ///
        /// Without this, names bind against the whole loaded union, which is
        /// what ingest wants — the best class from anything available. When
        /// the question is "project these onto THIS schema", the target has
        /// to be nameable, and this names it.
        #[arg(long = "onto")]
        onto: Option<String>,
        /// Override the semantic bind threshold.
        #[arg(long)]
        threshold: Option<f64>,
        /// Machine-readable result.
        #[arg(long)]
        json: bool,
    },
    /// Import a plain list of terms as an ontology a customer can bind
    /// against — one term per line, from anywhere.
    ///
    /// A target schema is USUALLY just a list of names: an enum, a column
    /// header row, a data dictionary, a controlled vocabulary. This turns
    /// any such list into a DRAFT artifact that goes through the ordinary
    /// promote gate. Nothing about any particular schema is known to PRISM;
    /// the list is the whole input, which is what makes bringing your own
    /// schema a file rather than a code change.
    ///
    /// Importing a vocabulary is OPTIONAL. Extraction never requires one:
    /// terms are read in the source's own words and bound afterwards, so
    /// with no vocabulary loaded everything is simply recorded unbound.
    Import {
        /// File with one term per line. `-` reads standard input. Blank
        /// lines and `#` comments are skipped.
        terms: PathBuf,
        /// Domain id for the imported vocabulary (its registry id).
        #[arg(long)]
        domain: String,
        /// Output artifact path. Default: ./ontology-<domain>.ttl
        #[arg(long)]
        output: Option<PathBuf>,
        /// Optional label for a single parent class every term is filed
        /// under, so the import is one taxonomy rather than N loose roots.
        #[arg(long)]
        parent: Option<String>,
    },
    /// Re-run the property resolution ladder over every term that is still
    /// unbound, against the ontologies loaded NOW.
    ///
    /// This is what makes a better ontology pay off retroactively: promote a
    /// proposal, or load a richer vocabulary, and the terms that had nothing
    /// to bind to bind now — WITHOUT re-reading a single paper. Bindings only
    /// ever strengthen (a lower rung never replaces a higher one), and facts
    /// are never touched: they keep their free-text spelling and gain
    /// identity through the term.
    Rebind {
        /// Report what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Override the semantic bind threshold for this run. Every score is
        /// recorded either way, so this can be tuned from measured data.
        #[arg(long)]
        threshold: Option<f64>,
        /// Machine-readable result.
        #[arg(long)]
        json: bool,
    },
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
    /// queue, newest first — so the proposals from the ingest you just ran are
    /// at the top rather than at position 342 of 346.
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
    /// Adjudicate the pending backlog with a model, then apply the verdicts.
    ///
    /// The extension loop is complete except for a reviewer: a reader that
    /// meets an unbindable term correctly refuses to guess and queues a class
    /// proposal with its citation — and then nothing accepts it, so the term
    /// never binds and every fact carrying it stays unclassified. Measured
    /// 2026-08-26: 80 of 80 unbound terms already had a queued proposal, and
    /// the queue held 346 items with zero dispositions.
    ///
    /// A human will not work through 346, and blanket acceptance is wrong —
    /// the queue genuinely mixes real classes (`yield strength`) with values
    /// (`1,311 mpa`) and procedures (`homogenized at 1,200 °c for 24 h`).
    /// That sorting is the judgement this command automates.
    ///
    /// SAFE BY CONSTRUCTION: accepting writes a DRAFT artifact and never
    /// promotes. The live ontology changes only through the separate
    /// deliberate `promote` gate, so the worst a wrong verdict can do is put
    /// a bad line in a draft file nobody has promoted.
    Judge {
        /// How many pending proposals to adjudicate.
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Decide and report WITHOUT accepting, rejecting, or writing.
        #[arg(long)]
        dry_run: bool,
        /// Domain id for a NEW draft artifact (as in `accept`).
        #[arg(long)]
        domain: Option<String>,
        /// Draft artifact to write or extend (as in `accept`).
        #[arg(long)]
        output: Option<PathBuf>,
        /// Override the judging model's endpoint.
        #[arg(long)]
        llm_url: Option<String>,
        /// Override the judging model.
        #[arg(long)]
        model: Option<String>,
        /// API key for the judging model.
        #[arg(long)]
        api_key: Option<String>,
        /// Machine-readable result.
        #[arg(long)]
        json: bool,
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
            bases,
            no_base,
            continue_domain,
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
                &bases,
                no_base,
                continue_domain,
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
        OntologyCommands::Bind {
            names,
            onto,
            threshold,
            json,
        } => bind_names(project_root, &names, onto.as_deref(), threshold, json).await,
        OntologyCommands::Import {
            terms,
            domain,
            output,
            parent,
        } => import_vocabulary(&terms, &domain, output.as_deref(), parent.as_deref()),
        OntologyCommands::Rebind {
            dry_run,
            threshold,
            json,
        } => rebind(project_root, dry_run, threshold, json).await,
    }
}

// ── Projection ─────────────────────────────────────────────────────────

/// Bind a list of free-text names onto the loaded ontologies.
async fn bind_names(
    project_root: &Path,
    names_path: &Path,
    onto: Option<&str>,
    threshold: Option<f64>,
    json: bool,
) -> Result<()> {
    let threshold =
        threshold.unwrap_or(prism_ingest::property_resolution::DEFAULT_SEMANTIC_BIND_THRESHOLD);
    if !(threshold.is_finite() && (0.0..=1.0).contains(&threshold)) {
        bail!("--threshold must be a similarity from 0 to 1, got {threshold}");
    }
    let raw = if names_path == Path::new("-") {
        use std::io::Read;
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        buffer
    } else {
        std::fs::read_to_string(names_path)
            .with_context(|| format!("reading names from {}", names_path.display()))?
    };
    let terms: Vec<prism_ingest::property_resolution::PropertyTerm> = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| prism_ingest::property_resolution::PropertyTerm {
            term: line.to_string(),
            // A projection request is not a sighting in a document, so it
            // queues no proposal and cites nothing.
            citation: None,
        })
        .collect();
    if terms.is_empty() {
        bail!("no names to bind in {}", names_path.display());
    }

    let config = prism_core::config::NodeConfig::load(Some(project_root));
    prism_ingest::ontologies::active_from_project(Some(&config.ontology.id), project_root)?;
    let ontologies = match onto {
        Some(id) => {
            // Register it from the project catalog first: a target schema is
            // usually a promoted project artifact, not a builtin.
            let target = prism_ingest::ontologies::active_from_project(Some(id), project_root)?;
            prism_ingest::ontologies::OntologySet::single(target)
        }
        None => prism_ingest::ontologies::loaded(Some(&config.ontology.id))?,
    };
    let tenant = prism_ingest::ontologies::storage_tenant(
        prism_provenance::LOCAL_TENANT,
        ontologies.primary().id(),
    );
    let store = prism_provenance::ProvenanceStore::open(&proposal_store_path()?).await?;
    let backend = tokio::task::spawn_blocking(prism_embed::from_config)
        .await
        .ok()
        .flatten();
    if backend.is_none() {
        eprintln!(
            "  WARNING: no embedding backend — rungs 1 and 2 apply, the semantic rung cannot run"
        );
    }
    let bindings = prism_ingest::property_resolution::resolve_property_terms(
        &store,
        &ontologies,
        backend.as_deref(),
        &tenant,
        "prism://bind",
        &terms,
        threshold,
    )
    .await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&prism_ingest::property_resolution::binding_report(
                &bindings
            ))?
        );
    } else {
        for binding in &bindings {
            match binding.class_iri.as_deref() {
                Some(iri) => println!(
                    "  {:?} -> {iri}  (rung {}{})",
                    binding.term,
                    binding.rung.as_str(),
                    binding
                        .score
                        .map_or_else(String::new, |s| format!(", score {s:.3}")),
                ),
                None => println!("  {:?} -> UNBOUND", binding.term),
            }
        }
    }
    Ok(())
}

// ── Vocabulary import ──────────────────────────────────────────────────

/// Turn a plain list of terms into a DRAFT ontology artifact.
///
/// This is the "bring your own schema" seam. A target vocabulary — someone
/// else's enum, a data dictionary, a column header row — is a list of names,
/// and a list of names is an ontology with one class per name. Writing it as
/// a normal artifact means every existing mechanism applies unchanged: the
/// promote gate, the loaded union, and the resolution ladder that binds
/// free-text property names onto whatever is loaded. No schema is known to
/// PRISM and none is compiled in.
fn import_vocabulary(
    terms_path: &Path,
    domain: &str,
    output: Option<&Path>,
    parent: Option<&str>,
) -> Result<()> {
    let raw = if terms_path == Path::new("-") {
        use std::io::Read;
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .context("reading terms from standard input")?;
        buffer
    } else {
        std::fs::read_to_string(terms_path)
            .with_context(|| format!("reading terms from {}", terms_path.display()))?
    };

    let mut seen = std::collections::HashSet::new();
    let mut terms: Vec<String> = Vec::new();
    for line in raw.lines() {
        let term = line.trim();
        if term.is_empty() || term.starts_with('#') {
            continue;
        }
        if seen.insert(term.to_lowercase()) {
            terms.push(term.to_string());
        }
    }
    if terms.is_empty() {
        bail!(
            "no terms found in {} — a vocabulary import needs at least one \
             non-empty, non-comment line",
            terms_path.display()
        );
    }

    let mut classes: Vec<induction::InducedClass> = Vec::new();
    if let Some(parent_label) = parent {
        classes.push(induction::InducedClass {
            label: parent_label.to_string(),
            definition: String::new(),
            parent: None,
            aligned_iri: None,
            declared_by_reference: true,
            sign_domain: None,
        });
    }
    for term in &terms {
        classes.push(induction::InducedClass {
            label: term.clone(),
            definition: String::new(),
            parent: parent.map(str::to_string),
            aligned_iri: None,
            declared_by_reference: false,
            sign_domain: None,
        });
    }

    // The corpus hash is over the TERMS as imported: re-importing the same
    // list claims the same version, and a changed list does not.
    let digest = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        for term in &terms {
            hasher.update(term.as_bytes());
            hasher.update(b"\n");
        }
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write;
            let _ = write!(hex, "{byte:02x}");
        }
        format!("sha256:{hex}")
    };

    let ontology = induction::InducedOntology {
        domain: domain.to_string(),
        status: induction::OntologyStatus::Draft,
        classes,
        relations: Vec::new(),
        provenance: induction::InductionProvenance {
            // No model read anything: this vocabulary was DECLARED, not
            // induced, and the artifact must not imply otherwise.
            model: "none (declared vocabulary import)".to_string(),
            prompt_version: "0".to_string(),
            corpus_hash: digest,
            documents_total: 0,
            documents_failed: 0,
            windows_read: 0,
            windows_attempted: 0,
            malformed_items: 0,
            ..Default::default()
        },
    };

    let path = output.map_or_else(
        || PathBuf::from(format!("./ontology-{domain}.ttl")),
        Path::to_path_buf,
    );
    ttl::write_artifact(&path, &ontology)?;
    println!(
        "IMPORTED: {} — {} term(s) as DRAFT ontology '{}'. Promote it with \
         `prism ontology promote {}`.",
        path.display(),
        terms.len(),
        domain,
        path.display(),
    );
    Ok(())
}

// ── Re-resolution ──────────────────────────────────────────────────────

/// Re-run the ladder over the unbound backlog against the ontologies loaded
/// now.
///
/// The ladder records EVERY term it sees, bound or not, with the score and
/// threshold in force at the time. That backlog is the work list here: a
/// term that found nothing when the only vocabulary was a 50-class EMMO
/// binds the moment a quantity-bearing ontology is promoted. Nothing is
/// re-extracted and no document is re-read — the join is the term itself.
/// The class population a re-resolution actually searches.
///
/// [`Ontology::classes`] is the EXTRACTION-facing slice — only declarations
/// carrying an extraction label — while the resolver walks
/// [`Ontology::ontology_classes`], the full navigable declaration. That is not
/// incidental: `property_resolution::label_candidates` says so in its own doc
/// comment, "the NAVIGABLE declaration is consulted (not just the smaller
/// extraction-facing slice) because binding is post-hoc identification".
///
/// Counting the extraction slice here told the operator a re-resolution would
/// search **15** classes when the real run searches **61** (measured on the
/// bundled EMMO + MatKG set, 2026-08-26, recorded as F57). A preview that
/// under-reports its own run by 4x is a lying surface, and a dry run exists
/// precisely to be believed — it is the one output an operator uses to decide
/// whether to run the thing for real.
fn resolvable_class_population(ontologies: &prism_ingest::ontologies::OntologySet) -> usize {
    ontologies
        .all()
        .iter()
        .map(|o| o.ontology_classes().len())
        .sum()
}

async fn rebind(
    project_root: &Path,
    dry_run: bool,
    threshold: Option<f64>,
    json: bool,
) -> Result<()> {
    let threshold =
        threshold.unwrap_or(prism_ingest::property_resolution::DEFAULT_SEMANTIC_BIND_THRESHOLD);
    if !(threshold.is_finite() && (0.0..=1.0).contains(&threshold)) {
        bail!("--threshold must be a similarity from 0 to 1, got {threshold}");
    }

    let config = prism_core::config::NodeConfig::load(Some(project_root));
    // Register the project's catalog artifact BEFORE asking for the loaded
    // set: `loaded` reads the process-wide registry, which knows nothing of
    // `.prism/ontologies/` until this runs. Without it the command fails on
    // exactly the ontology it exists to apply.
    prism_ingest::ontologies::active_from_project(Some(&config.ontology.id), project_root)?;
    let ontologies = prism_ingest::ontologies::loaded(Some(&config.ontology.id))?;
    let tenant = prism_ingest::ontologies::storage_tenant(
        prism_provenance::LOCAL_TENANT,
        ontologies.primary().id(),
    );

    let store = prism_provenance::ProvenanceStore::open(&proposal_store_path()?).await?;
    let unbound = store.unbound_term_bindings(&tenant).await?;
    if unbound.is_empty() {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "tenant": tenant,
                    "unbound_before": 0,
                    "bound_now": 0,
                    "still_unbound": 0,
                })
            );
        } else {
            println!(
                "no unbound terms for tenant {tenant:?} — nothing to re-resolve (ingest a \
                 paper, or the backlog is already fully bound)"
            );
        }
        return Ok(());
    }

    // A dry run must not write, and the ladder writes — so it is answered by
    // reporting the backlog rather than by running a ladder whose effects are
    // then discarded. Saying "N terms would be re-tried against M classes" is
    // honest; simulating a bind and throwing it away would not be.
    if dry_run {
        let classes = resolvable_class_population(&ontologies);
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "dry_run": true,
                    "tenant": tenant,
                    "unbound_before": unbound.len(),
                    "ontologies": ontologies.all().iter().map(|o| o.id()).collect::<Vec<_>>(),
                    "classes_available": classes,
                })
            );
        } else {
            println!(
                "{} unbound term(s) for tenant {tenant:?} would be re-tried against {} class(es) \
                 from {} loaded ontolog(ies) at threshold {threshold}",
                unbound.len(),
                classes,
                ontologies.all().len(),
            );
            for binding in unbound.iter().take(20) {
                println!(
                    "  {:?} (best score so far: {})",
                    binding.verbatim,
                    binding
                        .score
                        .map_or_else(|| "none".to_string(), |s| format!("{s:.3}"))
                );
            }
            if unbound.len() > 20 {
                println!("  … and {} more", unbound.len() - 20);
            }
        }
        return Ok(());
    }

    let terms: Vec<prism_ingest::property_resolution::PropertyTerm> = unbound
        .iter()
        .map(|binding| prism_ingest::property_resolution::PropertyTerm {
            term: binding.verbatim.clone(),
            // The original citation already backs the queued proposal; a
            // re-resolution adds no new sighting of its own.
            citation: None,
        })
        .collect();

    let backend = tokio::task::spawn_blocking(prism_embed::from_config)
        .await
        .ok()
        .flatten();
    if backend.is_none() {
        eprintln!(
            "  WARNING: no embedding backend configured — rungs 1 and 2 still apply, but the \
             semantic rung cannot run and terms needing it stay unbound"
        );
    }

    let bindings = prism_ingest::property_resolution::resolve_property_terms(
        &store,
        &ontologies,
        backend.as_deref(),
        &tenant,
        // Re-resolution is not a document reading; it is named as itself so
        // provenance never claims a paper was consulted when none was.
        "prism://rebind",
        &terms,
        threshold,
    )
    .await?;

    let bound_now = bindings.iter().filter(|b| b.class_iri.is_some()).count();
    let stamped: u64 = bindings.iter().map(|b| b.entities_stamped).sum();
    if json {
        let mut report = prism_ingest::property_resolution::binding_report(&bindings);
        if let Some(object) = report.as_object_mut() {
            object.insert("tenant".into(), serde_json::json!(tenant));
            object.insert("unbound_before".into(), serde_json::json!(unbound.len()));
            object.insert("bound_now".into(), serde_json::json!(bound_now));
            object.insert(
                "still_unbound".into(),
                serde_json::json!(bindings.len().saturating_sub(bound_now)),
            );
        }
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "re-resolved {} unbound term(s) for tenant {tenant:?} against {} loaded ontolog(ies) \
             at threshold {threshold}:",
            unbound.len(),
            ontologies.all().len(),
        );
        println!(
            "  BOUND NOW: {bound_now}   still unbound: {}   entities stamped: {stamped}",
            bindings.len().saturating_sub(bound_now),
        );
        for binding in &bindings {
            let Some(iri) = binding.class_iri.as_deref() else {
                continue;
            };
            println!(
                "  {:?} → {iri} (rung {}{})",
                binding.term,
                binding.rung.as_str(),
                binding
                    .score
                    .map_or_else(String::new, |s| format!(", score {s:.3}")),
            );
        }
        if bound_now == 0 {
            println!(
                "  nothing bound — the loaded ontologies declare no class these terms match. \
                 Promote a richer ontology and run this again; no paper is re-read."
            );
        }
    }
    Ok(())
}

// ── Ontology extension proposal governance ─────────────────────────────

/// Where the governance queue lives — the same store every ingest path
/// writes (`~/.prism/provenance.db`).
fn proposal_store_path() -> Result<PathBuf> {
    // Honours $PRISM_PROVENANCE_DB — pointing the ontology tools at a chosen
    // corpus used to open the DEFAULT store and collide with the node's lock.
    Ok(prism_provenance::store_path())
}

async fn proposals(command: ProposalCommands, project_root: &Path) -> Result<()> {
    let store = prism_provenance::ProvenanceStore::open(&proposal_store_path()?).await?;
    match command {
        ProposalCommands::List { limit, json } => {
            let pending = store.pending_ontology_proposals(limit as i64).await?;
            let total = store.pending_ontology_proposal_count().await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&pending_proposals_json(pending, limit, total))?
                );
            } else {
                if pending.is_empty() {
                    println!(
                        "no pending ontology proposals (queue empty — ingest a paper to grow one)"
                    );
                    return Ok(());
                }
                println!("pending ontology proposals (newest first):");
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
        ProposalCommands::Judge {
            limit,
            dry_run,
            domain,
            output,
            llm_url,
            model,
            api_key,
            json,
        } => {
            let pending = store.pending_ontology_proposals(limit as i64).await?;
            let total = store.pending_ontology_proposal_count().await?;
            if pending.is_empty() {
                println!("no pending proposals to judge (queue empty)");
                return Ok(());
            }

            let llm_config = crate::build_llm_config(
                project_root,
                llm_url.as_deref(),
                model.as_deref(),
                api_key.as_deref(),
            )?;
            let client = prism_ingest::llm::LlmClient::new(llm_config);
            let judge_id = format!("agent:{}", client.config().model);

            // Batched, because one call per proposal over a 346-item backlog
            // is 346 round trips for a decision the model can make in groups.
            const BATCH: usize = 25;
            let mut verdicts: Vec<JudgedProposal> = Vec::new();
            let mut out_of_range: Vec<usize> = Vec::new();
            for (n, chunk) in pending.chunks(BATCH).enumerate() {
                let listing = chunk
                    .iter()
                    .enumerate()
                    .map(|(i, (item, sightings))| {
                        format!(
                            "{}. label: {:?}\n   kind: {}\n   citations: {}",
                            i + 1,
                            item.label,
                            item.kind,
                            sightings
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                println!(
                    "judging batch {} ({} item(s)) with {} …",
                    n + 1,
                    chunk.len(),
                    client.config().model
                );
                let reply = client
                    .chat(JUDGE_SYSTEM, &listing)
                    .await
                    .context("the judging model call failed")?;
                for raw in parse_verdicts(&reply)? {
                    // Resolve against THIS batch. An out-of-range index is a
                    // dropped verdict, never a verdict applied to the wrong
                    // proposal — reject is final, so a misapplied one is
                    // unrecoverable.
                    match chunk.get(raw.index.wrapping_sub(1)) {
                        Some((item, _)) if raw.index >= 1 => verdicts.push(JudgedProposal {
                            item_id: item.item_id.clone(),
                            verdict: raw.verdict,
                            reason: raw.reason,
                        }),
                        _ => out_of_range.push(raw.index),
                    }
                }
            }

            let (mut accept_ids, mut reject, mut escalate) = (Vec::new(), Vec::new(), Vec::new());
            for v in &verdicts {
                match v.verdict.trim().to_lowercase().as_str() {
                    "accept" => accept_ids.push(v.item_id.clone()),
                    "reject" => reject.push(v.clone()),
                    // Anything the model did not say clearly is an escalation,
                    // never an acceptance.
                    _ => escalate.push(v.clone()),
                }
            }

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "judged": verdicts.len(),
                        "pending_total": total,
                        "accept": accept_ids.len(),
                        "reject": reject.len(),
                        "escalate": escalate.len(),
                        "out_of_range_indices": out_of_range,
                        "dry_run": dry_run,
                        "by": judge_id,
                    }))?
                );
            } else {
                println!(
                    "judged {} of {total} pending → accept {}, reject {}, escalate {}{}",
                    verdicts.len(),
                    accept_ids.len(),
                    reject.len(),
                    escalate.len(),
                    if out_of_range.is_empty() {
                        String::new()
                    } else {
                        format!(", {} out-of-range verdict(s) DROPPED", out_of_range.len())
                    }
                );
            }

            if dry_run {
                for v in escalate.iter().chain(reject.iter()).take(12) {
                    println!("  [{}] {} — {}", v.verdict, v.item_id, v.reason);
                }
                println!("dry run: nothing was accepted, rejected or written");
                return Ok(());
            }

            let decided_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);

            if !accept_ids.is_empty() {
                // Accepting is ALL-OR-NOTHING per call: it refuses the whole
                // batch when any proposal references a parent the active
                // ontology no longer declares. Measured 2026-08-26 on the real
                // backlog — 69 accepted, ZERO written, because 6 referenced
                // `matkg` parents that are not loaded. Refusing a dangling
                // reference is right; losing the other 63 to it is not. So try
                // the batch, and on refusal fall back to one at a time and
                // report exactly which ones could not land.
                let artifact = match accept_proposals(
                    &store,
                    &accept_ids,
                    domain.as_deref(),
                    output.clone(),
                    project_root,
                )
                .await
                {
                    Ok(artifact) => artifact,
                    Err(batch_err) => {
                        println!(
                            "batch accept refused ({batch_err}); retrying one at a time so a \
                             single unresolvable parent does not block the rest"
                        );
                        let mut landed = Vec::new();
                        let mut artifact = None;
                        for id in &accept_ids {
                            match accept_proposals(
                                &store,
                                std::slice::from_ref(id),
                                domain.as_deref(),
                                output.clone(),
                                project_root,
                            )
                            .await
                            {
                                Ok(path) => {
                                    artifact = Some(path);
                                    landed.push(id.clone());
                                }
                                Err(e) => println!("  NOT accepted: {id} — {e}"),
                            }
                        }
                        accept_ids = landed;
                        match artifact {
                            Some(path) => path,
                            None => {
                                println!("no proposal could be accepted into a draft artifact");
                                return Ok(());
                            }
                        }
                    }
                };
                for item_id in &accept_ids {
                    let reason = verdicts
                        .iter()
                        .find(|v| &v.item_id == item_id)
                        .map(|v| v.reason.clone())
                        .unwrap_or_else(|| "judged a class".to_string());
                    record_disposition(
                        &store,
                        item_id,
                        "accepted",
                        Some(artifact.display().to_string()),
                        reason,
                        &judge_id,
                        decided_at,
                    )
                    .await?;
                }
                println!(
                    "ACCEPTED {} → DRAFT {}\n  next: review it, then `prism ontology promote {}` — \
                     judging does NOT promote",
                    accept_ids.len(),
                    artifact.display(),
                    artifact.display()
                );
            }
            for v in &reject {
                record_disposition(
                    &store,
                    &v.item_id,
                    "rejected",
                    None,
                    v.reason.clone(),
                    &judge_id,
                    decided_at,
                )
                .await?;
            }
            if !reject.is_empty() {
                println!("REJECTED {} (final for those identities)", reject.len());
            }
            if !escalate.is_empty() {
                println!(
                    "LEFT PENDING {} for a human — the judge was not confident",
                    escalate.len()
                );
            }
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
/// What the judge decided about one proposal.
#[derive(Debug, Clone, serde::Deserialize)]
struct ProposalVerdict {
    /// 1-based position in the batch as presented — NOT the item id.
    ///
    /// Item ids are compound and enormous
    /// (`class|'LaserPowderBedFusion'|parents=[https://…#SynthesisMethod, …]`),
    /// full of quotes, commas, brackets and URLs. Measured 2026-08-26 on the
    /// real backlog: asking the model to echo them back inside JSON produced
    /// **272 unrecognised ids out of 346** — transcription noise, not
    /// disagreement. An integer cannot be mistranscribed into another valid
    /// item, so the mapping happens locally where it cannot go wrong.
    index: usize,
    /// `accept` | `reject` | `escalate`.
    verdict: String,
    reason: String,
}

/// One adjudicated proposal, after the index has been resolved locally.
#[derive(Debug, Clone)]
struct JudgedProposal {
    item_id: String,
    verdict: String,
    reason: String,
}

/// The rubric.
///
/// The ordering of the three rules is deliberate and so is the tie-break: a
/// wrong ACCEPT pollutes every fact that later binds to the bad class and is
/// expensive to undo, while an ESCALATE costs a curator ten seconds. So the
/// bias runs toward escalation, never toward acceptance — the same principle
/// that makes an honestly-red fact better than a wrongly-green one.
const JUDGE_SYSTEM: &str = "\
You curate a materials-science ontology. Each item was proposed by a reader \
that met a term it could not bind to anything existing. Every item carries a \
`kind`, and the kind decides which question you are answering.

kind = class  -> does the label name a KIND of thing or of measurable quantity, \
something specific instances or measurements could belong to?
  ACCEPT: yield strength - fracture toughness - Young's modulus - keyhole pore \
- laser powder bed fusion - solidus temperature
  REJECT: a specific VALUE ('1,311 MPa', '3315 K'); a specific configuration or \
dataset ('two BLSTM layers of 320 units', 'single-channel WSJ data with four \
additive noises'); a procedure instance ('homogenized at 1,200 C for 24 h'); a \
section heading ('experimental realization and properties'); a verb phrase \
naming a property rather than the property itself ('has band gap' - the CLASS \
would be 'band gap').

kind = relation  -> does the label name a RELATIONSHIP that can hold between \
two things? A relation is SUPPOSED to be a verb or a `hasX` phrase. Judge it as \
a relation and never reject it merely for not being a class.
  ACCEPT: catalyzes - hasActiveSite - has toughening mechanism - hasSolidusTemperature
  REJECT: only if it names no relationship at all, or is a value, a sentence \
fragment, or one specific event rather than a repeatable link.

ESCALATE (either kind) when a careful curator would genuinely want to look.

TIE-BREAK: when unsure, ESCALATE. Never ACCEPT to be helpful, and never REJECT \
because an item is the other kind - check its `kind` field first. A wrong \
ACCEPT contaminates every fact that binds to it; a wrong REJECT is FINAL for \
that identity and silently loses a real concept; an ESCALATE costs ten seconds.

Each item is numbered. Reply with ONLY a JSON array, one object per item, \
no prose, using the NUMBER — never copy the id:
[{\"index\":1,\"verdict\":\"accept|reject|escalate\",\"reason\":\"<one short clause>\"}]";

fn parse_verdicts(reply: &str) -> Result<Vec<ProposalVerdict>> {
    let start = reply.find('[');
    let end = reply.rfind(']');
    let (start, end) = match (start, end) {
        (Some(s), Some(e)) if e > s => (s, e),
        _ => bail!(
            "the judging model did not return a JSON array; its reply began: {:?}",
            reply.chars().take(200).collect::<String>()
        ),
    };
    serde_json::from_str::<Vec<ProposalVerdict>>(&reply[start..=end])
        .context("the judging model's JSON array did not match the expected verdict shape")
}

fn pending_proposals_json(
    pending: Vec<(prism_provenance::OntologyProposalItem, i64)>,
    limit: usize,
    total: i64,
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
    // `total` and `truncated` are the point: a window with no denominator is
    // how a reader concluded an ingest had proposed NOTHING while its five new
    // classes sat just past the limit. Measured 2026-08-25.
    let truncated = (rows.len() as i64) < total;
    serde_json::json!({
        "pending": rows,
        "limit": limit,
        "total": total,
        "truncated": truncated,
        "note": if truncated {
            format!(
                "showing {} of {total} pending proposals, NEWEST FIRST — raise --limit to see older ones",
                rows.len()
            )
        } else {
            format!("showing all {total} pending proposals, newest first")
        },
    })
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
/// Resolve the ontologies this run grows.
///
/// Default is the project's ACTIVE ontology — growth is the normal case, not
/// an opt-in, because an induction that ignores the ontology the project
/// already governs itself with produces a tree nobody can use. `--no-base`
/// asks for the standalone behaviour explicitly.
///
/// Bases resolve by registry id first (`emmo`, `matkg`, a promoted project
/// ontology) and then as a path to a TTL artifact, so a customer can hand over
/// a file without registering anything.
fn resolve_seed(
    project_root: &Path,
    domain: &str,
    bases: &[String],
    no_base: bool,
    continue_domain: bool,
) -> Result<induction::seed::Seed> {
    if no_base {
        return Ok(induction::seed::Seed::default());
    }
    let mut resolved: Vec<std::sync::Arc<dyn prism_ingest::ontologies::Ontology>> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    let push = |ontology: std::sync::Arc<dyn prism_ingest::ontologies::Ontology>,
                resolved: &mut Vec<_>,
                seen: &mut Vec<String>,
                is_own_prior: bool|
     -> Result<()> {
        let id = ontology.id().to_string();
        // `InducedOntology.domain` becomes the registry id AND the artifact
        // filename, and registration refuses an id that is already taken. A run
        // that grew `emmo` into `--domain emmo` could never be promoted, so
        // refuse it here with the reason rather than at promote time.
        //
        // `--continue` is the ONE case where the ids matching is the point:
        // growing this domain's own promoted artifact into its next version
        // replaces that artifact, which is exactly how an ontology compounds
        // across corpora. Applying the guard there made `--continue`
        // categorically impossible.
        if id == domain && !is_own_prior {
            bail!(
                "--domain {domain} collides with the base ontology {id}: the result could \
                 never be promoted, because that id is already registered. Give the grown \
                 ontology its own domain id."
            );
        }
        if !seen.contains(&id) {
            seen.push(id);
            resolved.push(ontology);
        }
        Ok(())
    };

    if bases.is_empty() {
        let active = prism_ingest::ontologies::active_for_project_config(project_root)?;
        push(active, &mut resolved, &mut seen, false)?;
    } else {
        for base in bases {
            let ontology =
                match prism_ingest::ontologies::active_from_project(Some(base), project_root) {
                    Ok(found) => found,
                    Err(registry_error) => {
                        let path = Path::new(base);
                        if !path.is_file() {
                            return Err(registry_error);
                        }
                        induction::register::load_induced_seed_from_path(path)?
                    }
                };
            push(ontology, &mut resolved, &mut seen, false)?;
        }
    }

    if continue_domain {
        let previous =
            prism_ingest::ontologies::project_ontology_artifact_path(project_root, domain)?;
        if previous.is_file() {
            let prior = induction::register::load_induced_seed_from_path(&previous)?;
            push(prior, &mut resolved, &mut seen, true)?;
        } else {
            bail!(
                "--continue found no promoted ontology for domain {domain} at {} — run \
                 without --continue to start it, then `prism ontology promote`.",
                previous.display()
            );
        }
    }

    let seed = induction::seed::seed_from(&resolved)?;
    if !seed.is_empty() {
        println!(
            "Growing {} — {} class(es), {} relation(s) inherited",
            seen.join(" + "),
            seed.classes.len(),
            seed.relations.len()
        );
        // Whatever could not be carried across is SAID, not dropped quietly.
        // EMMO's object properties, for instance, declare no rdfs:domain or
        // rdfs:range, so they cannot enter an artifact whose validator requires
        // typed endpoints — and inventing endpoints would assert structure the
        // base never claimed.
        for note in &seed.notes {
            println!("  note: {note}");
        }
    }
    Ok(seed)
}

// One parameter per CLI flag: this is the argument-dispatch boundary for
// `prism ontology induce`, so its arity is the command's arity. Grouping the
// flags into a struct would add a type whose only purpose is to be destructured
// immediately, and would put the flag list one indirection away from the clap
// definition it has to stay in step with.
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
    bases: &[String],
    no_base: bool,
    continue_domain: bool,
) -> Result<()> {
    let mut config = InductionConfig::new(domain)?;
    config.seed = resolve_seed(project_root, domain, bases, no_base, continue_domain)?;
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
    /// F57(b): `rebind --dry-run` must count the population the real run
    /// searches, not the extraction-facing slice.
    ///
    /// `classes()` keeps only declarations carrying an extraction label;
    /// `ontology_classes()` is the full navigable declaration, and
    /// `property_resolution::label_candidates` walks the latter. Measured on
    /// the bundled set the two are **15** and **61** — so the old preview told
    /// an operator a re-resolution would search a quarter of what it does.
    ///
    /// The inequality is asserted as well as the equality: without it this
    /// test would still pass if the two populations ever coincided, and would
    /// then be guarding nothing.
    #[test]
    fn dry_run_counts_the_population_the_resolver_actually_searches() {
        let ontologies =
            prism_ingest::ontologies::loaded(None).expect("bundled ontologies must load");

        let navigable: usize = ontologies
            .all()
            .iter()
            .map(|o| o.ontology_classes().len())
            .sum();
        let extraction_slice: usize = ontologies.all().iter().map(|o| o.classes().len()).sum();

        assert_eq!(
            super::resolvable_class_population(&ontologies),
            navigable,
            "the dry run must report the navigable declaration the resolver walks",
        );
        assert!(
            navigable > extraction_slice,
            "expected the navigable declaration ({navigable}) to be strictly larger than the \
             extraction slice ({extraction_slice}); if they are equal this test proves nothing \
             and the F57 regression could return unseen",
        );
    }

    // ── the proposal judge ──

    #[test]
    fn a_plain_verdict_array_parses() {
        let v = parse_verdicts(
            r#"[{"index":1,"verdict":"accept","reason":"names a quantity"},
                {"index":2,"verdict":"reject","reason":"a value, not a kind"}]"#,
        )
        .expect("parses");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].verdict, "accept");
        assert_eq!(v[1].index, 2);
    }

    #[test]
    fn a_verdict_array_wrapped_in_prose_or_fences_still_parses() {
        for reply in [
            "Here are my verdicts:\n```json\n[{\"index\":1,\"verdict\":\"escalate\",\"reason\":\"unclear\"}]\n```\nHope that helps.",
            "[{\"index\":1,\"verdict\":\"escalate\",\"reason\":\"unclear\"}]",
        ] {
            let v = parse_verdicts(reply).expect("tolerates wrapping");
            assert_eq!(v.len(), 1);
            assert_eq!(v[0].verdict, "escalate");
        }
    }

    #[test]
    fn a_reply_with_no_array_is_an_error_not_an_empty_verdict_list() {
        // The failure that matters: returning Ok(vec![]) here would read
        // downstream as "nothing to decide", leave the whole backlog
        // untouched, and REPORT SUCCESS — a silent no-op wearing a green tick.
        for reply in [
            "I'm sorry, I can't help with that.",
            "",
            "The proposals look reasonable to me.",
        ] {
            let err = parse_verdicts(reply).expect_err("a non-array reply must fail loudly");
            assert!(
                format!("{err:#}").contains("did not return a JSON array"),
                "the error must name what went wrong: {err:#}"
            );
        }
    }

    #[test]
    fn a_malformed_verdict_object_is_an_error() {
        let err = parse_verdicts(r#"[{"index":1}]"#)
            .expect_err("a verdict without a verdict field is not a verdict");
        assert!(format!("{err:#}").contains("verdict shape"), "{err:#}");
    }

    #[test]
    fn the_rubric_asks_for_a_number_never_the_item_id() {
        // Measured 2026-08-26 on the real 346-item backlog: asking the model to
        // echo compound ids back
        // (`class|'LaserPowderBedFusion'|parents=[https://…, …]`) produced
        // **272 unrecognised ids** — transcription noise, not disagreement.
        // An integer cannot be mistranscribed into another VALID item.
        assert!(JUDGE_SYSTEM.contains("Each item is numbered"));
        assert!(JUDGE_SYSTEM.contains("never copy the id"));
        assert!(
            !JUDGE_SYSTEM.contains("item_id"),
            "the reply shape must not ask for an id"
        );
    }

    #[test]
    fn the_rubric_biases_toward_escalation_never_acceptance() {
        // The tie-break is the safety property of the whole command: a wrong
        // ACCEPT contaminates every fact that later binds to the bad class.
        assert!(JUDGE_SYSTEM.contains("when unsure, ESCALATE"));
        assert!(JUDGE_SYSTEM.contains("Never ACCEPT to be helpful"));
        // And it must teach the distinction the real backlog actually needs.
        assert!(JUDGE_SYSTEM.contains("1,311 MPa"), "reject a VALUE");
        assert!(JUDGE_SYSTEM.contains("yield strength"), "accept a KIND");
        // The dry run caught this: the queue holds 296 classes AND 50
        // relations, and a class-only rubric rejected `catalyzes` and
        // `hasActiveSite` as "not a class" — a FINAL verdict that silently
        // destroys a real concept. The rubric must branch on `kind`.
        assert!(
            JUDGE_SYSTEM.contains("kind = relation"),
            "relations judged as relations"
        );
        assert!(
            JUDGE_SYSTEM.contains("catalyzes"),
            "a verb IS a valid relation"
        );
        assert!(
            JUDGE_SYSTEM.contains("never REJECT because an item is the other kind"),
            "the cross-kind mistake must be named explicitly"
        );
    }

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
        let parent_iri = "https://prism.mirdyne.com/ontology/gov-active#NeutralEntity".to_string();
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
            pending_proposals_json(store.pending_ontology_proposals(10).await.unwrap(), 10, 2);
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
