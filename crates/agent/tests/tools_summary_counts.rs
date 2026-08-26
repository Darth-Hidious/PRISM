// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! The `/tools` summary must never print numbers that do not add up.
//!
//! Measured on a real session before the fix: the summary printed the access
//! breakdown (54 + 83 + 25 = 162 loaded, which genuinely partitions) and then
//! immediately printed "approval-required 56 / auto-approved now 54 /
//! blocked now 0" under the same heading — 110, leaving 52 loaded tools
//! silently unaccounted for. Those last three are INDEPENDENT predicates:
//! `requires_approval` is a static property of a tool, `auto_approves` and
//! `blocks` are session state, and a tool can satisfy `blocks` and
//! `auto_approves` at the same time. Printed as a list under a total they read
//! as a breakdown, and a reader who trusts them is being lied to.
//!
//! These tests pin the repaired contract:
//!   1. the access group sums to the loaded total,
//!   2. the runtime group (auto-approved / will prompt / blocked) is a genuine
//!      PARTITION and sums to the loaded total, for any catalog and any
//!      permission mode, including the overlap case where a tool is both
//!      blocked by the mode and allowed by a session override,
//!   3. the static `approval-required` count is presented outside the runtime
//!      group and is never one of its terms.

/// Pre-main store isolation — every integration binary must declare this.
mod common;

use prism_agent::protocol::tools_summary_for_test;
use prism_agent::tool_catalog::ToolCatalog;
use serde_json::json;

/// Build a catalog through the production loader so `permission_mode` comes
/// from the real `get_tool_permission` table, not from a value the test chose.
fn catalog(entries: &[(&str, bool, Option<&str>)]) -> ToolCatalog {
    let tools = entries
        .iter()
        .map(|(name, requires_approval, source)| {
            json!({
                "name": name,
                "description": format!("{name} (test fixture)"),
                "input_schema": { "type": "object", "properties": {}, "additionalProperties": false },
                "requires_approval": requires_approval,
                "source": source,
            })
        })
        .collect::<Vec<_>>();
    ToolCatalog::from_tool_server_json(&json!({ "tools": tools }))
}

/// Pull the integer that follows `  <label>: ` on its own line.
///
/// Deliberately anchored on the rendered TEXT, not on the internal counters:
/// the bug was in what the human reads, so the guard reads the same thing.
fn labelled_count(summary: &str, label: &str) -> usize {
    let needle = format!("{label}:");
    let line = summary
        .lines()
        .find(|line| line.trim_start().starts_with(&needle))
        .unwrap_or_else(|| {
            panic!(
                "summary has no `{label}:` line. The tools summary must render two \
                 explicitly-labelled groups that each state what they sum to:\n  \
                 `by access (sums to N):` -> read-only / workspace-write / full-access\n  \
                 `right now (sums to N):`  -> auto-approved / will prompt / blocked\n\
                 plus `declared approval-required:` OUTSIDE both groups. Renaming or \
                 removing one of those lines is how the counts stopped adding \
                 up.\n---\n{summary}\n---"
            )
        });
    let rest = line.trim_start().trim_start_matches(&needle).trim_start();
    let digits = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("`{label}:` is not followed by a number: {line:?}"))
}

/// Pull the total a group HEADING claims to sum to, e.g. `by access (sums to 6):`.
fn claimed_total(summary: &str, heading: &str) -> usize {
    let line = summary
        .lines()
        .find(|line| line.trim_start().starts_with(heading))
        .unwrap_or_else(|| panic!("summary has no `{heading}` heading\n---\n{summary}\n---"));
    let after = line
        .split_once("sums to ")
        .unwrap_or_else(|| panic!("`{heading}` heading does not state what it sums to: {line:?}"))
        .1;
    let digits = after
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("`{heading}` heading has no total: {line:?}"))
}

/// Every scenario the guard sweeps: a catalog, a session mode, and the session
/// allow/deny overrides layered on top of it.
struct Scenario {
    what: &'static str,
    tools: ToolCatalog,
    mode: &'static str,
    allow: Vec<String>,
    deny: Vec<String>,
}

fn scenarios() -> Vec<Scenario> {
    // Names chosen so the real permission table spreads them across all three
    // access tiers: materials_search/web_search = read-only, file = workspace-
    // write, compute_submit = full-access, and an unmapped name defaults to
    // workspace-write.
    let mixed = [
        ("materials_search", false, None),
        ("web_search", false, None),
        ("show_scratchpad", false, None),
        ("file", true, None),
        ("execute_bash", true, None),
        ("compute_submit", true, None),
        ("some_plugin_tool", false, Some("mcp")),
    ];
    let read_only_only = [
        ("materials_search", false, None),
        ("web_search", false, None),
    ];
    // A read-only tool that DOES demand approval: auto-approve must not claim it.
    let approval_heavy = [
        ("materials_search", true, None),
        ("file", true, None),
        ("compute_submit", true, None),
    ];

    vec![
        Scenario {
            what: "empty catalog",
            tools: catalog(&[]),
            mode: "chat",
            allow: vec![],
            deny: vec![],
        },
        Scenario {
            what: "mixed catalog, chat mode, no overrides",
            tools: catalog(&mixed),
            mode: "chat",
            allow: vec![],
            deny: vec![],
        },
        Scenario {
            what: "mixed catalog, plan mode, no overrides",
            tools: catalog(&mixed),
            mode: "plan",
            allow: vec![],
            deny: vec![],
        },
        Scenario {
            // THE OVERLAP CASE. Plan mode denies every workspace-write and
            // full-access tool; a session `allow` override then auto-approves
            // one of them. `blocks("file")` and `auto_approves("file")` are
            // both true, so a summary that counts the two predicates
            // independently reports the same tool twice.
            what: "plan mode with an allow override on a mode-blocked tool",
            tools: catalog(&mixed),
            mode: "plan",
            allow: vec!["file".to_string(), "compute_submit".to_string()],
            deny: vec![],
        },
        Scenario {
            what: "chat mode with a deny override on an auto-approved tool",
            tools: catalog(&mixed),
            mode: "chat",
            allow: vec![],
            deny: vec!["materials_search".to_string()],
        },
        Scenario {
            what: "chat mode, allow and deny overrides on the same catalog",
            tools: catalog(&mixed),
            mode: "chat",
            allow: vec!["compute_submit".to_string()],
            deny: vec!["web_search".to_string()],
        },
        Scenario {
            what: "read-only catalog, plan mode",
            tools: catalog(&read_only_only),
            mode: "plan",
            allow: vec![],
            deny: vec![],
        },
        Scenario {
            what: "every tool declares approval-required, chat mode",
            tools: catalog(&approval_heavy),
            mode: "chat",
            allow: vec![],
            deny: vec![],
        },
        Scenario {
            what: "every tool declares approval-required, plan mode",
            tools: catalog(&approval_heavy),
            mode: "plan",
            allow: vec!["file".to_string()],
            deny: vec![],
        },
    ]
}

#[test]
fn runtime_group_is_a_partition_of_the_loaded_tools() {
    for scenario in scenarios() {
        let summary = tools_summary_for_test(
            &scenario.tools,
            scenario.mode,
            &scenario.allow,
            &scenario.deny,
        );
        let loaded = labelled_count(&summary, "loaded");
        assert_eq!(
            loaded,
            scenario.tools.len(),
            "[{}] summary reports a different `loaded` than the catalog holds\n---\n{}\n---",
            scenario.what,
            summary,
        );

        let auto_approved = labelled_count(&summary, "auto-approved");
        let will_prompt = labelled_count(&summary, "will prompt");
        let blocked = labelled_count(&summary, "blocked");
        let stated = claimed_total(&summary, "right now");

        assert_eq!(
            stated, loaded,
            "[{}] the runtime group claims to sum to {stated}, but {loaded} tools are loaded\n---\n{}\n---",
            scenario.what, summary,
        );
        assert_eq!(
            auto_approved + will_prompt + blocked,
            loaded,
            "[{}] auto-approved {auto_approved} + will prompt {will_prompt} + blocked {blocked} \
             = {}, but {loaded} tools are loaded — {} tools are unaccounted for. These three \
             MUST partition the loaded catalog: count `blocked` first, count `auto-approved` \
             only among the tools that are NOT blocked, and derive `will prompt` by \
             subtraction.\n---\n{}\n---",
            scenario.what,
            auto_approved + will_prompt + blocked,
            loaded.abs_diff(auto_approved + will_prompt + blocked),
            summary,
        );
    }
}

#[test]
fn access_group_is_a_partition_of_the_loaded_tools() {
    for scenario in scenarios() {
        let summary = tools_summary_for_test(
            &scenario.tools,
            scenario.mode,
            &scenario.allow,
            &scenario.deny,
        );
        let loaded = labelled_count(&summary, "loaded");
        let read_only = labelled_count(&summary, "read-only");
        let workspace_write = labelled_count(&summary, "workspace-write");
        let full_access = labelled_count(&summary, "full-access");
        let stated = claimed_total(&summary, "by access");

        assert_eq!(
            stated, loaded,
            "[{}] the access group claims to sum to {stated}, but {loaded} tools are loaded\n---\n{}\n---",
            scenario.what, summary,
        );
        assert_eq!(
            read_only + workspace_write + full_access,
            loaded,
            "[{}] read-only {read_only} + workspace-write {workspace_write} + full-access \
             {full_access} != loaded {loaded}\n---\n{}\n---",
            scenario.what,
            summary,
        );
    }
}

#[test]
fn declared_approval_required_is_never_a_term_of_the_runtime_group() {
    // `requires_approval` is a STATIC tool property. It is not "will prompt":
    // the mode and the session overrides decide that. This catalog makes the
    // difference visible — every tool declares approval-required, yet in chat
    // mode nothing is blocked, so the runtime group must still partition and
    // the static count must sit outside it, labelled as a tool property.
    let tools = catalog(&[
        ("materials_search", true, None),
        ("file", true, None),
        ("compute_submit", true, None),
    ]);
    let summary = tools_summary_for_test(&tools, "chat", &[], &[]);

    let declared = labelled_count(&summary, "declared approval-required");
    assert_eq!(
        declared, 3,
        "all three fixture tools declare requires_approval\n---\n{summary}\n---"
    );

    let runtime_block = summary
        .split("right now")
        .nth(1)
        .unwrap_or_else(|| panic!("summary has no `right now` group\n---\n{summary}\n---"));
    let runtime_terms = runtime_block
        .lines()
        .take_while(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !runtime_terms.contains("approval-required"),
        "the static approval-required count is inside the runtime group, where it reads \
         as one of the terms that sum to the loaded total. It is an independent predicate \
         and must be printed outside that group.\n---\n{runtime_terms}\n---"
    );

    // And the runtime group still partitions with that catalog.
    let loaded = labelled_count(&summary, "loaded");
    assert_eq!(
        labelled_count(&summary, "auto-approved")
            + labelled_count(&summary, "will prompt")
            + labelled_count(&summary, "blocked"),
        loaded,
        "runtime group does not sum to loaded\n---\n{summary}\n---"
    );
}

#[test]
fn a_tool_both_mode_blocked_and_override_allowed_is_counted_once() {
    // The concrete overlap that broke the arithmetic. Plan mode denies `file`
    // (workspace-write); a session `allow` override auto-approves it. Both
    // predicates hold. `blocked` wins — the tool cannot run — and it must
    // appear in exactly one column.
    let tools = catalog(&[
        ("materials_search", false, None),
        ("file", true, None),
        ("compute_submit", true, None),
    ]);
    let baseline = tools_summary_for_test(&tools, "plan", &[], &[]);
    let overridden = tools_summary_for_test(&tools, "plan", &["file".to_string()], &[]);

    let loaded = labelled_count(&overridden, "loaded");
    let auto_approved = labelled_count(&overridden, "auto-approved");
    let will_prompt = labelled_count(&overridden, "will prompt");
    let blocked = labelled_count(&overridden, "blocked");

    assert_eq!(
        auto_approved + will_prompt + blocked,
        loaded,
        "an allow override on a plan-mode-blocked tool made the runtime group double-count \
         it: auto-approved {auto_approved} + will prompt {will_prompt} + blocked {blocked} \
         != loaded {loaded}\n--- baseline ---\n{baseline}\n--- with override ---\n{overridden}\n---"
    );
    assert_eq!(
        blocked,
        labelled_count(&baseline, "blocked"),
        "the allow override must not change how many tools are blocked in plan mode — \
         plan mode's deny still wins\n--- baseline ---\n{baseline}\n--- with override ---\n{overridden}\n---"
    );
}
