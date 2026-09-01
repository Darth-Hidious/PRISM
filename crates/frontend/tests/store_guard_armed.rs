// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Tripwire: this crate's tests build `prism-agent` WITH the live-store guard.
//!
//! The guard is a cargo feature armed by the `prism-agent` dev-dependency in
//! this crate's Cargo.toml. Cargo unifies features per build, so the agent's
//! own self dev-dependency arms it only when the agent crate is the one under
//! test; `cargo test -p prism-frontend` builds an agent that resolves the default
//! store path silently unless THIS crate arms it too. If someone drops the
//! feature from the dev-dependency, this test is what goes red.

#[test]
#[allow(clippy::assertions_on_constants)]
fn the_live_store_guard_is_armed_for_this_crates_tests() {
    assert!(
        prism_agent::TEST_GUARD_ARMED,
        "prism-frontend tests are building prism-agent WITHOUT `test-guard`: tests here \
         can open the user's live ~/.prism/provenance.db. Restore `features = \
         [\"test-guard\"]` on the prism-agent dev-dependency in crates/frontend/Cargo.toml."
    );
}
