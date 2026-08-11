// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
pub mod agent_loop;
pub mod capability;
pub mod command_tools;
pub mod commands;
pub mod embeddings;
pub mod execution_contract;
pub mod hooks;
pub mod mcp;
pub mod meta_tools;
pub mod models;
pub mod node_supervisor;
pub mod notebook;
pub mod permissions;
pub mod prompt_profile;
pub mod prompts;
pub mod protocol;
pub mod reprompt;
pub mod scratchpad;
pub mod service;
pub mod session;
mod session_index;
pub mod skills;
pub mod subagent;
pub mod task;
#[cfg(any(test, feature = "test-guard"))]
pub mod testsupport;
pub mod tool_catalog;
pub mod tool_result;
pub mod transcript;
pub mod types;

/// Unit-test binary: point provenance writes at a scratch store BEFORE any
/// test (or any thread) starts. Integration binaries do the same through
/// `tests/common/mod.rs`. See `testsupport` for the contract.
// SAFETY (ctor): runs pre-main; the body only touches the environment, which
// is exactly what must happen before any thread can exist.
#[cfg(test)]
#[ctor::ctor(unsafe)]
fn isolate_provenance_store_for_unit_tests() {
    testsupport::isolate_provenance_store_pre_main();
}
