//! `prism papers` — the fast literature retrieval engine's CLI surface.
//!
//! Every subcommand prints one JSON document to stdout so the harness, MCP
//! clients, and shell pipelines all consume the same shape. Errors are
//! values in that JSON where possible; a hard failure exits non-zero with
//! the message on stderr.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use prism_retrieval::{
    EngineConfig, Paper, RelevancePolicy, RetrievalEngine, SourceId, SweepPlan, sweep,
};
use serde_json::json;

#[derive(Debug, Subcommand)]
pub enum PapersCommands {
    /// Search all sources concurrently for one query.
    Search {
        /// The query, e.g. "high entropy alloy phase stability".
        #[arg(long)]
        query: String,
        /// Max results per source before dedup.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Comma-separated source list; defaults to every source.
        #[arg(long)]
        sources: Option<String>,
        /// Contact address for the polite pools (OpenAlex, Crossref).
        #[arg(long)]
        mailto: Option<String>,
        /// Bypass the disk cache for this call.
        #[arg(long)]
        no_cache: bool,
    },
    /// Page through sources with a resumable, checkpointed sweep.
    Sweep {
        #[arg(long)]
        query: String,
        /// Max pages to fetch per source.
        #[arg(long, default_value_t = 3)]
        max_pages: usize,
        /// Page size requested from each source.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        sources: Option<String>,
        /// Checkpoint file; defaults to a plan-derived path in the cache dir.
        #[arg(long)]
        state: Option<PathBuf>,
        #[arg(long)]
        mailto: Option<String>,
        #[arg(long)]
        no_cache: bool,
    },
    /// Fetch and parse one paper's full text (JATS preferred, PDF fallback).
    FullText {
        /// PMC id such as PMC1234567 (fetches JATS from the PMC OA service).
        #[arg(long)]
        pmc: Option<String>,
        /// Direct full-text URL (JATS XML or PDF).
        #[arg(long)]
        url: Option<String>,
        /// Force the format when the URL gives no hint.
        #[arg(long, value_parser = ["jats", "pdf"])]
        format: Option<String>,
    },
    /// Extract EMMO-typed claims from a paper's full text via the local LLM.
    /// With no LLM configured this returns zero claims and says so — it
    /// never invents any.
    Claims {
        #[arg(long)]
        pmc: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        format: Option<String>,
        /// Override LLM model.
        #[arg(long)]
        model: Option<String>,
        /// Override LLM base URL.
        #[arg(long)]
        llm_url: Option<String>,
        /// API key for authenticated LLM providers.
        #[arg(long, env = "LLM_API_KEY")]
        api_key: Option<String>,
        /// Extract from at most this many blocks (0 = all). Bounds local-LLM
        /// run time on long documents.
        #[arg(long, default_value_t = 0)]
        max_blocks: usize,
        /// Also write the extracted claims into the bundled Turso store, so
        /// they join the local knowledge graph instead of only being printed.
        #[arg(long)]
        store: bool,
    },
}

/// Parse `--sources` into REGISTRY id strings — what selection is typed by.
/// The [`SourceId`] enum is used only as the CLI's name catalogue: the CLI
/// can only select built-ins (it has no way to register a third-party
/// adapter), so an unknown name is a typo and fails here with the list.
fn parse_sources(list: &Option<String>) -> Result<Vec<String>> {
    let Some(raw) = list else {
        return Ok(prism_retrieval::all_sources()
            .iter()
            .map(|id| id.as_str().to_string())
            .collect());
    };
    let mut out = Vec::new();
    for name in raw.split(',') {
        if name.trim().is_empty() {
            continue;
        }
        match SourceId::from_name(name) {
            Some(id) => out.push(id.as_str().to_string()),
            None => bail!(
                "unknown source {name:?}; known sources: {}",
                prism_retrieval::all_sources()
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    if out.is_empty() {
        bail!("--sources parsed to an empty list");
    }
    Ok(out)
}

fn build_engine(sources: Vec<String>, mailto: &Option<String>, no_cache: bool) -> RetrievalEngine {
    let cfg = EngineConfig {
        sources,
        mailto: mailto.clone(),
        cache_dir: if no_cache {
            None
        } else {
            prism_retrieval::default_cache_dir()
        },
        ..EngineConfig::default()
    };
    RetrievalEngine::new(cfg)
}

/// Paper descriptor for full-text/claims: identified by PMC id or a direct
/// URL. No other identifiers are guessed.
fn paper_for_fulltext(
    pmc: &Option<String>,
    url: &Option<String>,
    format: &Option<String>,
) -> Result<Paper> {
    let forced = match format.as_deref() {
        Some("jats") => Some(prism_retrieval::FulltextFormat::Jats),
        Some("pdf") => Some(prism_retrieval::FulltextFormat::Pdf),
        _ => None,
    };
    match (pmc, url) {
        (Some(pmc), _) => {
            let pmc_id = if pmc.to_ascii_uppercase().starts_with("PMC") {
                pmc.clone()
            } else {
                format!("PMC{pmc}")
            };
            let mut external_ids = std::collections::BTreeMap::new();
            external_ids.insert("pmc".to_string(), pmc_id.clone());
            Ok(Paper {
                source: "pmc".to_string(),
                source_id: pmc_id.clone(),
                title: String::new(),
                authors: Vec::new(),
                year: None,
                published: None,
                doi: None,
                external_ids,
                abstract_text: None,
                url: format!("https://www.ncbi.nlm.nih.gov/pmc/articles/{pmc_id}/"),
                fulltext_url: None,
                fulltext_format: forced,
                journal: None,
            })
        }
        (None, Some(url)) => Ok(Paper {
            source: "url".to_string(),
            source_id: url.clone(),
            title: String::new(),
            authors: Vec::new(),
            year: None,
            published: None,
            doi: None,
            external_ids: std::collections::BTreeMap::new(),
            abstract_text: None,
            url: url.clone(),
            fulltext_url: Some(url.clone()),
            fulltext_format: forced,
            journal: None,
        }),
        (None, None) => bail!("pass --pmc or --url to identify the document"),
    }
}

pub async fn handle(cmd: PapersCommands, project_root: &std::path::Path) -> Result<()> {
    match cmd {
        PapersCommands::Search {
            query,
            limit,
            sources,
            mailto,
            no_cache,
        } => {
            let source_ids = parse_sources(&sources)?;
            let engine = build_engine(source_ids, &mailto, no_cache)
                .with_relevance_policy(RelevancePolicy::default());
            let outcome = engine.search(&query, limit).await;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
        }
        PapersCommands::Sweep {
            query,
            max_pages,
            limit,
            sources,
            state,
            mailto,
            no_cache,
        } => {
            let source_ids = parse_sources(&sources)?;
            let engine = build_engine(source_ids.clone(), &mailto, no_cache);
            let plan = SweepPlan {
                query,
                sources: source_ids,
                max_pages_per_source: max_pages,
                per_page_limit: limit,
            };
            let state_path = state.unwrap_or_else(|| {
                let dir = prism_retrieval::default_cache_dir()
                    .unwrap_or_else(|| PathBuf::from(".prism/retrieval/cache"));
                sweep::default_state_path(&dir, &plan)
            });
            let outcome = engine
                .run_sweep(&plan, &state_path)
                .await
                .with_context(|| format!("sweep failed; state preserved at {state_path:?}"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "state_path": state_path.display().to_string(),
                    "outcome": outcome,
                }))?
            );
        }
        PapersCommands::FullText { pmc, url, format } => {
            let paper = paper_for_fulltext(&pmc, &url, &format)?;
            let engine = build_engine(vec![SourceId::Arxiv.as_str().to_string()], &None, false);
            match engine.fetch_fulltext_for(&paper).await? {
                Some(fulltext) => println!("{}", serde_json::to_string_pretty(&fulltext)?),
                None => println!(
                    "{}",
                    json!({
                        "fulltext": null,
                        "status": "no_fulltext_available",
                        "document": paper.url,
                    })
                ),
            }
        }
        PapersCommands::Claims {
            pmc,
            url,
            format,
            model,
            llm_url,
            api_key,
            max_blocks,
            store,
        } => {
            let paper = paper_for_fulltext(&pmc, &url, &format)?;
            let engine = build_engine(vec![SourceId::Arxiv.as_str().to_string()], &None, false);
            let Some(fulltext) = engine.fetch_fulltext_for(&paper).await? else {
                println!(
                    "{}",
                    json!({
                        "claims": [],
                        "status": "no_fulltext_available",
                        "document": paper.url,
                    })
                );
                return Ok(());
            };

            let llm_cfg = crate::build_llm_config(
                project_root,
                llm_url.as_deref(),
                model.as_deref(),
                api_key.as_deref(),
            )?;
            let extractor_model = llm_cfg.model.clone();
            if llm_cfg.base_url.trim().is_empty() || llm_cfg.model.trim().is_empty() {
                println!(
                    "{}",
                    json!({
                        "claims": [],
                        "status": "extractor_not_configured",
                        "reason": "no LLM endpoint configured; set one with `prism use` or pass --llm-url/--model. Zero claims returned — none were invented.",
                        "document": fulltext.source_url,
                    })
                );
                return Ok(());
            }

            // Cheap connectivity probe BEFORE spending one LLM call per
            // block: an unreachable endpoint must fail in seconds, not after
            // dozens of 300s-timeout retries.
            if let Err(reason) = probe_endpoint(&llm_cfg.base_url) {
                println!(
                    "{}",
                    json!({
                        "claims": [],
                        "status": "extractor_unreachable",
                        "reason": format!("{reason} Zero claims returned — none were invented."),
                        "document": fulltext.source_url,
                    })
                );
                return Ok(());
            }

            let llm = prism_ingest::llm::LlmClient::new(llm_cfg);
            let title = paper.title.clone();
            let document_id = paper
                .doi
                .clone()
                .or_else(|| paper.external_ids.get("pmc").cloned())
                .unwrap_or_else(|| paper.source_id.clone());
            let document_url = paper.url.clone();
            let source = paper.source.clone();

            let mut claims = Vec::new();
            let mut rejected = Vec::new();
            let mut blocks_extracted = 0usize;
            // A block whose extraction could not be parsed yields zero claims,
            // which is indistinguishable from a block that genuinely contained
            // none. `TextExtraction` reports the reason precisely so that stops
            // being invisible — but this loop was discarding it with `.facts`,
            // leaving the literature path exactly as silent as before.
            let mut extraction_failures: Vec<serde_json::Value> = Vec::new();
            // Facts the extractor dropped ONE BY ONE (malformed shape, or a
            // unit no QUDT identifier could be resolved for). Distinct from
            // `extraction_failures` (a whole block yielding nothing) and
            // from `rejected` (claims refused at validation): these never
            // became claims at all, and only this list says why.
            let mut dropped_facts: Vec<serde_json::Value> = Vec::new();
            // Extract per located block so every claim inherits a locator a
            // human can follow back into the document.
            for block in &fulltext.blocks {
                use prism_retrieval::fulltext::BlockKind;
                if !matches!(
                    block.locator.kind,
                    BlockKind::Body | BlockKind::Table | BlockKind::Caption
                ) {
                    continue;
                }
                if max_blocks > 0 && blocks_extracted >= max_blocks {
                    break;
                }
                blocks_extracted += 1;
                let extraction =
                    prism_ingest::text_extract::extract_facts_from_text(&llm, &title, &block.text)
                        .await
                        .with_context(|| "LLM fact extraction failed")?;
                if let Some(reason) = &extraction.parse_error {
                    extraction_failures.push(json!({
                        "section": block.locator.section_path,
                        "reason": reason,
                    }));
                }
                for reason in &extraction.dropped_facts {
                    dropped_facts.push(json!({
                        "section": block.locator.section_path,
                        "reason": reason,
                    }));
                }
                for fact in extraction.facts {
                    // Containment: find the verbatim span of THIS block that
                    // supports the fact. Facts with no supporting span cannot
                    // become claims — stamping them would record provenance a
                    // document never gave (extractor prompt examples included).
                    let support = prism_retrieval::claims::supporting_quote_or_refusal(
                        &fact.subject,
                        &fact.object,
                        fact.value,
                        &block.text,
                    );
                    let quote = support.as_ref().ok().cloned();
                    // Clone the drop-record fields BEFORE `fact` moves into
                    // `claim_from_fact` and `claim` into `validate_and_stamp`:
                    // without them the rejected entries carry only a reason,
                    // and the over-refusal histogram cannot be built from
                    // production output at all.
                    let subject = fact.subject.clone();
                    let object = fact.object.clone();
                    let value = fact.value;
                    let claim = claim_from_fact(
                        fact,
                        &document_id,
                        &document_url,
                        &source,
                        &block.locator,
                        quote,
                    );
                    match prism_retrieval::claims::validate_and_stamp(claim, &block.text) {
                        Ok(stamped) => claims.push(stamped),
                        Err(reason) => {
                            // A MissingQuote drop is refined by `support`: a
                            // missing quote is the model's fault only when no
                            // span held the fact at all (NoSpan maps back to
                            // MissingQuote). When a span held it but a guard
                            // refused every occurrence of the value, record
                            // WHICH guard: that drop is the matcher's refusal,
                            // and the guard name is the only observable signal
                            // of over-refusal. MissingQuote implies `support`
                            // is Err — Ok support gave the claim a quote, and
                            // only a quote-less claim is refused as
                            // MissingQuote — so no Ok arm exists here.
                            let reason = match (reason, support) {
                                (
                                    prism_retrieval::claims::ClaimRejection::MissingQuote,
                                    Err(refusal),
                                ) => prism_retrieval::claims::ClaimRejection::from(refusal),
                                (reason, _) => reason,
                            };
                            rejected.push(json!({
                                "reason": reason,
                                "subject": subject,
                                "object": object,
                                "value": value,
                                "locator": block.locator,
                            }));
                        }
                    }
                }
            }
            // Persisting is opt-in. Until now `papers claims` printed EMMO
            // claims and dropped them: the retrieval half and the graph half
            // were both built and never joined, so PRISM could read a paper
            // without ever knowing what was in it.
            let stored = if store {
                Some({
                    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                    let db_path = std::path::PathBuf::from(home).join(".prism/provenance.db");
                    store_claims(&claims, &fulltext.source_url, &extractor_model, &db_path).await?
                })
            } else {
                None
            };

            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "claims": claims,
                    "rejected": rejected,
                    "status": "ok",
                    "document": fulltext.source_url,
                    "blocks_extracted": blocks_extracted,
                    "max_blocks": max_blocks,
                    "stored": stored,
                    // Non-empty means some blocks produced nothing because the
                    // model misbehaved, NOT because the paper was silent there.
                    "extraction_failures": extraction_failures,
                    // Facts dropped individually during extraction (bad shape
                    // or an unresolvable unit) — they never became claims,
                    // and a numeric value is never kept with its unit
                    // discarded. One entry per fact, with the reason.
                    "dropped_facts": dropped_facts,
                    // Every block is read WHOLE now (the extractor no longer
                    // truncates its input); the key stays for consumers of
                    // the old shape and is honestly always zero.
                    "truncated_bytes": 0,
                }))?
            );
        }
    }
    Ok(())
}

/// Write extracted literature claims into the bundled Turso store.
///
/// `ExtractedClaim` and `MaterialFact` are structurally the same fact in two
/// crates; the only real conversion is the unit, which is a plain `String` on
/// the retrieval side and a validated `QudtUnit` on the storage side.
///
/// A claim whose unit fails QUDT validation is REJECTED and counted, never
/// written with the unit quietly dropped: a measurement that loses its unit is
/// a wrong number, not a slightly poorer one.
///
/// Evidence class is re-capped through `evidence_for_result` on the way in.
/// `validate_and_stamp` already caps at literature, but this store call is a
/// separate entry point and must not depend on an upstream promise.
fn semantic_entities_for_claim_facts(
    facts: &[prism_provenance::LocalFact],
) -> Vec<prism_ingest::semantic_validation::SemanticEntityProposal> {
    use prism_ingest::semantic_validation::SemanticEntityProposal;

    // `write_fact_with_classification` stamps the assertion with the active
    // ontology artifact but deliberately writes the established legacy node
    // shapes; the claim payload proposes no endpoint class. Mirror those
    // exact labels and keep every class IRI absent. This makes typing
    // explicitly unavailable instead of inventing a model judgement from a
    // fact kind or confusing an ontology provenance stamp with node typing.
    let proposal = |name: &str, legacy_label: &str| SemanticEntityProposal {
        name: name.to_string(),
        entity_type: legacy_label.to_string(),
        storage_label: legacy_label.to_string(),
        class_iri: None,
    };

    let mut entities = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for fact in facts {
        let subject = proposal(&fact.subject, "Matter");
        let object_label = match fact.kind.as_deref() {
            Some("measurement") => "Property",
            Some("phase") => "Phase",
            Some("composition") => "Composition",
            Some("contains") => "Element",
            Some("processing") => "Manufacturing",
            Some("structure") => "CrystalStructure",
            Some("application") => "Application",
            _ => "Entity",
        };
        let object = proposal(&fact.object, object_label);

        for entity in [subject, object] {
            let identity = (
                entity.name.clone(),
                entity.entity_type.clone(),
                entity.storage_label.clone(),
                entity.class_iri.clone(),
            );
            if seen.insert(identity) {
                entities.push(entity);
            }
        }
    }
    entities
}

async fn store_claims(
    claims: &[prism_retrieval::claims::ExtractedClaim],
    document_url: &str,
    model: &str,
    db_path: &std::path::Path,
) -> Result<serde_json::Value> {
    use prism_provenance::{
        EvidenceSource, FactPayload as _, LocalProvenance, MaterialFact, MeasurementCondition,
        ProvenanceStore, QudtUnit, evidence_for_result,
    };

    if claims.is_empty() {
        return Ok(json!({
            "written": 0,
            "rejected": 0,
            "store": null,
            "semantic_validation": null,
        }));
    }

    let store = ProvenanceStore::open(db_path).await?;
    // Literature claims use the same built-in EMMO classification contract
    // as text ingest. Resolve it through the production registry so the
    // assertion records the exact version IRI and vendored artifact hash
    // (REQ-OWL-S1-CLASSIFICATION-PROVENANCE).
    let ontology = prism_ingest::ontologies::active(None)?;

    let now = chrono::Utc::now().to_rfc3339();
    let prov = LocalProvenance {
        activity_id: uuid::Uuid::new_v4().to_string(),
        agent_id: if model.is_empty() {
            "prism-papers".to_string()
        } else {
            model.to_string()
        },
        agent_kind: "SoftwareAgent".into(),
        source_entity_id: document_url.to_string(),
        source_kind: "Document".into(),
        tenant: "local".into(),
        started_at: now.clone(),
        ended_at: now,
        locality: "local".into(),
        // Local extraction reads the document itself — not a relay.
        origin_source_id: None,
    };
    let mut rejected: Vec<serde_json::Value> = Vec::new();
    let mut prepared = Vec::with_capacity(claims.len());

    for claim in claims {
        let unit = match claim.unit.as_deref() {
            Some(raw) => match QudtUnit::new(raw) {
                Ok(unit) => Some(unit),
                Err(e) => {
                    rejected.push(json!({
                        "subject": claim.subject,
                        "object": claim.object,
                        "reason": format!("unit {raw:?} is not a valid QUDT identifier: {e}"),
                    }));
                    continue;
                }
            },
            None => None,
        };

        // A `measurement` with no value is DROPPED by the store —
        // `write_fact` returns Ok(()) having written nothing (see the
        // `Some("measurement")` arm in prism-provenance: "a measurement
        // without a value fails schema validation and is dropped"). Counting
        // that as written reports facts that are not in the graph.
        //
        // `validate_and_stamp` does not catch it: it rejects a value with no
        // unit, not a measurement with no value. The kind and the value come
        // from the model independently, so nothing upstream ties them.
        if claim.kind.as_deref() == Some("measurement") && claim.value.is_none() {
            rejected.push(json!({
                "subject": claim.subject,
                "object": claim.object,
                "reason": "kind is `measurement` but no value was extracted; the store drops \
                           such a fact, so writing it would report a fact that is not there",
            }));
            continue;
        }

        let mut conditions = Vec::with_capacity(claim.conditions.len());
        let mut bad_condition = None;
        for condition in &claim.conditions {
            let cond_unit = match condition.unit.as_deref() {
                Some(raw) => match QudtUnit::new(raw) {
                    Ok(unit) => Some(unit),
                    Err(e) => {
                        bad_condition =
                            Some(format!("condition {:?} unit {raw:?}: {e}", condition.name));
                        break;
                    }
                },
                None => None,
            };
            conditions.push(MeasurementCondition {
                name: condition.name.clone(),
                value: match &condition.value {
                    prism_retrieval::claims::ConditionValue::Number(n) => {
                        prism_provenance::ConditionValue::Number(*n)
                    }
                    prism_retrieval::claims::ConditionValue::Text(t) => {
                        prism_provenance::ConditionValue::Text(t.clone())
                    }
                },
                unit: cond_unit,
            });
        }
        if let Some(reason) = bad_condition {
            rejected.push(json!({
                "subject": claim.subject,
                "object": claim.object,
                "reason": reason,
            }));
            continue;
        }

        let fact = MaterialFact {
            subject: claim.subject.clone(),
            predicate: claim.predicate.clone(),
            object: claim.object.clone(),
            value: claim.value,
            unit,
            conditions,
            confidence: claim.confidence,
            evidence_class: evidence_for_result(
                EvidenceSource::LiteratureExtraction,
                [serde_json::from_value(json!(claim.evidence_class)).unwrap_or_default()],
            ),
            kind: claim.kind.clone(),
        };

        prepared.push((claim, fact));
    }

    // The model proposes; geometry measures. Every structurally accepted
    // claim is prepared before the first activity/fact write. Cost per paper:
    // one embedding-model batch for all distinct endpoints, plus the
    // validator's batched graph scans -- never one model/SQL round trip per
    // claim. The advisory report is never consulted to merge, drop, rewrite,
    // or block a claim.
    let local_facts: Vec<_> = prepared
        .iter()
        .map(|(_, fact)| fact.to_local_fact())
        .collect();
    let semantic_entities = semantic_entities_for_claim_facts(&local_facts);
    let semantic_policy = prism_ingest::semantic_validation::SemanticValidationPolicy::default();
    let semantic = prism_ingest::semantic_validation::validate_write_best_effort(
        &store,
        &semantic_entities,
        &local_facts,
        &prov.tenant,
        &semantic_policy,
    )
    .await;

    store.record_activity(&prov).await?;

    let mut written = 0usize;
    for (claim, fact) in &prepared {
        match store
            .write_fact_with_classification(
                fact,
                &prov,
                prism_provenance::OntologyClassification {
                    version_iri: ontology.version_iri().as_str(),
                    artifact_sha256: ontology.artifact_sha256(),
                },
            )
            .await
        {
            Ok(()) => written += 1,
            Err(e) => rejected.push(json!({
                "subject": claim.subject,
                "object": claim.object,
                "reason": format!("store write failed: {e}"),
            })),
        }
    }

    // Reuse the validator's one model batch for semantic search indexing.
    // This remains best-effort and happens only after the unchanged writes,
    // so vector storage can neither block nor alter graph persistence.
    if let Some(embedding_model) = semantic.embedding_model()
        && let Err(error) = store
            .store_precomputed_name_embeddings(
                semantic.embedding_names(),
                semantic.embedding_vectors(),
                &prov.tenant,
                embedding_model,
            )
            .await
    {
        tracing::warn!(%error, "papers claim embeddings were not stored");
    }

    Ok(json!({
        "written": written,
        "rejected": rejected.len(),
        "rejections": rejected,
        "store": db_path.display().to_string(),
        "tenant": "local",
        "semantic_validation": semantic.report,
    }))
}

/// TCP-probe an LLM base URL with a hard 3-second budget.
fn probe_endpoint(base_url: &str) -> Result<(), String> {
    use std::net::ToSocketAddrs;

    // Hard offline, checked FIRST — before `to_socket_addrs`, not just before
    // the connect. Resolution is itself a network call: a DNS query for an
    // agent-chosen host leaves the machine even if the TCP handshake never
    // happens.
    //
    // This probe is agent-reachable with no human gate. `papers` is
    // `PermissionMode::ReadOnly, requires_approval: false` and its
    // `FlagPolicy::Only` list includes `--llm-url`
    // (agent/src/command_tools.rs), and `execute_cli_command` spawns the CLI
    // with no `env_clear`, so a `PRISM_OFFLINE=1` parent is inherited and was
    // then ignored right here. A model could name the host.
    //
    // `check_url` rather than `enabled()`: a local llama.cpp endpoint is the
    // normal case and must stay probeable offline.
    prism_runtime::offline::check_url(base_url)?;
    let without_scheme = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url);
    let host_port = without_scheme.split('/').next().unwrap_or("");
    let host_port = match host_port.rsplit_once(':') {
        Some((_host, p)) if p.parse::<u16>().is_ok() => host_port.to_string(),
        _ => format!(
            "{host_port}:{port}",
            port = if base_url.starts_with("https") {
                443
            } else {
                80
            }
        ),
    };
    if host_port.starts_with(':') {
        return Err(format!("cannot parse host from {base_url}"));
    }
    let addr = host_port
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {host_port}: {e}"))?
        .next()
        .ok_or_else(|| format!("cannot resolve {host_port}"))?;
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(3))
        .map(|_| ())
        .map_err(|e| format!("LLM endpoint {addr} unreachable: {e}."))
}

/// Convert one extracted `MaterialFact` into a provenance-carrying claim.
/// `quote` is the verbatim supporting span found in the cited block (see
/// `supporting_quote`); evidence is stamped by `validate_and_stamp`
/// (ceiling: research).
fn claim_from_fact(
    fact: prism_provenance::MaterialFact,
    document_id: &str,
    document_url: &str,
    source: &str,
    locator: &prism_retrieval::Locator,
    quote: Option<String>,
) -> prism_retrieval::claims::ExtractedClaim {
    use prism_provenance::FactPayload;
    use prism_retrieval::claims::{ConditionValue, MeasurementCondition};
    // Pull everything we need by reference BEFORE moving fields out.
    let unit = fact.unit.as_ref().map(|u| u.as_str().to_string());
    let conditions = fact
        .conditions()
        .iter()
        .map(|c| MeasurementCondition {
            name: c.name.clone(),
            value: match &c.value {
                prism_provenance::ConditionValue::Number(n) => ConditionValue::Number(*n),
                prism_provenance::ConditionValue::Text(t) => ConditionValue::Text(t.clone()),
            },
            unit: c.unit.as_ref().map(|u| u.as_str().to_string()),
        })
        .collect();
    let evidence_class = fact.evidence_class().as_str().to_string();
    prism_retrieval::claims::ExtractedClaim {
        subject: fact.subject,
        predicate: fact.predicate,
        object: fact.object,
        value: fact.value,
        unit,
        conditions,
        confidence: fact.confidence,
        kind: fact.kind,
        evidence_class,
        provenance: prism_retrieval::claims::ClaimProvenance {
            document_id: document_id.to_string(),
            document_url: document_url.to_string(),
            source: source.to_string(),
            locator: locator.clone(),
            quote,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CRATE's lock, not a private one.
    ///
    /// `boot_checks::ENV_LOCK` is already shared by `boot_checks.rs` and
    /// `main.rs`; this file declared a second `static LOCK` for the same
    /// process-global `PRISM_OFFLINE`. Two locks that do not exclude each
    /// other serialize nothing, and all three files compile into one test
    /// binary that cargo runs multi-threaded. Sixth occurrence of this shape —
    /// `d3fcdfa4` consolidated it in `crates/mesh` and missed that
    /// `crates/cli` had the same bug.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::boot_checks::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// Restores the var on drop, so a failed assertion cannot leave it set for
    /// the rest of the binary.
    struct OfflineGuard(Option<String>);
    impl Drop for OfflineGuard {
        fn drop(&mut self) {
            unsafe {
                match self.0.take() {
                    Some(v) => std::env::set_var("PRISM_OFFLINE", v),
                    None => std::env::remove_var("PRISM_OFFLINE"),
                }
            }
        }
    }

    /// `--sources ntrs` must select the NTRS adapter: the CLI's name
    /// catalogue (SourceId), the default selection, and the registry that
    /// actually serves fetches all have to agree, or the name parses while
    /// the engine reports an unknown source.
    #[test]
    fn ntrs_is_selectable_and_backed_by_a_registered_adapter() {
        let ids = parse_sources(&Some("ntrs".to_string())).expect("'ntrs' must parse");
        assert_eq!(ids, ["ntrs"]);
        let default = parse_sources(&None).expect("default set must parse");
        assert!(
            default.contains(&"ntrs".to_string()),
            "ntrs missing from the default selection: {default:?}"
        );
        assert!(
            prism_retrieval::SourceRegistry::builtin()
                .get("ntrs")
                .is_some(),
            "the catalogue names 'ntrs' but no adapter is registered under it"
        );
    }

    /// The probe must refuse a remote host BEFORE resolving it. `papers` is an
    /// agent tool with `requires_approval: false` whose flag allow-list
    /// includes `--llm-url`, and the spawned CLI inherits `PRISM_OFFLINE`
    /// (no `env_clear`), so a model could name the host and this was the one
    /// step that ignored the flag.
    #[test]
    fn probe_refuses_a_remote_endpoint_offline() {
        let _guard = env_lock();
        let _restore = OfflineGuard(std::env::var("PRISM_OFFLINE").ok());
        unsafe { std::env::set_var("PRISM_OFFLINE", "1") };

        let err = probe_endpoint("https://llm.example.invalid/v1")
            .expect_err("offline must refuse a remote endpoint");
        assert!(err.contains("offline mode"), "{err}");
        assert!(
            err.contains("llm.example.invalid"),
            "must name what it blocked: {err}"
        );
        // It must NOT have got as far as resolution — a DNS failure message
        // would mean the lookup already left the machine.
        assert!(
            !err.contains("cannot resolve"),
            "resolved before refusing: {err}"
        );
    }

    /// A local llama.cpp endpoint stays probeable offline — `check_url`, not a
    /// blanket refusal. Nothing listens on port 1, so reaching a CONNECT error
    /// rather than a policy one proves the guard let it through.
    #[test]
    fn probe_still_allows_loopback_offline() {
        let _guard = env_lock();
        let _restore = OfflineGuard(std::env::var("PRISM_OFFLINE").ok());
        unsafe { std::env::set_var("PRISM_OFFLINE", "1") };

        let err =
            probe_endpoint("http://127.0.0.1:1/v1").expect_err("nothing is listening on port 1");
        assert!(
            !err.contains("offline mode"),
            "loopback must not be refused by policy: {err}"
        );
    }

    /// Without this the two above would pass even if the guard refused
    /// unconditionally.
    #[test]
    fn probe_guard_is_inert_when_offline_is_unset() {
        let _guard = env_lock();
        let _restore = OfflineGuard(std::env::var("PRISM_OFFLINE").ok());
        unsafe { std::env::remove_var("PRISM_OFFLINE") };

        let err = probe_endpoint("http://127.0.0.1:1/v1").expect_err("nothing is listening");
        assert!(
            !err.contains("offline mode"),
            "guard fired with offline unset: {err}"
        );
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use prism_retrieval::claims::{
        ClaimProvenance, ConditionValue, ExtractedClaim, MeasurementCondition,
    };

    fn scratch_db() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("prism_papers_test_{}.db", uuid::Uuid::new_v4()))
    }

    fn cleanup(p: &std::path::Path) {
        for suffix in ["", "-wal", "-shm"] {
            let mut q = p.to_path_buf().into_os_string();
            q.push(suffix);
            let _ = std::fs::remove_file(q);
        }
    }

    fn claim(object: &str, unit: Option<&str>, cond_unit: Option<&str>) -> ExtractedClaim {
        ExtractedClaim {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_measurement".into(),
            object: object.into(),
            value: Some(1140.0),
            unit: unit.map(str::to_string),
            conditions: cond_unit
                .map(|u| {
                    vec![MeasurementCondition {
                        name: "temperature".into(),
                        value: ConditionValue::Number(298.15),
                        unit: Some(u.to_string()),
                    }]
                })
                .unwrap_or_default(),
            confidence: Some(0.9),
            kind: Some("measurement".into()),
            evidence_class: "research".into(),
            provenance: ClaimProvenance {
                document_id: "10.1000/xyz".into(),
                document_url: "https://example.org/paper".into(),
                source: "arxiv".into(),
                locator: prism_retrieval::Locator {
                    kind: prism_retrieval::fulltext::BlockKind::Body,
                    section_path: vec!["Results".into()],
                    label: None,
                    char_offset: 0,
                },
                quote: None,
            },
        }
    }

    #[test]
    fn claim_endpoints_mirror_legacy_write_labels_without_inventing_types() {
        let facts = vec![
            prism_provenance::LocalFact {
                subject: "Ti-6Al-4V".into(),
                predicate: "has_measurement".into(),
                object: "UTS".into(),
                value: Some(1140.0),
                unit: Some("QUDT:MegaPA".into()),
                confidence: Some(0.9),
                kind: Some("measurement".into()),
            },
            prism_provenance::LocalFact {
                subject: "Ti-6Al-4V".into(),
                predicate: "used_in".into(),
                object: "turbine blade".into(),
                value: None,
                unit: None,
                confidence: Some(0.8),
                kind: Some("application".into()),
            },
        ];

        let entities = semantic_entities_for_claim_facts(&facts);
        let subject = entities
            .iter()
            .find(|entity| entity.name == "Ti-6Al-4V")
            .expect("subject proposal");
        assert_eq!(subject.entity_type, "Matter");
        assert_eq!(subject.storage_label, "Matter");
        assert_eq!(subject.class_iri, None);

        let object = entities
            .iter()
            .find(|entity| entity.name == "UTS")
            .expect("object proposal");
        assert_eq!(object.entity_type, "Property");
        assert_eq!(object.storage_label, "Property");
        assert_eq!(object.class_iri, None);

        let legacy = entities
            .iter()
            .find(|entity| entity.name == "turbine blade")
            .expect("legacy object proposal");
        assert_eq!(legacy.entity_type, "Application");
        assert_eq!(legacy.storage_label, "Application");
        assert_eq!(
            legacy.class_iri, None,
            "the active EMMO subset declares no Application IRI"
        );
    }

    /// The gap this exists to close: extracted literature claims must land in
    /// the graph, not just be printed.
    #[tokio::test]
    async fn a_valid_claim_is_written_and_readable_back() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };
        let db = scratch_db();

        let out = store_claims(
            &[claim("UTS", Some("QUDT:MegaPA"), Some("QUDT:K"))],
            "https://example.org/paper",
            "test-model",
            &db,
        )
        .await
        .expect("store");

        assert_eq!(out["written"], 1, "claim was not written: {out}");
        assert_eq!(out["rejected"], 0);
        for check in ["near_duplicates", "typing", "triple_plausibility"] {
            assert_eq!(
                out["semantic_validation"][check]["status"], "unavailable",
                "an absent embedding backend cannot pose as applied: {out}"
            );
            assert_eq!(
                out["semantic_validation"][check]["passed"],
                serde_json::Value::Null,
                "an unavailable check cannot pose as passed: {out}"
            );
        }

        let store = prism_provenance::ProvenanceStore::open(&db).await.unwrap();
        let facts = store
            .recall_with_context("Ti-6Al-4V", "local", 10)
            .await
            .unwrap();
        assert_eq!(facts.len(), 1, "fact not readable back");
        assert_eq!(facts[0].object, "UTS");
        assert_eq!(
            facts[0].evidence_class,
            prism_provenance::EvidenceClass::Research,
            "literature must stay ORANGE/research",
        );
        assert_eq!(facts[0].source, "https://example.org/paper");
        cleanup(&db);
    }

    /// A measurement that loses its unit is a wrong number, not a slightly
    /// poorer one. An invalid QUDT unit must reject the claim, not write it
    /// unitless.
    #[tokio::test]
    async fn a_claim_with_an_invalid_unit_is_rejected_not_silently_unitless() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };
        let db = scratch_db();

        let out = store_claims(
            &[claim("UTS", Some("megapascals"), None)],
            "https://example.org/paper",
            "test-model",
            &db,
        )
        .await
        .expect("store");

        assert_eq!(out["written"], 0, "an unvalidated unit was written: {out}");
        assert_eq!(out["rejected"], 1);
        assert!(
            out["rejections"][0]["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("QUDT"),
            "rejection does not say why: {out}"
        );

        let store = prism_provenance::ProvenanceStore::open(&db).await.unwrap();
        let facts = store
            .recall_with_context("Ti-6Al-4V", "local", 10)
            .await
            .unwrap();
        assert!(facts.is_empty(), "rejected claim reached the store anyway");
        cleanup(&db);
    }

    /// A `measurement` with no value is dropped by the store while returning
    /// Ok(()), so counting it as written reports a fact that is not in the
    /// graph. `validate_and_stamp` does not catch this — it rejects a value
    /// with no unit, not a measurement with no value.
    #[tokio::test]
    async fn a_valueless_measurement_is_rejected_not_counted_as_written() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };
        let db = scratch_db();

        let mut c = claim("UTS", Some("QUDT:MegaPA"), None);
        c.value = None; // kind stays "measurement"

        let out = store_claims(&[c], "https://example.org/paper", "m", &db)
            .await
            .expect("store");

        assert_eq!(
            out["written"], 0,
            "counted a fact the store discards: {out}"
        );
        assert_eq!(out["rejected"], 1);

        let store = prism_provenance::ProvenanceStore::open(&db).await.unwrap();
        let facts = store
            .recall_with_context("Ti-6Al-4V", "local", 10)
            .await
            .unwrap();
        assert!(facts.is_empty(), "the dropped fact appears in the store");
        cleanup(&db);
    }

    #[tokio::test]
    async fn no_claims_means_no_store_file_and_no_error() {
        let db = scratch_db();
        let out = store_claims(&[], "https://example.org/paper", "m", &db)
            .await
            .expect("store");
        assert_eq!(out["written"], 0);
        assert_eq!(out["semantic_validation"], serde_json::Value::Null);
        assert!(!db.exists(), "an empty claim set created a database anyway");
    }
}
