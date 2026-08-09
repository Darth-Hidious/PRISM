// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! The store-isolation contract of this crate's test suite, as tests.
//!
//! Background: `cargo test -p prism-agent` used to WRITE INTO THE USER'S LIVE
//! `~/.prism/provenance.db` — run_turn's fire-and-forget provenance writes
//! resolved the default store path with no override, from green tests. These
//! tests pin the two halves of the fix: pre-main injection of a scratch store
//! (tests/common/mod.rs + the lib.rs ctor) and the `test-guard` abort in
//! `hooks::provenance_db_path` for any test process that skips injection.

/// Pre-main store isolation — every integration binary must declare this.
mod common;

/// The self dev-dependency in Cargo.toml must keep `test-guard` enabled for
/// every test build. If someone removes it, the guard silently stops
/// guarding — this test is the tripwire that makes that removal loud.
#[test]
// The assertion IS deliberately on a compile-time constant: this is a
// tripwire that must fail AS A NAMED TEST when the self dev-dependency
// stops arming `test-guard`.
#[allow(clippy::assertions_on_constants)]
fn test_guard_feature_is_active_in_test_builds() {
    assert!(
        cfg!(feature = "test-guard"),
        "prism-agent test targets are building WITHOUT the test-guard \
         feature: the live-store guard is disarmed. Restore the self \
         dev-dependency `prism-agent = {{ path = \".\", features = \
         [\"test-guard\"] }}` in crates/agent/Cargo.toml."
    );
}

/// Pre-main isolation must have injected a scratch store, the resolver must
/// honor it, and the result must never be the user's live store path.
#[test]
fn resolver_honors_the_injected_scratch_store() {
    let env = std::env::var_os("PRISM_PROVENANCE_DB")
        .expect("pre-main isolation (tests/common/mod.rs) must set PRISM_PROVENANCE_DB");
    let resolved = prism_agent::hooks::provenance_db_path();
    assert_eq!(
        resolved,
        std::path::PathBuf::from(&env),
        "provenance_db_path must resolve to the injected override"
    );
    if let Some(home) = std::env::var_os("HOME") {
        assert_ne!(
            resolved,
            std::path::Path::new(&home).join(".prism/provenance.db"),
            "a test resolver must never yield the live store path"
        );
    }
}
