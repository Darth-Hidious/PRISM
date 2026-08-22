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
    /// Extract active-ontology-bound claims from a paper via the local LLM.
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
    /// Retrieve a subject's literature and write every paper's full text
    /// into a corpus directory — the input `prism ontology induce` reads.
    /// Papers whose full text is not openly retrievable are reported, not
    /// silently dropped.
    Corpus {
        /// The subject to research, e.g. "refractory high entropy alloy oxidation".
        #[arg(long)]
        query: String,
        /// Corpus directory to write. Created if absent; existing `.txt`
        /// files for the same papers are reused, so a re-run resumes.
        #[arg(long)]
        out: PathBuf,
        /// Max results per source before dedup.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Stop after this many full texts land (0 = every paper retrieved).
        #[arg(long, default_value_t = 0)]
        max_docs: usize,
        /// Shortest accepted full text, in characters. Below this a document
        /// is a parse failure, not a paper.
        #[arg(long, default_value_t = 3000)]
        min_chars: usize,
        #[arg(long)]
        sources: Option<String>,
        #[arg(long)]
        mailto: Option<String>,
        #[arg(long)]
        no_cache: bool,
    },
}

/// Parse `--sources` into REGISTRY id strings — what selection is typed by.
/// The [`SourceId`] enum is used only as the CLI's name catalogue: the CLI
/// can only select built-ins (it has no way to register a third-party
/// adapter), so an unknown name is a typo and fails here with the list.
/// Filesystem-safe stem for a source id. Ids carry `/` (arXiv `cond-mat/0512xxx`)
/// and `:` (some OpenAlex ids), which would otherwise create directories or
/// break on case-insensitive volumes.
/// Whether a paper has SOME openly retrievable full text.
///
/// A PMC id counts: [`prism_retrieval::fulltext::fetch_fulltext`] resolves it
/// to open-access JATS BEFORE it ever looks at `fulltext_url`. Testing the URL
/// alone discarded every PubMed paper in the OA subset — on one live query
/// that was the difference between 12 and 40 retrievable full texts out of the
/// same 113 papers.
fn has_open_fulltext(paper: &Paper) -> bool {
    paper.fulltext_url.is_some() || paper.external_ids.contains_key("pmc")
}

fn corpus_slug(source_id: &str) -> String {
    source_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

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
        PapersCommands::Corpus {
            query,
            out,
            limit,
            max_docs,
            min_chars,
            sources,
            mailto,
            no_cache,
        } => {
            let source_ids = parse_sources(&sources)?;
            let engine = build_engine(source_ids, &mailto, no_cache)
                .with_relevance_policy(RelevancePolicy::default());
            let outcome = engine.search(&query, limit).await;
            std::fs::create_dir_all(&out)
                .with_context(|| format!("cannot create corpus directory {out:?}"))?;

            let mut written = Vec::new();
            let mut reused = Vec::new();
            let mut unavailable = Vec::new();
            for paper in &outcome.papers {
                if max_docs > 0 && written.len() + reused.len() >= max_docs {
                    break;
                }
                let path = out.join(format!(
                    "{}-{}.txt",
                    paper.source,
                    corpus_slug(&paper.source_id)
                ));
                if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) as usize >= min_chars {
                    reused.push(path.display().to_string());
                    continue;
                }
                if !has_open_fulltext(paper) {
                    unavailable.push(
                        json!({"document": paper.url, "reason": "no open full-text location"}),
                    );
                    continue;
                }
                let text = match engine.fetch_fulltext_for(paper).await {
                    Ok(Some(full)) => full.plain_text,
                    Ok(None) => {
                        unavailable.push(
                            json!({"document": paper.url, "reason": "no_fulltext_available"}),
                        );
                        continue;
                    }
                    Err(err) => {
                        unavailable.push(json!({"document": paper.url, "reason": err.to_string()}));
                        continue;
                    }
                };
                if text.chars().count() < min_chars {
                    unavailable.push(json!({
                        "document": paper.url,
                        "reason": format!("parsed {} chars, below --min-chars {min_chars}", text.chars().count()),
                    }));
                    continue;
                }
                std::fs::write(&path, &text).with_context(|| format!("cannot write {path:?}"))?;
                written.push(json!({
                    "path": path.display().to_string(),
                    "title": paper.title,
                    "chars": text.chars().count(),
                }));
            }

            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "corpus_dir": out.display().to_string(),
                    "query": query,
                    "papers_found": outcome.papers.len(),
                    "written": written,
                    "reused": reused,
                    "unavailable": unavailable,
                    "next": format!(
                        "prism ontology induce {} --domain <domain>",
                        out.display()
                    ),
                }))?
            );
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
            let ontology_id = crate::active_ontology_from_config(project_root)?;
            // The UNION of loaded ontologies, active one first: the reader
            // consults every loaded vocabulary, and a term from any of them
            // binds.
            let ontologies = prism_ingest::ontologies::loaded(Some(&ontology_id))?;
            let title = paper.title.clone();
            let document_id = paper
                .doi
                .clone()
                .or_else(|| paper.external_ids.get("pmc").cloned())
                .unwrap_or_else(|| paper.source_id.clone());
            let document_url = paper.url.clone();
            let source = paper.source.clone();

            let mut claims = Vec::new();
            // Retained in the response schema for compatibility. Agentic
            // population records failed checks on each claim instead of
            // moving the claim into this refusal list.
            let rejected: Vec<serde_json::Value> = Vec::new();
            let mut agreement_exclusions: Vec<prism_ingest::text_extract::SampleExclusion> =
                Vec::new();
            // What cross-sample agreement achieved, so a run where nothing
            // agreed cannot report as a clean ingest.
            let mut agreement: Option<prism_ingest::text_extract::AgreementSummary> = None;
            let mut model_insufficient: Option<prism_ingest::text_extract::ModelInsufficiency> =
                None;
            // The paper agent gets one workspace containing every selected
            // source block. Blocks retain their locator and line range for
            // human navigation, but search_paper/read_paper address the same
            // complete text throughout the loop.
            use prism_retrieval::fulltext::BlockKind;
            let selected_blocks = fulltext
                .blocks
                .iter()
                .filter(|block| {
                    // Abstract included deliberately. It was excluded, and the
                    // abstract is where a paper states its headline quantities
                    // in their most self-contained form — the exact shape an
                    // extractor wants. The materials-IE literature is largely
                    // BUILT on abstracts (Dagdelen et al., Nat. Commun. 2024),
                    // so dropping it discarded the highest-density section.
                    //
                    // Duplication with the body is not a cost here: all windows
                    // of one document write under one provenance activity, so a
                    // fact asserted twice counts once and simply gains a
                    // corroboration.
                    //
                    // Title is NOT added: it already reaches the model as the
                    // separate `title` argument to the extractor, and repeating
                    // it inside the body text would only spend context.
                    matches!(
                        block.locator.kind,
                        BlockKind::Abstract
                            | BlockKind::Body
                            | BlockKind::Table
                            | BlockKind::Caption
                    )
                })
                .take(if max_blocks == 0 {
                    usize::MAX
                } else {
                    max_blocks
                })
                .collect::<Vec<_>>();
            let blocks_extracted = selected_blocks.len();
            let mut paper_text = String::new();
            let mut located_lines = Vec::with_capacity(selected_blocks.len());
            for block in &selected_blocks {
                if !paper_text.is_empty() {
                    paper_text.push('\n');
                }
                let line_start = paper_text.bytes().filter(|byte| *byte == b'\n').count() + 1;
                paper_text.push_str(&block.text);
                let line_end = line_start + block.text.lines().count().max(1) - 1;
                located_lines.push((line_start, line_end, &block.locator));
            }

            let mut extraction_failures: Vec<serde_json::Value> = Vec::new();
            let mut agent_traces: Vec<prism_ingest::paper_agent::PaperAgentTrace> = Vec::new();
            let mut agent_turns = 0usize;
            let mut agent_tool_calls = 0usize;
            let mut proposed_classes: Vec<prism_ingest::paper_agent::OntologyClassProposal> =
                Vec::new();
            let mut proposed_relations: Vec<prism_ingest::paper_agent::OntologyRelationProposal> =
                Vec::new();
            let source_snapshot_home = store.then(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                std::path::PathBuf::from(home).join(".prism")
            });
            // Only structurally unrepresentable proposals reach this list.
            // Source checks stay attached to claims as verification notes.
            let mut dropped_facts: Vec<serde_json::Value> = Vec::new();
            if !paper_text.trim().is_empty() {
                let extraction =
                    prism_ingest::text_extract::extract_facts_from_text_with_ontologies_and_policy(
                        &llm,
                        &ontologies,
                        &title,
                        &paper_text,
                        prism_ingest::text_extract::GroundingPolicy::default(),
                        crate::paper_agent_policy(project_root),
                    )
                    .await
                    .with_context(|| "agentic paper extraction failed")?;
                agreement_exclusions = extraction.agreement_exclusions.clone();
                agreement = extraction.agreement.clone();
                model_insufficient = extraction.model_insufficient.clone();
                if let Some(reason) = &extraction.parse_error {
                    extraction_failures.push(json!({
                        "reason": reason,
                    }));
                }
                for reason in &extraction.dropped_facts {
                    dropped_facts.push(json!({
                        "reason": reason,
                    }));
                }
                agent_turns += extraction
                    .agent_traces
                    .iter()
                    .map(|trace| trace.turns)
                    .sum::<usize>();
                agent_tool_calls += extraction
                    .agent_traces
                    .iter()
                    .flat_map(|trace| &trace.samples)
                    .map(|turn| turn.tool_calls.len())
                    .sum::<usize>();
                agent_traces.extend(extraction.agent_traces);
                proposed_classes.extend(extraction.proposed_classes);
                proposed_relations.extend(extraction.proposed_relations);
                if extraction.facts.len() != extraction.citations.len()
                    || extraction.facts.len() != extraction.ontology_bindings.len()
                {
                    extraction_failures.push(json!({
                        "reason": format!(
                            "extractor returned {} facts, {} citations, and {} ontology bindings",
                            extraction.facts.len(),
                            extraction.citations.len(),
                            extraction.ontology_bindings.len()
                        ),
                    }));
                } else {
                    let source_text_path = if extraction.facts.is_empty() {
                        None
                    } else {
                        source_snapshot_home
                            .as_deref()
                            .map(|home| crate::persist_source_text_snapshot(home, &paper_text))
                            .transpose()?
                            .map(|path| path.display().to_string())
                    };
                    for ((fact, citation), ontology_binding) in extraction
                        .facts
                        .into_iter()
                        .zip(extraction.citations)
                        .zip(extraction.ontology_bindings)
                    {
                        let locator = located_lines
                            .iter()
                            .find(|(start, end, _)| {
                                citation.line_start() as usize >= *start
                                    && citation.line_start() as usize <= *end
                            })
                            .map(|(_, _, locator)| *locator)
                            .or_else(|| selected_blocks.first().map(|block| &block.locator))
                            .expect("a non-empty paper workspace has a source locator");
                        // CONTRACT CHANGE (annotate-not-refuse): propose_fact
                        // selected and bounds-checked these exact lines. Do not
                        // run a second lexical refusal over the model's read.
                        let mut claim = claim_from_fact(
                            fact,
                            &document_id,
                            &document_url,
                            &source,
                            locator,
                            source_text_path.as_deref(),
                            &citation,
                            ontology_binding,
                        );
                        claim.evidence_class =
                            prism_retrieval::claims::cap_at_literature(&claim.evidence_class)
                                .to_string();
                        claims.push(claim);
                    }
                }
            }
            // Persisting is opt-in. Until now `papers claims` printed EMMO
            // claims and dropped them: the retrieval half and the graph half
            // were both built and never joined, so PRISM could read a paper
            // without ever knowing what was in it.
            let stored = if store {
                Some({
                    let db_path = prism_provenance::store_path();
                    store_claims(
                        &claims,
                        &fulltext.source_url,
                        &extractor_model,
                        &db_path,
                        &ontologies,
                        &proposed_classes,
                        &proposed_relations,
                    )
                    .await?
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
                    "ontology": ontology_id,
                    "paper_agent": {
                        "loops": agent_traces.len(),
                        "turns": agent_turns,
                        "tool_calls": agent_tool_calls,
                        // Samples that got no agreement vote because they
                        // never had a fair chance to read the document, each
                        // with the measured reason.
                        "agreement_exclusions": agreement_exclusions,
                        // What cross-sample agreement ACHIEVED. Absent for a
                        // single-pass run, which never attempted it.
                        "agreement": agreement,
                        "traces": agent_traces,
                    },
                    // Non-null means EVERY sample showed the routed model was
                    // not capable of reading this document; the annotated
                    // facts are retained, and the verdict names the model,
                    // the numbers, and what to change.
                    "model_insufficient": model_insufficient,
                    "ontology_extensions": {
                        "classes": proposed_classes,
                        "relations": proposed_relations,
                    },
                    // Non-empty means some blocks produced nothing because the
                    // model misbehaved, NOT because the paper was silent there.
                    "extraction_failures": extraction_failures,
                    // Facts dropped individually during extraction because
                    // their JSON shape cannot become a fact. Unit vocabulary
                    // is not a Rust gate: non-empty terms are preserved, and
                    // an absent term remains absent while an explicitly blank
                    // term is stored with a unit-unresolved annotation. One
                    // entry per actual drop.
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
/// the retrieval side and a validated non-empty `UnitTerm` on the storage
/// side.
///
/// Every non-empty unit term is retained exactly as selected by the reading
/// model, whether it is an IRI, a prefixed name, or source spelling. An absent
/// term is also preserved as absence: Rust cannot infer whether the active
/// ontology considers a numeric value dimensionless. An explicitly blank
/// term is a structural defect and is recorded as `unit_unresolved`.
///
/// Evidence class is re-capped through `evidence_for_result` on the way in.
/// This store call is a separate entry point and does not depend on an
/// upstream evidence-class promise.
/// Build typing proposals, carrying the class IRI the reading model selected
/// from the ACTIVE ONTOLOGY.
///
/// `ExtractedClaim.ontology` has always had `subject_class_iri` and
/// `object_class_iri`, and `LocalFact` — the storage-side type this used to
/// take — has no field for either. So the binding was dropped at the
/// conversion and every proposal went out with `class_iri: None`. The comment
/// here even asserted the payload carried no class IRI, which was true of
/// `LocalFact` and false of the claim it came from.
///
/// Measured 2026-08-20 on a real three-paper store: `emmo_entity.class_iri`
/// populated for 10 of 149 rows, every proposal typed `Entity`, and the typing
/// validator reporting "127 carried no declared class IRI" — 100% of them. The
/// geometry check could not run at all, so nothing verified that an entity was
/// filed under the right class.
///
/// Nothing is DERIVED here: a class IRI is used only when the model selected
/// one against the active ontology. Inventing a class from a closed Rust
/// `kind` list would put domain vocabulary in this CLI, which is exactly what
/// the original comment was right to refuse.
fn semantic_entities_for_claims<'a>(
    claims: impl Iterator<
        Item = (
            &'a str,
            &'a str,
            &'a prism_retrieval::claims::ClaimOntologyBinding,
        ),
    >,
) -> Vec<prism_ingest::semantic_validation::SemanticEntityProposal> {
    use prism_ingest::semantic_validation::SemanticEntityProposal;

    // An entity's type label still comes from the ontology binding or stays
    // the neutral `Entity`; the IRI is what makes the class checkable.
    let proposal = |name: &str, class_iri: Option<&String>| SemanticEntityProposal {
        name: name.to_string(),
        entity_type: "Entity".to_string(),
        storage_label: "Entity".to_string(),
        class_iri: class_iri.cloned(),
    };

    let mut entities = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (subject_name, object_name, binding) in claims {
        let subject = proposal(subject_name, binding.subject_class_iri.as_ref());
        let object = proposal(object_name, binding.object_class_iri.as_ref());

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
    ontologies: &prism_ingest::ontologies::OntologySet,
    proposed_classes: &[prism_ingest::paper_agent::OntologyClassProposal],
    proposed_relations: &[prism_ingest::paper_agent::OntologyRelationProposal],
) -> Result<serde_json::Value> {
    use prism_provenance::{
        EvidenceSource, FactPayload as _, LocalProvenance, MaterialFact, MeasurementCondition,
        ProvenanceStore, UnitTerm, evidence_for_result,
    };

    // The PRIMARY (active) ontology owns the storage tenant, the run-level
    // classification stamp, and the typed fact shapes. Class-IRI bindings
    // resolve against the whole loaded set below — a term supplied by a
    // second loaded ontology types its node, and the binding says which.
    let ontology = ontologies.primary();

    if claims.is_empty() && proposed_classes.is_empty() && proposed_relations.is_empty() {
        return Ok(json!({
            "written": 0,
            "rejected": 0,
            "store": null,
            "semantic_validation": null,
        }));
    }

    let store = ProvenanceStore::open(db_path).await?;
    let now = chrono::Utc::now().to_rfc3339();
    let base_prov = LocalProvenance {
        activity_id: uuid::Uuid::new_v4().to_string(),
        agent_id: if model.is_empty() {
            "prism-papers".to_string()
        } else {
            model.to_string()
        },
        agent_kind: "SoftwareAgent".into(),
        source_entity_id: document_url.to_string(),
        source_kind: "Document".into(),
        // Same composed tenancy as every other ingest path — a promoted
        // ontology's claims must not blend into EMMO's keyspace.
        tenant: prism_ingest::ontologies::storage_tenant(
            prism_provenance::LOCAL_TENANT,
            ontology.id(),
        ),
        started_at: now.clone(),
        ended_at: now,
        locality: "local".into(),
        // Legacy/uncached claims fall back to this URL. New population runs
        // replace it per claim with the exact cached source-text block while
        // retaining this URL as origin_source_id.
        origin_source_id: None,
    };
    let mut rejected: Vec<serde_json::Value> = Vec::new();
    let mut citation_warnings: Vec<serde_json::Value> = Vec::new();
    let mut prepared = Vec::with_capacity(claims.len());

    for claim in claims {
        let mut verification = claim.verification;
        let mut verification_reason = claim.verification_reason.clone();
        let (unit, unit_error) = match claim.unit.as_deref() {
            Some(raw) => match UnitTerm::new(raw) {
                Ok(unit) => (Some(unit), None),
                Err(error) => (None, Some(format!("unit term {raw:?} is empty: {error}"))),
            },
            None => (None, None),
        };
        if let Some(reason) = unit_error {
            let status = prism_provenance::VerificationStatus::UnitUnresolved;
            if verification.is_none_or(|current| status.rank() < current.rank()) {
                verification = Some(status);
                verification_reason = Some(reason);
            }
        }

        let mut conditions = Vec::with_capacity(claim.conditions.len());
        let mut condition_unit_defect = None;
        for condition in &claim.conditions {
            let cond_unit = match condition.unit.as_deref() {
                Some(raw) => match UnitTerm::new(raw) {
                    Ok(unit) => Some(unit),
                    Err(e) => {
                        condition_unit_defect.get_or_insert_with(|| {
                            format!("condition {:?} unit {raw:?}: {e}", condition.name)
                        });
                        None
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
        if let Some(reason) = condition_unit_defect {
            let status = prism_provenance::VerificationStatus::UnitUnresolved;
            if verification.is_none_or(|current| status.rank() < current.rank()) {
                verification = Some(status);
                verification_reason = Some(reason);
            }
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
            // Preserve the reader's verification annotation. `None` is kept
            // only for legacy claims that predate the paper-agent status and
            // remains included in compatibility reads.
            verification,
            verification_reason,
        };

        // Claim provenance used to die at this conversion: the quote and
        // locator were present on `ExtractedClaim`, then only the document
        // URL survived into `LocalProvenance`. A complete citation now rides
        // the per-source evidence row. An incomplete/invalid citation is a
        // NOTE, not a verdict: the fact still follows the normal write path.
        let citation = match (
            claim.provenance.source_revision_id.as_deref(),
            claim.provenance.line_start,
            claim.provenance.line_end,
            claim.provenance.quote.as_deref(),
        ) {
            (Some(revision), Some(line_start), Some(line_end), Some(span)) => {
                let locator_json = serde_json::to_string(&claim.provenance.locator).ok();
                match prism_provenance::SourceCitation::new(
                    line_start,
                    line_end,
                    span,
                    revision,
                    locator_json,
                ) {
                    Ok(citation) => Some(citation),
                    Err(error) => {
                        citation_warnings.push(json!({
                            "subject": claim.subject,
                            "object": claim.object,
                            "reason": error.to_string(),
                        }));
                        None
                    }
                }
            }
            fields => {
                citation_warnings.push(json!({
                    "subject": claim.subject,
                    "object": claim.object,
                    "reason": if fields == (None, None, None, None) {
                        "claim has no exact source revision, line range, or evidence span"
                    } else {
                        "claim citation is incomplete; revision, both line bounds, and span are all required"
                    },
                }));
                None
            }
        };

        let mut claim_prov = base_prov.clone();
        claim_prov.activity_id = uuid::Uuid::new_v4().to_string();
        if let Some(source_text_path) = claim.provenance.source_text_path.as_deref() {
            claim_prov.source_entity_id = source_text_path.to_string();
            claim_prov.origin_source_id = Some(document_url.to_string());
        }
        prepared.push((claim, fact, citation, claim_prov));
    }

    // The model proposes; geometry measures. Every structurally accepted
    // claim is prepared before the first activity/fact write. Cost per paper:
    // one embedding-model batch for all distinct endpoints, plus the
    // validator's batched graph scans -- never one model/SQL round trip per
    // claim. The advisory report is never consulted to merge, drop, rewrite,
    // or block a claim.
    let local_facts: Vec<_> = prepared
        .iter()
        .map(|(_, fact, _, _)| fact.to_local_fact())
        .collect();
    // From the CLAIMS, not the converted facts — the conversion has no field
    // for a class IRI, so taking facts here silently untyped every entity.
    let semantic_entities =
        semantic_entities_for_claims(prepared.iter().map(|(claim, _, _, _)| {
            (
                claim.subject.as_str(),
                claim.object.as_str(),
                &claim.ontology,
            )
        }));
    let mut semantic_policy =
        prism_ingest::semantic_validation::SemanticValidationPolicy::default();
    // The numeric prior's eligible kinds come from the active ontology's
    // declaration, not a materials-shaped Rust default.
    semantic_policy.resolve_eligible_fact_kinds(ontology);
    let semantic = prism_ingest::semantic_validation::validate_write_best_effort(
        &store,
        &semantic_entities,
        &local_facts,
        &base_prov.tenant,
        &semantic_policy,
    )
    .await;

    let mut written = 0usize;
    for (claim, fact, citation, claim_prov) in &prepared {
        store.record_activity(claim_prov).await?;
        let classification = prism_provenance::OntologyClassification {
            version_iri: ontology.version_iri().as_str(),
            artifact_sha256: ontology.artifact_sha256(),
        };
        let write = match citation {
            Some(citation) => {
                let subject = claim
                    .ontology
                    .subject_class_iri
                    .as_deref()
                    .map(|iri| prism_ingest::paper_agent::resolve_class_binding(ontologies, iri))
                    .transpose()
                    .map_err(anyhow::Error::msg)?;
                let object = claim
                    .ontology
                    .object_class_iri
                    .as_deref()
                    .map(|iri| prism_ingest::paper_agent::resolve_class_binding(ontologies, iri))
                    .transpose()
                    .map_err(anyhow::Error::msg)?;
                let nodes = prism_provenance::OntologyBoundFactNodes {
                    subject: subject
                        .as_ref()
                        .map(|node| prism_provenance::ClassifiedNode {
                            entity_type: &node.entity_type,
                            storage_label: &node.storage_label,
                            class_iri: &node.class_iri,
                        }),
                    object: object
                        .as_ref()
                        .map(|node| prism_provenance::ClassifiedNode {
                            entity_type: &node.entity_type,
                            storage_label: &node.storage_label,
                            class_iri: &node.class_iri,
                        }),
                };
                store
                    .write_ontology_bound_fact_with_citation(
                        fact,
                        claim_prov,
                        fact.evidence_class,
                        nodes,
                        classification,
                        citation,
                    )
                    .await
            }
            None => {
                store
                    .write_fact_with_classification(
                        fact,
                        claim_prov,
                        classification,
                        // The graph shape for the claim's kind is the active
                        // ontology's declaration — the store holds no
                        // kind→(class, edge) table of its own.
                        fact.kind
                            .as_deref()
                            .and_then(|kind| ontology.fact_graph_shape(kind)),
                    )
                    .await
            }
        };
        match write {
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
                &base_prov.tenant,
                embedding_model,
            )
            .await
    {
        tracing::warn!(%error, "papers claim embeddings were not stored");
    }

    // Persist the ontology-extension proposals with their citations — the
    // same governance queue the text-ingest path writes. Until now this
    // command PRINTED them into its JSON blob and nothing else: a proposal
    // died with the terminal it was printed to. Identities already
    // dispositioned are suppressed and counted, never silently dropped.
    let mut proposals_enqueued = 0usize;
    let mut proposals_suppressed = 0usize;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0);
    {
        use prism_ingest::paper_agent::{class_proposal_queue_item, relation_proposal_queue_item};
        let mut queued = Vec::new();
        for proposal in proposed_classes {
            queued.push(class_proposal_queue_item(
                proposal,
                document_url,
                &base_prov.tenant,
                now_secs,
            ));
        }
        for proposal in proposed_relations {
            queued.push(relation_proposal_queue_item(
                proposal,
                document_url,
                &base_prov.tenant,
                now_secs,
            ));
        }
        for (item, citation_json) in queued {
            match store
                .enqueue_ontology_proposal(&item, &citation_json, now_secs)
                .await
            {
                Ok(prism_provenance::OntologyProposalEnqueue::SupersededByDisposition) => {
                    proposals_suppressed += 1;
                }
                Ok(_) => proposals_enqueued += 1,
                Err(error) => {
                    rejected.push(json!({
                        "subject": item.label,
                        "object": "ontology-proposal",
                        "reason": format!("governance queue write failed: {error}"),
                    }));
                }
            }
        }
    }

    Ok(json!({
        "written": written,
        "rejected": rejected.len(),
        "rejections": rejected,
        "citation_warnings": citation_warnings,
        "ontology_proposals": {
            "enqueued": proposals_enqueued,
            "suppressed": proposals_suppressed,
        },
        "store": db_path.display().to_string(),
        "tenant": base_prov.tenant,
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

/// Convert one extracted `MaterialFact` into a provenance-carrying claim,
/// retaining the exact source revision, line range, and span the paper agent
/// read before it proposed the fact. Literature evidence keeps its research
/// ceiling; no second lexical validator replaces the agent's cited read.
#[allow(clippy::too_many_arguments)]
fn claim_from_fact(
    fact: prism_provenance::MaterialFact,
    document_id: &str,
    document_url: &str,
    source: &str,
    locator: &prism_retrieval::Locator,
    source_text_path: Option<&str>,
    citation: &prism_provenance::SourceCitation,
    ontology_binding: prism_ingest::paper_agent::FactOntologyBinding,
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
        verification: fact.verification,
        verification_reason: fact.verification_reason,
        ontology: prism_retrieval::claims::ClaimOntologyBinding {
            subject_class_iri: ontology_binding.subject_class_iri,
            predicate_iri: ontology_binding.predicate_iri,
            object_class_iri: ontology_binding.object_class_iri,
            subject_ontology_id: ontology_binding.subject_ontology_id,
            predicate_ontology_id: ontology_binding.predicate_ontology_id,
            object_ontology_id: ontology_binding.object_ontology_id,
        },
        provenance: prism_retrieval::claims::ClaimProvenance {
            document_id: document_id.to_string(),
            document_url: document_url.to_string(),
            source: source.to_string(),
            source_revision_id: Some(citation.source_revision_id().to_string()),
            line_start: Some(citation.line_start()),
            line_end: Some(citation.line_end()),
            source_text_path: source_text_path.map(str::to_string),
            locator: locator.clone(),
            quote: Some(citation.evidence_span().to_string()),
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
    fn bare_paper() -> Paper {
        Paper {
            source: "pubmed".into(),
            source_id: "12345".into(),
            title: "T".into(),
            authors: Vec::new(),
            year: None,
            published: None,
            doi: None,
            external_ids: std::collections::BTreeMap::new(),
            abstract_text: None,
            url: "https://example.org/12345".into(),
            fulltext_url: None,
            fulltext_format: None,
            journal: None,
        }
    }

    #[test]
    fn a_pmc_id_is_a_full_text_location() {
        // Nothing to fetch: neither a URL nor a PMC id.
        assert!(!has_open_fulltext(&bare_paper()));

        // PubMed advertises no fulltext_url at all, but a PMC id resolves to
        // open-access JATS. Requiring the URL threw these away.
        let mut pmc = bare_paper();
        pmc.external_ids.insert("pmc".into(), "PMC7654321".into());
        assert!(has_open_fulltext(&pmc), "a PMC id must count as full text");

        // A DOI is NOT a full-text location.
        let mut doi_only = bare_paper();
        doi_only
            .external_ids
            .insert("doi".into(), "10.1000/x".into());
        assert!(!has_open_fulltext(&doi_only));

        // The advertised URL still counts on its own.
        let mut url = bare_paper();
        url.fulltext_url = Some("https://arxiv.org/pdf/2512.06308v2".into());
        assert!(has_open_fulltext(&url));
    }

    #[test]
    fn corpus_filenames_cannot_escape_the_corpus_directory() {
        // arXiv ids carry '/' (cond-mat/0512345); some ids carry ':'.
        assert_eq!(corpus_slug("cond-mat/0512345"), "cond-mat_0512345");
        assert_eq!(corpus_slug("2512.06308v2"), "2512.06308v2");
        assert_eq!(corpus_slug("../../etc/passwd"), ".._.._etc_passwd");
        assert!(!corpus_slug("a/b:c").contains('/'));
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
            verification: None,
            verification_reason: None,
            ontology: Default::default(),
            provenance: ClaimProvenance {
                document_id: "10.1000/xyz".into(),
                document_url: "https://example.org/paper".into(),
                source: "arxiv".into(),
                source_revision_id: None,
                line_start: None,
                line_end: None,
                source_text_path: None,
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
    fn claim_endpoints_stay_generic_without_an_ontology_class_iri() {
        // CONTRACT CHANGE (agentic paper reading): a closed `kind`-to-class
        // table no longer pretends to classify endpoints. Until an ontology
        // IRI is proposed, semantic validation sees a generic entity.
        let facts = [
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

        // Facts with no ontology binding: the proposals must stay untyped
        // rather than inventing a class.
        let empty = prism_retrieval::claims::ClaimOntologyBinding::default();
        let entities = semantic_entities_for_claims(
            facts
                .iter()
                .map(|f| (f.subject.as_str(), f.object.as_str(), &empty)),
        );
        let subject = entities
            .iter()
            .find(|entity| entity.name == "Ti-6Al-4V")
            .expect("subject proposal");
        assert_eq!(subject.entity_type, "Entity");
        assert_eq!(subject.storage_label, "Entity");
        assert_eq!(subject.class_iri, None);

        let object = entities
            .iter()
            .find(|entity| entity.name == "UTS")
            .expect("object proposal");
        assert_eq!(object.entity_type, "Entity");
        assert_eq!(object.storage_label, "Entity");
        assert_eq!(object.class_iri, None);

        let legacy = entities
            .iter()
            .find(|entity| entity.name == "turbine blade")
            .expect("legacy object proposal");
        assert_eq!(legacy.entity_type, "Entity");
        assert_eq!(legacy.storage_label, "Entity");
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
        let ontologies = prism_ingest::ontologies::loaded(None).expect("default ontology");
        let mut cited = claim("UTS", Some("QUDT:MegaPA"), None);
        cited.provenance.source_revision_id = Some("a".repeat(64));
        cited.provenance.line_start = Some(7);
        cited.provenance.line_end = Some(8);
        cited.provenance.quote = Some("UTS was 1140 MPa".into());

        let out = store_claims(
            &[cited],
            "https://example.org/paper",
            "test-model",
            &db,
            &ontologies,
            &[],
            &[],
        )
        .await
        .expect("store");

        assert_eq!(out["written"], 1, "claim was not written: {out}");
        assert_eq!(out["rejected"], 0);
        assert_eq!(out["citation_warnings"], json!([]), "{out}");
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
            .recall_with_context_filtered(
                "Ti-6Al-4V",
                &["local"],
                10,
                prism_provenance::VerificationFilter::Any,
            )
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
        // CONTRACT CHANGE (agentic paper reading): provenance now retains
        // the exact revision and line witness rather than stopping at a
        // prose URL on the aggregate assertion.
        let assertion_id = prism_provenance::conditioned_assertion_id(
            "local",
            "Ti-6Al-4V",
            "has_measurement",
            "UTS",
            Some(1140.0),
            Some("QUDT:MegaPA"),
            &[],
        )
        .unwrap();
        let evidence = store.assertion_evidence_by_id(&assertion_id).await.unwrap();
        assert_eq!(evidence.len(), 1);
        let expected_revision = "a".repeat(64);
        assert_eq!(
            evidence[0].source_revision_id.as_deref(),
            Some(expected_revision.as_str())
        );
        assert_eq!(evidence[0].line_start, Some(7));
        assert_eq!(evidence[0].line_end, Some(8));
        assert_eq!(
            evidence[0].evidence_span.as_deref(),
            Some("UTS was 1140 MPa")
        );
        cleanup(&db);
    }

    /// CONTRACT CHANGE (annotate-not-refuse): missing legacy citation data is
    /// a visible note, not a reason to discard an otherwise storable fact.
    #[tokio::test]
    async fn an_uncited_legacy_claim_is_stored_with_a_citation_warning() {
        // CONTRACT CHANGE: citation metadata added by agentic population is
        // nullable for old rows; absence is reported without restoring the
        // former claim drop.
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };
        let db = scratch_db();
        let ontologies = prism_ingest::ontologies::loaded(None).expect("default ontology");

        let out = store_claims(
            &[claim("UTS", Some("QUDT:MegaPA"), None)],
            "https://example.org/legacy-paper",
            "test-model",
            &db,
            &ontologies,
            &[],
            &[],
        )
        .await
        .expect("legacy fact remains storable");

        assert_eq!(out["written"], 1, "{out}");
        assert_eq!(out["rejected"], 0, "{out}");
        assert_eq!(out["citation_warnings"].as_array().unwrap().len(), 1);
        assert!(
            out["citation_warnings"][0]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("no exact source revision")),
            "{out}"
        );
        cleanup(&db);
    }

    #[tokio::test]
    async fn customer_unit_terms_are_preserved_exactly() {
        // CONTRACT CHANGE (vocabulary-neutral units): this formerly treated
        // every non-QUDT spelling as invalid. Storage now preserves the exact
        // non-empty terms selected from a customer's active ontology; Rust
        // neither translates nor rejects them.
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };
        let db = scratch_db();
        let ontologies = prism_ingest::ontologies::loaded(None).expect("default ontology");
        let selected_unit = "customer:U-42";
        let selected_condition_unit = "https://customer.example/ontology/unit/C-7";

        let out = store_claims(
            &[claim(
                "reported property",
                Some(selected_unit),
                Some(selected_condition_unit),
            )],
            "https://example.org/paper",
            "test-model",
            &db,
            &ontologies,
            &[],
            &[],
        )
        .await
        .expect("store");

        assert_eq!(out["written"], 1, "claim was not written: {out}");
        assert_eq!(out["rejected"], 0, "{out}");

        let store = prism_provenance::ProvenanceStore::open(&db).await.unwrap();
        let facts = store
            .recall_with_context_filtered(
                "Ti-6Al-4V",
                &["local"],
                10,
                prism_provenance::VerificationFilter::Any,
            )
            .await
            .unwrap();
        assert_eq!(facts.len(), 1, "claim disappeared: {facts:?}");
        assert_eq!(facts[0].unit.as_deref(), Some(selected_unit));
        assert_eq!(facts[0].conditions.len(), 1);
        assert_eq!(
            facts[0].conditions[0]
                .unit
                .as_ref()
                .map(|unit| unit.as_str()),
            Some(selected_condition_unit)
        );
        assert_eq!(facts[0].verification_status, None);
        cleanup(&db);
    }

    #[tokio::test]
    async fn absent_unit_terms_are_semantic_and_blank_terms_are_annotated() {
        // CONTRACT CHANGE (vocabulary-neutral units): Rust cannot declare an
        // absent term wrong because the active ontology may define the value
        // as dimensionless. Only an explicitly blank term is a structural
        // defect; every fact still remains stored.
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };
        let db = scratch_db();
        let ontologies = prism_ingest::ontologies::loaded(None).expect("default ontology");

        let mut missing_condition = claim("missing condition unit", Some("customer:U-42"), None);
        missing_condition.conditions.push(MeasurementCondition {
            name: "customer:condition".into(),
            value: ConditionValue::Number(7.4),
            unit: None,
        });

        let out = store_claims(
            &[
                claim("missing unit", None, None),
                claim("blank unit", Some("   "), None),
                missing_condition,
            ],
            "https://example.org/paper",
            "test-model",
            &db,
            &ontologies,
            &[],
            &[],
        )
        .await
        .expect("store");

        assert_eq!(out["written"], 3, "facts were not written: {out}");
        assert_eq!(out["rejected"], 0, "{out}");

        let store = prism_provenance::ProvenanceStore::open(&db).await.unwrap();
        let facts = store
            .recall_with_context_filtered(
                "Ti-6Al-4V",
                &["local"],
                10,
                prism_provenance::VerificationFilter::Any,
            )
            .await
            .unwrap();
        assert_eq!(facts.len(), 3, "claims disappeared: {facts:?}");
        let missing = facts
            .iter()
            .find(|fact| fact.object == "missing unit")
            .expect("missing-unit fact");
        assert_eq!(missing.unit, None);
        assert_eq!(missing.verification_status, None);
        assert_eq!(missing.verification_reason, None);
        let missing_condition = facts
            .iter()
            .find(|fact| fact.object == "missing condition unit")
            .expect("missing-condition-unit fact");
        assert_eq!(missing_condition.verification_status, None);
        assert_eq!(missing_condition.conditions[0].unit, None);
        let blank = facts
            .iter()
            .find(|fact| fact.object == "blank unit")
            .expect("blank-unit fact");
        assert_eq!(blank.unit, None);
        assert_eq!(
            blank.verification_status,
            Some(prism_provenance::VerificationStatus::UnitUnresolved)
        );
        assert!(
            blank
                .verification_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("is empty")),
            "blank unit annotation: {facts:?}"
        );
        cleanup(&db);
    }

    #[tokio::test]
    async fn a_valueless_legacy_kind_hint_is_stored_as_a_generic_edge() {
        // CONTRACT CHANGE (agentic paper reading): `kind` is no longer a
        // closed dispatch instruction. A value-less relation is therefore a
        // normal cited edge, not a silently dropped malformed measurement.
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };
        let db = scratch_db();
        let ontologies = prism_ingest::ontologies::loaded(None).expect("default ontology");

        let mut c = claim("UTS", Some("QUDT:MegaPA"), None);
        c.value = None; // kind stays "measurement"

        let out = store_claims(
            &[c],
            "https://example.org/paper",
            "m",
            &db,
            &ontologies,
            &[],
            &[],
        )
        .await
        .expect("store");

        assert_eq!(out["written"], 1, "generic edge was not written: {out}");
        assert_eq!(out["rejected"], 0, "{out}");

        let store = prism_provenance::ProvenanceStore::open(&db).await.unwrap();
        let facts = store
            .recall_with_context("Ti-6Al-4V", "local", 10)
            .await
            .unwrap();
        assert_eq!(facts.len(), 1, "generic edge disappeared: {facts:?}");
        assert_eq!(facts[0].object, "UTS");
        assert_eq!(facts[0].value, None);
        cleanup(&db);
    }

    #[tokio::test]
    async fn no_claims_means_no_store_file_and_no_error() {
        let db = scratch_db();
        let ontologies = prism_ingest::ontologies::loaded(None).expect("default ontology");
        let out = store_claims(
            &[],
            "https://example.org/paper",
            "m",
            &db,
            &ontologies,
            &[],
            &[],
        )
        .await
        .expect("store");
        assert_eq!(out["written"], 0);
        assert_eq!(out["semantic_validation"], serde_json::Value::Null);
        assert!(!db.exists(), "an empty claim set created a database anyway");
    }
}
