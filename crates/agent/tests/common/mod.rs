// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Shared pre-main setup for every integration test binary in this crate.
//! Each `tests/*.rs` file MUST declare `mod common;` — a binary that skips it
//! and reaches the provenance store is aborted by the `test-guard` build of
//! `prism_agent::hooks::provenance_db_path` instead of being allowed to open
//! the user's live `~/.prism/provenance.db`.

/// Runs before `main`, before any thread exists — the only sound moment to
/// `set_var`. Points this process's provenance writes at a scratch store.
// SAFETY (ctor): pre-main; the body only touches the environment, which is
// exactly what must happen before any thread can exist.
#[ctor::ctor(unsafe)]
fn isolate_provenance_store() {
    prism_agent::testsupport::isolate_provenance_store_pre_main();
}
