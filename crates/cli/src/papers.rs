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
                        .with_context(|| "LLM fact extraction failed")?
                        .facts;
                for fact in facts {
                    let claim =
                        claim_from_fact(fact, &document_id, &document_url, &source, &block.locator);
                    match prism_retrieval::claims::validate_and_stamp(claim) {
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
/// Evidence is stamped by `validate_and_stamp` (ceiling: research).
fn claim_from_fact(
    fact: prism_provenance::MaterialFact,
    document_id: &str,
    document_url: &str,
    source: &str,
    locator: &prism_retrieval::Locator,
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
            quote: None,
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
