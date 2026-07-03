// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Process-wide embedding backend — lazy, shared, and strictly optional.
//!
//! One backend per process (native model init costs seconds and ~100 MB of
//! RAM; the first ever init downloads ~90 MB). Initialization happens on
//! first use, inside `spawn_blocking`, from background provenance tasks —
//! never on the turn path and never at startup. If no backend can be built
//! (config `off`, offline first run, bad openai config) everything degrades
//! to the keyword-only paths that existed before.

use std::sync::{Arc, OnceLock};

use prism_embed::EmbedBackend;
use prism_provenance::{ProvenanceRecord, ProvenanceStore};

static BACKEND: OnceLock<Option<Arc<dyn EmbedBackend>>> = OnceLock::new();

/// The backend if selection + init has already completed (successfully or
/// not). Never blocks, never downloads — `None` also while a first init is
/// still in flight. Use from latency-sensitive paths like `recall`.
pub fn backend_if_ready() -> Option<Arc<dyn EmbedBackend>> {
    BACKEND.get().cloned().flatten()
}

/// Get-or-init the process-wide backend. The first call may download the
/// native model, so the init runs on the blocking pool; concurrent callers
/// coalesce on the same `OnceLock`. Returns `None` when embedding is
/// disabled or unavailable.
pub async fn backend() -> Option<Arc<dyn EmbedBackend>> {
    if let Some(b) = BACKEND.get() {
        return b.clone();
    }
    tokio::task::spawn_blocking(|| {
        BACKEND
            .get_or_init(|| prism_embed::from_config().map(Arc::from))
            .clone()
    })
    .await
    .unwrap_or_default()
}

/// Embed a freshly written provenance record and store the vector.
/// Fire-and-forget: called from already-spawned provenance tasks, errors
/// are logged at debug and dropped, the ledger row is never affected.
pub async fn embed_record(store: &ProvenanceStore, record: &ProvenanceRecord) {
    let Some(backend) = backend().await else {
        return;
    };
    let text = prism_provenance::embedding_text(record);
    if let Err(e) = store
        .embed_and_store(&record.id, &text, backend.as_ref())
        .await
    {
        tracing::debug!("provenance embedding failed for {}: {e:#}", record.id);
    }
}
