// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Shared pre-main setup for every integration test binary in this crate.
//! Each `tests/*.rs` file MUST declare `mod common;` — server flows can drive
//! the real agent loop, and a binary that skips this and reaches the
//! provenance store is aborted by prism-agent's `test-guard` build instead of
//! being allowed to open the user's live `~/.prism/provenance.db`.

/// Runs before `main`, before any thread exists — the only sound moment to
/// `set_var`. Points this process's provenance writes at a scratch store.
// SAFETY (ctor): pre-main; the body only touches the environment, which is
// exactly what must happen before any thread can exist.
#[ctor::ctor(unsafe)]
fn isolate_provenance_store() {
    prism_agent::testsupport::isolate_provenance_store_pre_main();
}
