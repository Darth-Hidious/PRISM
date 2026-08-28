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
    EngineConfig, Paper, RelevancePolicy, RetrievalEngine, SourceId, SweepPlan,
    fulltext::BlockKind, sweep,
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

/// The precision judge for the selector stage, when an LLM is configured.
///
/// `None` is the honest answer for a deployment with no LLM: the stage then
/// reports itself unavailable and keeps every paper. It is never a silent
/// pass-through — a search that COULD NOT be judged must not read like one
/// that was judged and found everything relevant.
fn literature_judge(
    project_root: &std::path::Path,
) -> Option<std::sync::Arc<dyn prism_retrieval::Selector>> {
    let cfg = crate::build_llm_config(project_root, None, None, None).ok()?;
    if cfg.base_url.trim().is_empty() || cfg.model.trim().is_empty() {
        return None;
    }
    Some(std::sync::Arc::new(prism_retrieval::LlmSelector::new(
        prism_llm::LlmClient::new(cfg),
    )))
}

/// Both relevance stages, in order, for every path that returns papers to a
/// caller — `search` and `corpus` alike, because a corpus written to disk
/// carries its mistakes further than a search result does.
///
/// They answer different questions. The embedding filter scores SIMILARITY,
/// which buys recall: measured 2026-08-28 it kept "Completely Symmetric
/// Resistance Forms on the Stretched Sierpinski Gasket" for a query about
/// sealing gaskets, because the words match though the subjects share
/// nothing. No threshold separates those two — the genuinely relevant
/// PFAS-free-seals paper scored no higher, and papers dropped at 0.5937 were
/// no worse. The selector asks a QUESTION instead — would reading this help —
/// which is the judgement a cosine cannot make at any threshold.
fn with_relevance_stages(
    engine: RetrievalEngine,
    project_root: &std::path::Path,
) -> RetrievalEngine {
    engine
        .with_relevance_policy(RelevancePolicy::default())
        .with_selector_policy(prism_retrieval::SelectorPolicy::default())
        .with_selector(literature_judge(project_root))
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
            let engine =
                with_relevance_stages(build_engine(source_ids, &mailto, no_cache), project_root);
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
            // Every source, not just arXiv. `parse_sources(&None)` already means
            // "all adapters"; hardcoding arXiv here meant SEARCH could reach every
            // source while EXTRACTION could fetch from exactly one — and a paper on
            // any other host was reported `no_fulltext_available`, which is a lie:
            // the paper has full text, this engine had no adapter wired for it.
            let engine = build_engine(parse_sources(&None)?, &None, false);
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
            let engine =
                with_relevance_stages(build_engine(source_ids, &mailto, no_cache), project_root);
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
            // Every source, not just arXiv. `parse_sources(&None)` already means
            // "all adapters"; hardcoding arXiv here meant SEARCH could reach every
            // source while EXTRACTION could fetch from exactly one — and a paper on
            // any other host was reported `no_fulltext_available`, which is a lie:
            // the paper has full text, this engine had no adapter wired for it.
            let engine = build_engine(parse_sources(&None)?, &None, false);
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
            // The search record's title when there is one; otherwise the
            // document's own. `papers_ingest` takes a URL, so on that path
            // `paper.title` is always empty.
            let title = Some(paper.title.clone())
                .filter(|t| !t.trim().is_empty())
                .or_else(|| title_from_document(&fulltext))
                .unwrap_or_default();
            let document_id = paper
                .doi
                .clone()
                .or_else(|| paper.external_ids.get("pmc").cloned())
                .unwrap_or_else(|| paper.source_id.clone());
            let source = paper.source.clone();

            // One per chunk and per agreement sample; `best_write_up` picks
            // the one that stands for the paper.
            let mut write_ups: Vec<prism_ingest::paper_agent::PaperWriteUp> = Vec::new();
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
            let (paper_text, located_lines) = assemble_paper_workspace(&fulltext, max_blocks);
            let blocks_extracted = located_lines.len();

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
                write_ups.extend(extraction.write_ups);
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
                            .or_else(|| located_lines.first().map(|(_, _, locator)| *locator))
                            .expect("a non-empty paper workspace has a source locator");
                        // CONTRACT CHANGE (annotate-not-refuse): propose_fact
                        // selected and bounds-checked these exact lines. Do not
                        // run a second lexical refusal over the model's read.
                        let mut claim = claim_from_fact(
                            fact,
                            &document_id,
                            &paper.url,
                            &source,
                            locator,
                            source_text_path.as_deref(),
                            &citation,
                            ontology_binding,
                        );
                        // Measured 2026-08-28: all 218 facts of a live run
                        // stored `indeterminate` — which `EvidenceClass`
                        // itself defines as "model assertion with NO
                        // grounding". Every one had a source revision hash,
                        // an exact line range and a citation span. By the
                        // enum's own definition that is `research`,
                        // "extracted from literature".
                        //
                        // The cause was a ceiling with no floor.
                        // `cap_at_literature` can only LOWER a class, the
                        // model is never asked for one (paper_agent asserts
                        // it does not supply it), so the serde default —
                        // `Indeterminate` — survived every time.
                        //
                        // A claim reaching this line came from a cited read
                        // of a fetched document. That provenance is the
                        // pipeline's own, and it is stronger evidence than
                        // any class a model could self-report, so it is not
                        // capped DOWN from here either: literature evidence
                        // is exactly research, never verified higher by the
                        // act of reading, never lower than what was cited.
                        claim.evidence_class =
                            prism_retrieval::claims::EVIDENCE_RESEARCH.to_string();
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
                        ReadPaper {
                            url: &fulltext.source_url,
                            title: &title,
                            // The search record's abstract when there is one;
                            // otherwise the document's own, because
                            // papers_ingest is given a URL and has no search
                            // record to read from. Absent in both is stored
                            // as absent.
                            abstract_text: &paper
                                .abstract_text
                                .clone()
                                .or_else(|| labelled_block(&fulltext, BlockKind::Abstract))
                                .or_else(|| abstract_from_document(&fulltext.plain_text))
                                .unwrap_or_default(),
                            write_up: prism_ingest::text_extract::best_write_up(&write_ups),
                        },
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

/// The text of the first block the parser labelled `kind`.
///
/// JATS declares the title and the abstract STRUCTURALLY — `<article-title>`
/// and `<abstract>` — so neither the title nor the word "Abstract" appears as
/// a heading in `plain_text`. Scanning the text therefore finds nothing on the
/// JATS path, which is the path six of the seven papers in the 2026-08-28 run
/// took. The label is the paper's own declaration and is exact; prefer it, and
/// keep the text scan for PDFs, where there are no labels and the heading
/// really is in the prose.
#[must_use]
fn labelled_block(fulltext: &prism_retrieval::Fulltext, kind: BlockKind) -> Option<String> {
    let text = fulltext
        .blocks
        .iter()
        .find(|block| block.locator.kind == kind)?
        .text
        .trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The paper's own title, taken from the document when it declares one.
///
/// Same hole as the abstract below, same cause: `papers_ingest` is given a
/// bare URL or PMC id, so there is no search record and `Paper::title` is
/// `String::new()`. Measured 2026-08-28: all seven notes of a live run stored
/// an empty title, which leaves a reader unable to tell one write-up from
/// another.
///
/// An over-long block is REFUSED rather than truncated. A truncated abstract
/// is still the abstract's opening and useful; a truncated title is simply a
/// different, wrong title, and a wrong title is worse than an honest absence.
#[must_use]
fn title_from_document(fulltext: &prism_retrieval::Fulltext) -> Option<String> {
    /// Generous for a real title, short enough that a mis-parsed block that
    /// swallowed the body is recognisable as one.
    const MAX_TITLE_CHARS: usize = 500;

    labelled_block(fulltext, BlockKind::Title).filter(|t| t.chars().count() <= MAX_TITLE_CHARS)
}

/// The paper's own abstract, taken from the document when it declares one.
///
/// A search result carries an abstract; a bare URL does not, and
/// `papers_ingest` takes a URL. Measured on the run of 2026-08-28: the
/// write-up stored and `abstract kept: 0 chars` — the note landed, the
/// paper's own summary did not, because the field was read from a search
/// record that path never has.
///
/// So it is read from the document, and ONLY where the document says so. The
/// tempting alternative — take the first N characters — would file a title
/// block, an author list and a footer under the name "abstract", which is
/// inventing a summary rather than keeping one. A paper that declares no
/// abstract gets `None`, and absence is stored as absence.
#[must_use]
fn abstract_from_document(plain_text: &str) -> Option<String> {
    /// Long enough for a dense abstract, short enough that a missed section
    /// boundary cannot swallow the introduction.
    const MAX_ABSTRACT_CHARS: usize = 4_000;

    let lower = plain_text.to_lowercase();
    let start = lower.find("abstract")?;
    // Skip the heading word itself plus any punctuation or dash that follows
    // it, so the stored text begins at the prose.
    let after = plain_text[start + "abstract".len()..]
        .trim_start_matches([':', '.', '-', '—', '–', ' ', '\t', '\r', '\n']);

    // An abstract ends where the next section begins. Match the headings
    // papers actually use, lowercased; whichever comes first wins.
    let body_lower = after.to_lowercase();
    let end = [
        "introduction",
        "1. introduction",
        "1 introduction",
        "keywords",
        "index terms",
    ]
    .iter()
    .filter_map(|marker| body_lower.find(marker))
    .min()
    .unwrap_or(after.len())
    .min(MAX_ABSTRACT_CHARS);

    let text = after[..end].trim();
    // A heading with nothing under it is not an abstract.
    (!text.is_empty()).then(|| text.to_string())
}

/// One paper as it was actually read: its identity, what it says about
/// itself, and the reader's write-up.
///
/// Grouped because these four travel together and mean nothing apart — a
/// write-up with no url cannot be filed, and a url with no reading is the
/// state that made a run's sources unaccountable.
#[derive(Clone, Copy)]
struct ReadPaper<'a> {
    /// DOI, arXiv id or URL — the paper's own identity, and the key its node
    /// is stored under.
    url: &'a str,
    title: &'a str,
    /// Kept verbatim. Fetched on every search and discarded before the note
    /// existed.
    abstract_text: &'a str,
    /// `None` when the reader produced none, which is reported as absence.
    write_up: Option<&'a prism_ingest::paper_agent::PaperWriteUp>,
}

async fn store_claims(
    claims: &[prism_retrieval::claims::ExtractedClaim],
    paper: ReadPaper<'_>,
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

    // The paper's own node: what it WAS, beside the facts taken from it.
    // Written here because this is the layer that has the whole paper — its
    // url, title and abstract — and the store; the chunk reader has neither,
    // which is why the write-up is collected there and persisted here.
    //
    // The abstract is kept verbatim. It was fetched on every search and
    // discarded, so after a run nothing could say what any source argued.
    if let Some(write_up) = paper.write_up {
        store
            .record_paper_note(&prism_provenance::PaperNote {
                source_id: paper.url.to_string(),
                tenant: prism_provenance::LOCAL_TENANT.to_string(),
                title: paper.title.to_string(),
                abstract_text: paper.abstract_text.to_string(),
                review: write_up.key_findings.clone(),
                question: write_up.question.clone(),
                method: write_up.method.clone(),
                key_findings: write_up.key_findings.clone(),
                limitations: write_up.limitations.clone(),
                // Empty until the caller that HAS a task passes one down: the
                // CLI reads a url, not a research question. Recorded as absent
                // rather than filled with the paper's own question, which
                // would quietly answer "useful for what?" with the wrong
                // thing.
                task: String::new(),
                relevance: write_up.relevance.clone(),
                depth: write_up.depth.clone(),
                depth_reason: write_up.depth_reason.clone(),
                next_steps: write_up.next_steps.clone(),
                // Set by the caller that knows which paper sent it here.
                led_from: None,
                origin_action_id: prism_provenance::action_id_from_env(),
                created_at: now.clone(),
            })
            .await?;
    }
    let base_prov = LocalProvenance {
        activity_id: uuid::Uuid::new_v4().to_string(),
        agent_id: if model.is_empty() {
            "prism-papers".to_string()
        } else {
            model.to_string()
        },
        agent_kind: "SoftwareAgent".into(),
        source_entity_id: paper.url.to_string(),
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
        // The agent tool call that launched this CLI run, when one did.
        // `None` when a person ran the command directly.
        origin_action_id: prism_provenance::action_id_from_env(),
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
            claim_prov.origin_source_id = Some(paper.url.to_string());
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

    // The ontology resolution ladder, AFTER the writes: bind each measured
    // claim's free-text property name against the union of loaded
    // ontologies (exact → normalised → semantic → proposal). Facts are
    // already stored, so no rung can discard one; a resolution failure is
    // reported in the result, never allowed to fail the ingest it follows.
    let property_terms = property_terms_for_claims(ontologies, &prepared);
    let property_resolution = if property_terms.is_empty() {
        None
    } else {
        let backend = tokio::task::spawn_blocking(prism_embed::from_config)
            .await
            .ok()
            .flatten();
        match prism_ingest::property_resolution::resolve_property_terms(
            &store,
            ontologies,
            backend.as_deref(),
            &base_prov.tenant,
            paper.url,
            &property_terms,
            prism_ingest::property_resolution::DEFAULT_SEMANTIC_BIND_THRESHOLD,
        )
        .await
        {
            Ok(bindings) => Some(prism_ingest::property_resolution::binding_report(&bindings)),
            Err(error) => Some(json!({
                "error": format!(
                    "property resolution failed after the writes (facts are unaffected): {error:#}"
                ),
            })),
        }
    };

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
                paper.url,
                &base_prov.tenant,
                now_secs,
            ));
        }
        for proposal in proposed_relations {
            queued.push(relation_proposal_queue_item(
                proposal,
                paper.url,
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

    // How many predicates landed in a loaded ontology, and how many did not.
    //
    // ANNOTATE, NEVER REFUSE. The tabular pipeline flags an unknown relation
    // through `validate_graph`'s `unknown_rel`; this path writes facts one at
    // a time and never ran that check, so a paper could contribute a whole
    // private vocabulary in silence. Measured 2026-08-27: 23 facts stored from
    // one paper, ZERO predicates ontology-bound, and nothing in the run said
    // so — it took a SQL query afterwards to find out.
    //
    // Refusing them would be the wrong cure, and this file already records
    // why: refuse-at-the-door lost the fact entirely, and quietly normalising
    // it stored the number as if it were clean. So the facts are stored and
    // the run SAYS what it did — a number a reader can act on, at the point of
    // use, instead of a silence that reads like success.
    let bound_predicates = claims
        .iter()
        .filter(|claim| {
            let predicate = claim.predicate.trim();
            predicate.starts_with("http://")
                || predicate.starts_with("https://")
                || ontologies
                    .all()
                    .iter()
                    .any(|ontology| ontology.relation_for_label(predicate).is_some())
        })
        .count();
    let unbound_predicates = claims.len().saturating_sub(bound_predicates);

    Ok(json!({
        "written": written,
        "rejected": rejected.len(),
        "rejections": rejected,
        "citation_warnings": citation_warnings,
        "ontology_proposals": {
            "enqueued": proposals_enqueued,
            "suppressed": proposals_suppressed,
        },
        "property_resolution": property_resolution,
        // Visible in the result, so "nothing bound" cannot look like success.
        "predicate_binding": {
            "bound": bound_predicates,
            "unbound": unbound_predicates,
            "note": "unbound predicates are STORED, not dropped — a name only \
                     this paper uses cannot corroborate with any other paper",
        },
        "store": db_path.display().to_string(),
        "tenant": base_prov.tenant,
        "semantic_validation": semantic.report,
    }))
}

/// The free-text property names the resolution ladder should bind for one
/// batch of prepared claims, each with the citation of the fact that
/// carried it.
///
/// Selection is SHAPE, never vocabulary: a claim whose fact carries a finite
/// value AND a unit term states a measurement (the same grounding rule the
/// tabular mapper binds `kind` on), and its property is named by
/// - the PREDICATE, when the model did not bind it to an ontology property
///   and no loaded ontology declares it as a relation label ("crack-growth
///   resistance" — the live free-text shape), and
/// - the OBJECT, when the model did not classify it ("has_measurement" →
///   "yield strength" — the property rides the object of a relation-shaped
///   predicate).
///
/// Both candidates pass through [`is_property_name_shaped`], which drops
/// strings that merely restate the value ("950 MPa").
fn property_terms_for_claims(
    ontologies: &prism_ingest::ontologies::OntologySet,
    prepared: &[(
        &prism_retrieval::claims::ExtractedClaim,
        prism_provenance::MaterialFact,
        Option<prism_provenance::SourceCitation>,
        prism_provenance::LocalProvenance,
    )],
) -> Vec<prism_ingest::property_resolution::PropertyTerm> {
    use prism_ingest::paper_agent::PaperCitation;

    let mut terms = Vec::new();
    for (claim, fact, _, _) in prepared {
        let citation = match (
            claim.provenance.source_revision_id.as_deref(),
            claim.provenance.line_start,
            claim.provenance.line_end,
            claim.provenance.quote.as_deref(),
        ) {
            (Some(revision), Some(start), Some(end), Some(quote)) if start >= 1 && end >= start => {
                usize::try_from(start)
                    .ok()
                    .zip(usize::try_from(end).ok())
                    .map(|(from_line, to_line)| PaperCitation {
                        source_revision_id: revision.to_string(),
                        from_line,
                        to_line,
                        quoted_text: quote.to_string(),
                    })
            }
            _ => None,
        };
        prism_ingest::property_resolution::property_terms_for_fact(
            ontologies,
            fact,
            claim.ontology.predicate_iri.as_deref(),
            claim.ontology.object_class_iri.as_deref(),
            citation,
            &mut terms,
        );
    }
    terms
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

/// The reader's workspace, assembled from a parsed full text.
///
/// Selected blocks are joined into ONE newline-separated text — the only
/// surface `search_paper`/`read_paper` serve and the only coordinate system
/// citations use. Each selected block's one-based line range in that text is
/// returned beside its locator, so a fact citing table lines is stamped with
/// the table's locator (kind, label, section path) in its provenance —
/// table values cite their table exactly the way prose values cite their
/// section.
///
/// Abstract included deliberately. It was excluded, and the abstract is
/// where a paper states its headline quantities in their most
/// self-contained form — the exact shape an extractor wants. The
/// materials-IE literature is largely BUILT on abstracts (Dagdelen et al.,
/// Nat. Commun. 2024), so dropping it discarded the highest-density section.
///
/// Duplication with the body is not a cost here: all windows of one document
/// write under one provenance activity, so a fact asserted twice counts once
/// and simply gains a corroboration.
///
/// Title is NOT added: it already reaches the model as the separate `title`
/// argument to the extractor, and repeating it inside the body text would
/// only spend context.
fn assemble_paper_workspace(
    fulltext: &prism_retrieval::Fulltext,
    max_blocks: usize,
) -> (String, Vec<(usize, usize, &prism_retrieval::Locator)>) {
    use prism_retrieval::BlockKind;
    let selected_blocks = fulltext
        .blocks
        .iter()
        .filter(|block| {
            matches!(
                block.locator.kind,
                BlockKind::Abstract | BlockKind::Body | BlockKind::Table | BlockKind::Caption
            )
        })
        .take(if max_blocks == 0 {
            usize::MAX
        } else {
            max_blocks
        });
    let mut paper_text = String::new();
    let mut located_lines = Vec::new();
    for block in selected_blocks {
        if !paper_text.is_empty() {
            paper_text.push('\n');
        }
        let line_start = paper_text.bytes().filter(|byte| *byte == b'\n').count() + 1;
        paper_text.push_str(&block.text);
        let line_end = line_start + block.text.lines().count().max(1) - 1;
        located_lines.push((line_start, line_end, &block.locator));
    }
    (paper_text, located_lines)
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

    /// A REAL published JATS document (PMC13302085, MDPI *Materials*, CC BY
    /// 4.0 — the license statement travels inside the file). Every one of
    /// its five tables lives in `<floats-group>`/`<app-group>`, the shape
    /// that used to reach the reader with NO tables at all.
    const REAL_JATS: &str = include_str!("../../retrieval/tests/fixtures/PMC13302085.nxml");

    /// The selector must be REACHED by the path that serves papers, not
    /// merely exist.
    ///
    /// Built-and-unwired is this codebase's most common defect and it was this
    /// feature's first state: `selector.rs` was complete and tested while
    /// `papers search` — the path `prior_art_search` actually invokes — never
    /// constructed it, so every search would have run the embedding filter
    /// alone and reported nothing about a judge at all.
    ///
    /// Driven through the production constructor over an engine with no
    /// sources, so there is no network and nothing the test built itself
    /// stands in for the thing under test. Which status comes back depends on
    /// whether THIS machine has an LLM configured — `build_llm_config` reads
    /// the global `~/.prism` config, not only the project — so the assertion
    /// is on what must hold either way: the stage ran and reported.
    #[tokio::test]
    async fn the_search_path_reaches_the_selector_stage() {
        let engine = with_relevance_stages(
            build_engine(Vec::new(), &None, true),
            std::path::Path::new("/nonexistent-project-root"),
        );

        let outcome = engine.search("pfas free elastomer seals", 1).await;

        let selector =
            outcome.relevance.selector.as_ref().expect(
                "the search path must reach the selector stage; None means it was never wired",
            );
        assert_eq!(
            selector.dropped, 0,
            "nothing was retrieved, so nothing can be dropped"
        );
    }

    /// With no judge the stage is honestly UNAVAILABLE and keeps everything.
    /// A search that could not be judged must never read like one that was
    /// judged and found everything relevant — that is the difference between
    /// an empty result and an unasked question.
    #[tokio::test]
    async fn a_search_without_a_judge_says_so_rather_than_passing_silently() {
        let engine = build_engine(Vec::new(), &None, true)
            .with_relevance_policy(RelevancePolicy::default())
            .with_selector_policy(prism_retrieval::SelectorPolicy::default())
            .with_selector(None);

        let outcome = engine.search("pfas free elastomer seals", 1).await;

        let selector = outcome
            .relevance
            .selector
            .as_ref()
            .expect("an unavailable judge still reports");
        assert_eq!(
            selector.status,
            prism_retrieval::SelectorStatus::Unavailable
        );
        assert_eq!(selector.dropped, 0, "an unavailable judge drops nothing");
    }

    /// JATS declares the title and abstract as TAGS, so neither word appears
    /// in the prose — the text scan alone finds nothing on this path, and this
    /// is the path six of the seven papers in the 2026-08-28 run took, every
    /// one of them storing an empty title.
    ///
    /// Driven through the real parser on the real JATS fixture rather than a
    /// `Fulltext` assembled here: a struct built by the test proves only that
    /// the test can build a struct.
    #[test]
    fn title_and_abstract_come_from_the_labelled_blocks_on_the_jats_path() {
        let fulltext = prism_retrieval::fulltext::parse_jats(REAL_JATS.as_bytes()).unwrap();

        let title = title_from_document(&fulltext).expect("JATS declares <article-title>");
        assert!(!title.trim().is_empty());
        assert!(
            !title.contains('<'),
            "the block text is parsed, not raw markup: {title:?}"
        );

        let abstract_text =
            labelled_block(&fulltext, BlockKind::Abstract).expect("JATS declares <abstract>");
        assert!(!abstract_text.trim().is_empty());
        assert_ne!(abstract_text, title, "these are different blocks");

        // The text scan is the PDF fallback and cannot serve this path: the
        // word "Abstract" is a tag here, never prose.
        assert!(
            !fulltext.plain_text.to_lowercase().starts_with("abstract"),
            "if the heading were in the prose this fix would be unnecessary"
        );
    }

    /// A block that swallowed the body is not a title. Refused, not truncated:
    /// a truncated title is a different, wrong title, and a wrong title is
    /// worse than an honest absence.
    #[test]
    fn an_implausibly_long_title_block_is_refused_not_truncated() {
        let long = "word ".repeat(200);
        let jats = format!(
            "<article><front><article-meta><title-group><article-title>{long}\
             </article-title></title-group></article-meta></front></article>"
        );
        let fulltext = prism_retrieval::fulltext::parse_jats(jats.as_bytes()).unwrap();
        assert!(
            labelled_block(&fulltext, BlockKind::Title).is_some(),
            "the block is there"
        );
        assert_eq!(
            title_from_document(&fulltext),
            None,
            "but it is not a title, and half of it is not one either"
        );
    }

    /// End to end from real JATS to the reader's workspace: the exact text
    /// `search_paper`/`read_paper` serve must contain the table BY NAME and
    /// its rows WITH cell boundaries, and the row's line must map to a
    /// Table locator carrying the label — that locator is what
    /// `claim_from_fact` stamps into a claim's provenance, so a value from
    /// a table cites its table the way prose values cite their section.
    #[test]
    fn workspace_serves_tables_by_name_and_maps_their_lines_to_table_locators() {
        let fulltext = prism_retrieval::fulltext::parse_jats(REAL_JATS.as_bytes()).unwrap();
        let (paper_text, located_lines) = assemble_paper_workspace(&fulltext, 0);

        // The heading a reader searches for ("Table A1") is in the text the
        // tools serve, with the caption that carries the table's meaning.
        assert!(
            paper_text
                .contains("Table A1. Literature Data Used for LOF Process-Window Validation."),
            "the table heading never reached the reader's workspace"
        );
        // Figure captions from <floats-group> reach the reader too.
        assert!(
            paper_text.contains("Figure 1. Illustration of the melt pool geometry"),
            "the figure caption never reached the reader's workspace"
        );

        // A data row arrives with its cell boundaries, transcribed from the
        // XML source by hand (not from parser output).
        let row = "1 | 99.9 | No LOF | Malý et al., 2022 [[53] ] | 400 | 500 | 60 | 30";
        let row_line = paper_text
            .lines()
            .position(|line| line == row)
            .map(|index| index + 1)
            .expect("the delimited data row must be in the workspace text");

        // The line a citation of that row would carry maps to the table's
        // locator: kind Table, label "Table A1".
        let locator = located_lines
            .iter()
            .find(|(start, end, _)| row_line >= *start && row_line <= *end)
            .map(|(_, _, locator)| *locator)
            .expect("the row's line must fall inside a located block");
        assert_eq!(locator.kind, prism_retrieval::BlockKind::Table);
        assert_eq!(locator.label.as_deref(), Some("Table A1"));
    }

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

    /// A run that stores a private vocabulary must SAY so. Measured
    /// 2026-08-27: 23 facts from one paper, zero predicates ontology-bound,
    /// and nothing in the run's own output mentioned it — the defect was only
    /// findable by querying SQL afterwards, so the run read like a success.
    ///
    /// Reported, never refused. This file already records why refusing is the
    /// wrong cure: refuse-at-the-door lost the fact, and quietly normalising
    /// stored the number as if it were clean.
    #[tokio::test]
    async fn a_run_reports_how_many_predicates_landed_in_an_ontology() {
        let db = scratch_db();
        let ontologies = prism_ingest::ontologies::loaded(None).expect("default ontology");

        let mut invented = claim("UTS", Some("QUDT:MegaPA"), None);
        invented.predicate = "pfasLayerReductionFactor".into();
        invented.provenance.source_revision_id = Some("a".repeat(64));
        invented.provenance.line_start = Some(1);
        invented.provenance.line_end = Some(2);
        invented.provenance.quote = Some("UTS was 1140 MPa".into());

        let out = store_claims(
            &[invented],
            ReadPaper {
                url: "https://example.org/paper",
                title: "A title",
                abstract_text: "An abstract kept verbatim.",
                write_up: None,
            },
            "test-model",
            &db,
            &ontologies,
            &[],
            &[],
        )
        .await
        .expect("store");

        let binding = &out["predicate_binding"];
        assert_eq!(
            binding["unbound"], 1,
            "an invented predicate must be counted, not passed over: {out}"
        );
        assert_eq!(binding["bound"], 0);
    }

    /// The paper's own abstract is kept when the document declares one, and
    /// NOT invented when it does not.
    ///
    /// Measured 2026-08-28: a run stored its write-up with `abstract kept: 0
    /// chars`, because the field was read from a search record and
    /// `papers_ingest` is given a bare URL.
    #[test]
    fn the_abstract_is_taken_from_the_paper_or_left_absent() {
        let paper = "Some Title\nA. Author\n\nAbstract\nWe measured the service \
                     temperature of FFKM seals.\n\n1. Introduction\nSeals matter.";
        let found = abstract_from_document(paper).expect("the paper declares one");
        assert!(
            found.starts_with("We measured"),
            "starts at the prose: {found:?}"
        );
        assert!(
            !found.contains("Introduction"),
            "stops where the next section begins: {found:?}"
        );
        assert!(
            !found.contains("A. Author"),
            "and never reaches back into the front matter"
        );

        // No abstract declared: absence, not the first paragraph relabelled.
        assert_eq!(
            abstract_from_document("Title\n\n1. Introduction\nStraight in."),
            None
        );
        // A heading with nothing under it is not an abstract.
        assert_eq!(abstract_from_document("Abstract\n\n"), None);
    }

    /// Keywords end an abstract too — some papers put them before the
    /// introduction, and swallowing them would file a keyword list as prose.
    #[test]
    fn a_keyword_block_ends_the_abstract() {
        let paper = "Abstract: We measured FFKM.\nKeywords: PFAS, seals, FFKM\n\n                     1. Introduction";
        let found = abstract_from_document(paper).expect("declared");
        assert_eq!(found, "We measured FFKM.");
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
            ReadPaper {
                url: "https://example.org/paper",
                title: "A title",
                abstract_text: "An abstract kept verbatim.",
                write_up: None,
            },
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

    /// The resolution ladder runs AFTER the writes, over the union of
    /// loaded ontologies, for each measured claim's free-text property
    /// names: here the object "metallic material" binds (rung 2) to the
    /// bundled EMMO class and stamps the property node the write minted,
    /// while the free-text predicate "has_measurement" stays free text —
    /// recorded unbound AND queued as a cited class proposal — and the fact
    /// itself is untouched by both outcomes.
    #[tokio::test]
    async fn the_resolution_ladder_binds_measured_property_terms_after_the_writes() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };
        let db = scratch_db();
        let ontologies = prism_ingest::ontologies::loaded(None).expect("default ontology");
        let mut cited = claim("metallic material", Some("QUDT:MegaPA"), None);
        cited.provenance.source_revision_id = Some("b".repeat(64));
        cited.provenance.line_start = Some(3);
        cited.provenance.line_end = Some(4);
        cited.provenance.quote = Some("the metallic material reached 1140 MPa".into());

        // A claim with NO measured value: not measurement-shaped, so neither
        // its predicate nor its object may become a property term.
        let mut unmeasured = claim("turbine blades", None, None);
        unmeasured.predicate = "used_in".into();
        unmeasured.value = None;
        unmeasured.kind = None;

        // The live free-text shape: the PREDICATE names the property and the
        // object merely restates the value — the value-string must not
        // become a class proposal.
        let mut free_text = claim("950 MPa", Some("QUDT:MegaPA"), None);
        free_text.predicate = "crack-growth resistance".into();
        free_text.value = Some(950.0);
        free_text.provenance.source_revision_id = Some("c".repeat(64));
        free_text.provenance.line_start = Some(9);
        free_text.provenance.line_end = Some(9);
        free_text.provenance.quote = Some("crack-growth resistance of 950 MPa".into());

        let out = store_claims(
            &[cited, unmeasured, free_text],
            ReadPaper {
                url: "https://example.org/paper",
                title: "A title",
                abstract_text: "An abstract kept verbatim.",
                write_up: None,
            },
            "test-model",
            &db,
            &ontologies,
            &[],
            &[],
        )
        .await
        .expect("store");
        assert_eq!(out["written"], 3, "{out}");

        let resolution = &out["property_resolution"];
        assert_eq!(resolution["terms"], 3, "{out}");
        assert_eq!(resolution["normalized"], 1, "{out}");
        assert_eq!(resolution["proposed"], 2, "{out}");
        assert_eq!(resolution["proposals_enqueued"], 2, "{out}");
        assert_eq!(
            resolution["entities_stamped"], 1,
            "the object node the write minted was stamped: {out}"
        );

        let store = prism_provenance::ProvenanceStore::open(&db).await.unwrap();

        // The bound term's durable record: rung 2, EMMO class, no score.
        let bound = store
            .term_binding("local", "metallic material")
            .await
            .unwrap()
            .expect("binding row for the object term");
        assert_eq!(bound.rung, prism_provenance::TERM_BINDING_RUNG_NORMALIZED);
        assert_eq!(bound.ontology_id.as_deref(), Some("emmo"));
        assert!(
            bound
                .class_iri
                .as_deref()
                .is_some_and(|iri| iri.starts_with("https://w3id.org/emmo#")),
            "{bound:?}"
        );
        assert_eq!(bound.score, None);

        // The free-text predicate's record: unbound, pending proposal id.
        let unbound = store
            .term_binding("local", "has_measurement")
            .await
            .unwrap()
            .expect("binding row for the free-text predicate");
        assert_eq!(unbound.rung, prism_provenance::TERM_BINDING_RUNG_PROPOSED);
        assert_eq!(unbound.class_iri, None);
        let item_id = unbound
            .proposal_item_id
            .as_deref()
            .expect("cited rung-4 term queued a proposal");

        // The proposal sits in the human governance queue with the claim's
        // own citation attached as its sighting.
        let pending = store.pending_ontology_proposals(10).await.unwrap();
        let (item, _) = pending
            .iter()
            .find(|(item, _)| item.item_id == item_id)
            .expect("the ladder's proposal is pending");
        assert_eq!(item.kind, "class");
        assert_eq!(item.label, "has_measurement");

        // The free-text predicate of the third claim is a term too, and its
        // proposal is queued; the value-string object is NOT a term.
        assert!(
            store
                .term_binding("local", "crack-growth resistance")
                .await
                .unwrap()
                .is_some_and(|row| row.proposal_item_id.is_some()),
        );
        for never_a_term in ["used_in", "turbine blades", "950 mpa"] {
            assert!(
                store
                    .term_binding("local", never_a_term)
                    .await
                    .unwrap()
                    .is_none(),
                "{never_a_term:?} must not enter the ladder"
            );
        }

        // And no rung discarded a fact — all three claims are in the graph.
        let facts = store
            .recall_with_context_filtered(
                "Ti-6Al-4V",
                &["local"],
                10,
                prism_provenance::VerificationFilter::Any,
            )
            .await
            .unwrap();
        assert_eq!(facts.len(), 3, "every fact survived resolution: {facts:?}");
        for object in ["metallic material", "turbine blades", "950 MPa"] {
            assert!(
                facts.iter().any(|fact| fact.object == object),
                "fact with object {object:?} survived: {facts:?}"
            );
        }
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
            ReadPaper {
                url: "https://example.org/legacy-paper",
                title: "A title",
                abstract_text: "An abstract kept verbatim.",
                write_up: None,
            },
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
            ReadPaper {
                url: "https://example.org/paper",
                title: "A title",
                abstract_text: "An abstract kept verbatim.",
                write_up: None,
            },
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
            ReadPaper {
                url: "https://example.org/paper",
                title: "A title",
                abstract_text: "An abstract kept verbatim.",
                write_up: None,
            },
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
            ReadPaper {
                url: "https://example.org/paper",
                title: "A title",
                abstract_text: "An abstract kept verbatim.",
                write_up: None,
            },
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
            ReadPaper {
                url: "https://example.org/paper",
                title: "A title",
                abstract_text: "An abstract kept verbatim.",
                write_up: None,
            },
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
