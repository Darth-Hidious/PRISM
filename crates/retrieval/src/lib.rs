//! # prism-retrieval
//!
//! Fast, polite, resumable literature retrieval aimed at EMMO-typed
//! ingestion. Federates across machine-readable APIs (arXiv, OpenAlex,
//! Crossref, PubMed, Semantic Scholar, Europe PMC preprints, ChemRxiv,
//! DOAJ) — never a JavaScript-rendered page.
//!
//! Design priorities, in order:
//! 1. Concurrent, polite, resumable fetch (resumability beats raw speed).
//! 2. Fast full-text extraction with claim locators (JATS preferred, PDF
//!    fallback).
//! 3. Structured output for EMMO ingestion: claims carry units, conditions,
//!    provenance, and an evidence class that literature can never promote.
//!
//! Papers are metadata records; claims are extracted separately and stamped
//! through `prism_provenance::evidence_for_result` (ceiling: `research`).

pub mod cache;
pub mod claims;
pub mod engine;
pub mod fulltext;
pub mod http;
pub mod model;
pub mod ratelimit;
pub mod sources;
pub mod sweep;

pub use engine::{EngineConfig, RetrievalEngine, default_cache_dir};
pub use fulltext::{BlockKind, Fulltext, Locator, TextBlock};
pub use model::{FulltextFormat, Paper, SearchOutcome, SourcePage, SourceStatus};
pub use sources::{FetchCtx, SourceId, all_sources};
pub use sweep::{SweepOutcome, SweepPlan, SweepState};
