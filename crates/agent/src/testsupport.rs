// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Test-only support. Compiled solely under `cfg(test)` or the `test-guard`
//! feature (which the self dev-dependency enables for every test target of
//! this crate) — never part of a production build.

/// Point every provenance write of this test process at a scratch store.
///
/// MUST run pre-main (from a `#[ctor::ctor]` constructor): `std::env::set_var`
/// is unsound once threads exist, and the resolver may be called from any
/// tokio worker thread. The lib test binary registers the constructor in
/// `lib.rs`; each integration test binary registers it by declaring
/// `mod common;` (see `tests/common/mod.rs`).
///
/// Without this, any test that reaches [`crate::hooks::provenance_db_path`]
/// would resolve the user's LIVE `~/.prism/provenance.db` — the `test-guard`
/// build aborts on that instead of writing (which is exactly how the defect
/// this file exists for went unnoticed: fire-and-forget writes into the real
/// store, from green tests).
///
/// Respected escape hatches:
/// - `PRISM_PROVENANCE_DB` already set — the caller (CI, a wrapper script, a
///   subprocess test) has chosen the store; keep it.
/// - `PRISM_TEST_NO_STORE_ISOLATION=1` — deliberately run WITHOUT isolation
///   so the guard's abort path can be exercised (the subprocess guard test
///   in `hooks.rs` and manual mutation checks use this).
pub fn isolate_provenance_store_pre_main() {
    if std::env::var_os("PRISM_TEST_NO_STORE_ISOLATION").is_some() {
        return;
    }
    if std::env::var_os("PRISM_PROVENANCE_DB").is_some() {
        return;
    }
    let path = std::env::temp_dir().join(format!(
        "prism-agent-test-provenance-{}.db",
        std::process::id()
    ));
    // SAFETY: called from a pre-main constructor; no other thread exists yet.
    unsafe { std::env::set_var("PRISM_PROVENANCE_DB", &path) };
}
