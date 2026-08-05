//! `prism papers` — the fast literature retrieval engine's CLI surface.
//!
//! Every subcommand prints one JSON document to stdout so the harness, MCP
//! clients, and shell pipelines all consume the same shape. Errors are
//! values in that JSON where possible; a hard failure exits non-zero with
//! the message on stderr.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use prism_retrieval::{EngineConfig, Paper, RetrievalEngine, SourceId, SweepPlan, sweep};
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
    },
}

fn parse_sources(list: &Option<String>) -> Result<Vec<SourceId>> {
    let Some(raw) = list else {
        return Ok(prism_retrieval::all_sources());
    };
    let mut out = Vec::new();
    for name in raw.split(',') {
        if name.trim().is_empty() {
            continue;
        }
        match SourceId::from_name(name) {
            Some(id) => out.push(id),
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

fn build_engine(
    sources: Vec<SourceId>,
    mailto: &Option<String>,
    no_cache: bool,
) -> RetrievalEngine {
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
            let engine = build_engine(source_ids, &mailto, no_cache);
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
            let engine = build_engine(vec![SourceId::Arxiv], &None, false);
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
        } => {
            let paper = paper_for_fulltext(&pmc, &url, &format)?;
            let engine = build_engine(vec![SourceId::Arxiv], &None, false);
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
                let facts =
                    prism_ingest::text_extract::extract_facts_from_text(&llm, &title, &block.text)
                        .await
                        .with_context(|| "LLM fact extraction failed")?;
                for fact in facts {
                    // Containment: find the verbatim span of THIS block that
                    // supports the fact. Facts with no supporting span cannot
                    // become claims — stamping them would record provenance a
                    // document never gave (extractor prompt examples included).
                    let quote = prism_retrieval::claims::supporting_quote(
                        &fact.subject,
                        &fact.object,
                        fact.value,
                        &block.text,
                    );
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
                        Err(reason) => rejected.push(json!({
                            "reason": reason,
                        })),
                    }
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "claims": claims,
                    "rejected": rejected,
                    "status": "ok",
                    "document": fulltext.source_url,
                    "blocks_extracted": blocks_extracted,
                    "max_blocks": max_blocks,
                }))?
            );
        }
    }
    Ok(())
}

/// TCP-probe an LLM base URL with a hard 3-second budget.
fn probe_endpoint(base_url: &str) -> Result<(), String> {
    use std::net::ToSocketAddrs;
    let without_scheme = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url);
    let host_port = without_scheme.split('/').next().unwrap_or("");
    let host_port = match host_port.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => host_port.to_string(),
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
