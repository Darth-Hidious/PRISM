//! Core data model for the retrieval engine.
//!
//! Papers are metadata records with provenance back to their source. They are
//! NOT claims: no evidence class attaches to a paper record. Evidence classes
//! attach to extracted claims (see `claims.rs`) via
//! `prism_provenance::evidence_for_result`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How a full text is available, when known. JATS XML is preferred over PDF
/// for extraction because it preserves structure (sections, tables, captions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FulltextFormat {
    Jats,
    Pdf,
}

/// One retrieved paper with enough provenance to locate any claim in it later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paper {
    /// Source that served this record (e.g. "arxiv", "openalex").
    pub source: String,
    /// The source's own identifier for the record.
    pub source_id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    /// Raw publication date string as the source gave it.
    pub published: Option<String>,
    /// Lowercased, trimmed DOI when the source reports one.
    pub doi: Option<String>,
    /// Other identifiers keyed by kind: "arxiv", "pmc", "pmid", "openalex", ...
    pub external_ids: BTreeMap<String, String>,
    pub abstract_text: Option<String>,
    /// Canonical landing page.
    pub url: String,
    /// Direct full-text location when the source advertises one.
    pub fulltext_url: Option<String>,
    pub fulltext_format: Option<FulltextFormat>,
    pub journal: Option<String>,
}

impl Paper {
    /// Exact-identifier dedup key. DOI beats arXiv ID beats PMC ID; otherwise
    /// the record is kept per-source. Fuzzy title matching is deliberately NOT
    /// done here: merging records on guessed identity is how wrong data is
    /// manufactured.
    pub fn dedup_key(&self) -> String {
        if let Some(doi) = &self.doi {
            return format!("doi:{doi}");
        }
        if let Some(arxiv) = self.external_ids.get("arxiv") {
            return format!("arxiv:{arxiv}");
        }
        if let Some(pmc) = self.external_ids.get("pmc") {
            return format!("pmc:{pmc}");
        }
        format!("{}:{}", self.source, self.source_id)
    }

    /// Merge another record for the same work into this one, filling gaps
    /// without overwriting present values. Keeps the record with a full-text
    /// location as the survivor's full text when this one lacks it.
    pub fn absorb(&mut self, other: &Paper) {
        if self.year.is_none() {
            self.year = other.year;
        }
        if self.published.is_none() {
            self.published.clone_from(&other.published);
        }
        if self.doi.is_none() {
            self.doi.clone_from(&other.doi);
        }
        if self.abstract_text.is_none() {
            self.abstract_text.clone_from(&other.abstract_text);
        }
        if self.fulltext_url.is_none() {
            self.fulltext_url.clone_from(&other.fulltext_url);
            self.fulltext_format = other.fulltext_format;
        }
        if self.journal.is_none() {
            self.journal.clone_from(&other.journal);
        }
        if self.authors.is_empty() {
            self.authors.clone_from(&other.authors);
        }
        for (kind, id) in &other.external_ids {
            self.external_ids
                .entry(kind.clone())
                .or_insert_with(|| id.clone());
        }
    }
}

/// One fetched page with completeness accounting.
///
/// `raw_count` is how many records the SERVER returned on this page, before
/// any parser skip (title-less entries, missing DOI, missing `bibjson`).
/// Pagination termination must gate on `raw_count`, never on `papers.len()`:
/// one skipped record on a full page otherwise ends the chain while the
/// source is reported `ok`. `available` is the server's own total for the
/// whole query when it reports one — `None` means the source did not say,
/// never a guess.
#[derive(Debug, Clone, Default)]
pub struct SourcePage {
    pub papers: Vec<Paper>,
    pub raw_count: usize,
    pub available: Option<u64>,
}

/// Honest per-source outcome. A source that failed says so; it is never
/// silently dropped from the outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceStatus {
    pub source: String,
    /// "ok", "timeout", "error". ("cache" was documented here but nothing
    /// ever produced it — cache service is reported via `cache_hit`.)
    pub status: String,
    pub count: usize,
    /// Server-reported total matches for the query, when the source reports
    /// one. `count < available` means the caller got LESS than what exists —
    /// visible here instead of silently absorbed into an `ok`.
    #[serde(default)]
    pub available: Option<u64>,
    pub latency_ms: f64,
    pub cache_hit: bool,
    pub error: Option<String>,
    /// Typed kind when the SOURCE failed (`status` "error"/"timeout") —
    /// retry, backoff and reporting can branch on it. `None` on success and
    /// for configuration failures (an id with no registered adapter), which
    /// are not source failures. The `status` string and `error` text are
    /// unchanged; this field only adds information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<crate::sources::source::FailureKind>,
}

/// Result of one federated search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchOutcome {
    pub papers: Vec<Paper>,
    pub duplicates_merged: usize,
    pub source_status: Vec<SourceStatus>,
    /// What semantic relevance checking did (or why it did not run). A
    /// default keeps older serialized outcomes readable while making every
    /// new search explicit about whether papers were returned unfiltered.
    #[serde(default)]
    pub relevance: crate::relevance::RelevanceReport,
    pub elapsed_ms: f64,
}
