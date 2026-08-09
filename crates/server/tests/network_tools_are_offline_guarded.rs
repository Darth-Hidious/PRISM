//! Guard: a file that shells out to a network tool must consult the offline policy.
//!
//! Sibling of `no_exit_to_cli.rs`, and it exists for the same reason that one
//! does — a rule nobody can enforce by review keeps getting broken.
//!
//! PRISM shells out to `ssh`, `kubectl`, `gh`, `hf`, `ollama`, `docker`,
//! `podman` and `pip`. Each of those then talks to the network *itself*, so no
//! amount of auditing `reqwest` call sites sees them, and several carry a
//! credential: `ssh -i <key>` sends the user's private key, `gh` its OAuth
//! token, `hf` the Hugging Face token, `docker pull` a registry login. Every one
//! of these was live under `PRISM_OFFLINE=1` at some point on this branch, and
//! they were found one class at a time — sockets, then subprocesses, then a
//! vendored client that dials on its own.
//!
//! The gap this closes is regression, not discovery: an adversarial reviewer
//! pointed out that six of eight guards added in `c73c316c` had no test in
//! either direction, and proved it by disabling one and watching the suite stay
//! green at 82/82. Deleting any single guard still compiled and still passed.
//!
//! Granularity is deliberately FILE level, matching `no_exit_to_cli.rs`. It
//! cannot prove a guard dominates a particular spawn — only a reader can — but
//! it does catch the regression that actually happens: the last guard in a file
//! disappearing during a refactor. Anything finer would need real control-flow
//! analysis and would be brittle enough that people would delete the test.

use std::fs;
use std::path::{Path, PathBuf};

/// Invocations that reach the network through a tool PRISM does not control,
/// and that match real code today. Each is asserted to still match, so a marker
/// cannot go stale and quietly stop enforcing anything.
///
/// Literal program names, plus the two shapes where the program comes from a
/// variable and only the ARGUMENTS reveal it: a container runtime held in a
/// field (`Command::new(&self.runtime).args(["pull", …])`) and the pyiron
/// sidecar interpreter (`Command::new(sidecar_python()).args(["-m","pip", …])`).
/// Without those two, `crates/compute/src/local.rs` and
/// `crates/cli/src/pyiron_cmd.rs` would pass by being unreadable.
const ACTIVE_MARKERS: &[&str] = &[
    "Command::new(\"ssh\")",
    "Command::new(\"kubectl\")",
    "Command::new(\"gh\")",
    "Command::new(\"hf\")",
    "Command::new(\"ollama\")",
    "args([\"pull\"",
    "\"-m\", \"pip\", \"install\"",
    "\"--pull\"",
];

/// Markers kept deliberately even though they match nothing right now.
///
/// Both container runtimes are invoked through a variable today —
/// `Command::new(runtime.binary())`, `Command::new(&self.runtime)` — so the
/// literals never appear. They stay so that hardcoding `docker`/`podman`
/// tomorrow is caught the day it lands, and they are listed separately so the
/// staleness check below does not have to be weakened to accommodate them.
const FORWARD_MARKERS: &[&str] = &["Command::new(\"docker\")", "Command::new(\"podman\")"];

fn network_tool_markers() -> Vec<&'static str> {
    ACTIVE_MARKERS
        .iter()
        .chain(FORWARD_MARKERS)
        .copied()
        .collect()
}

/// Consulting the policy at all. Either primitive counts: `check_url` for a
/// target that might legitimately be loopback, `enabled()` for a tool whose
/// destination is fixed and remote.
const GUARD_MARKERS: &[&str] = &["offline::enabled", "offline::check_url"];

/// Files allowed to invoke a network tool with no guard, each with the reason.
///
/// Empty, and it should stay that way. An entry here is a decision that a
/// packet may leave under hard offline mode — it belongs in review, not in a
/// silent pass.
const ALLOWED_UNGUARDED: &[(&str, &str)] = &[];

/// Every file known to invoke a network tool, pinned by path.
///
/// The per-marker staleness check is not sufficient on its own. Most markers
/// match two or more files, so a file can leave the scan entirely — refactor
/// `Command::new("gh")` to `Command::new(gh_binary())`, which the docstring
/// above admits is already the shape `docker`/`podman` and the pyiron
/// interpreter use — while another file keeps that marker alive. Drop the
/// guard in the same change and nothing fails.
///
/// Pinning the file list closes that: a file that stops matching any marker
/// fails here and forces the question "did the spawn move, or did the marker
/// stop seeing it?" to be answered in review rather than by silence.
const EXPECTED_SPAWNERS: &[&str] = &[
    "agent/src/protocol.rs",
    "cli/src/main.rs",
    "cli/src/pyiron_cmd.rs",
    "compute/src/byoc.rs",
    "compute/src/local.rs",
    "node/src/executor.rs",
    "node/src/runtime_service.rs",
    "python-bridge/src/venv.rs",
];

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with('*')
}

#[test]
fn every_file_that_spawns_a_network_tool_consults_the_offline_policy() {
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .to_path_buf();

    let mut sources = Vec::new();
    for entry in fs::read_dir(&crates_dir).expect("read crates/").flatten() {
        let src = entry.path().join("src");
        if src.is_dir() {
            rust_sources(&src, &mut sources);
        }
    }
    assert!(
        sources.len() > 100,
        "scanned only {} files — the walk is broken, and a broken walk passes silently",
        sources.len()
    );

    let markers = network_tool_markers();
    let mut spawning_files = 0usize;
    let mut matched_files: Vec<String> = Vec::new();
    let mut active_hits = vec![0usize; ACTIVE_MARKERS.len()];
    let mut violations: Vec<String> = Vec::new();

    for path in &sources {
        let Ok(body) = fs::read_to_string(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(&crates_dir)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        // This test's own marker list would otherwise match itself.
        if rel.contains("network_tools_are_offline_guarded") {
            continue;
        }

        let hit = body
            .lines()
            .find(|line| !is_comment(line) && markers.iter().any(|m| line.contains(m)));
        for (i, m) in ACTIVE_MARKERS.iter().enumerate() {
            if body.lines().any(|l| !is_comment(l) && l.contains(m)) {
                active_hits[i] += 1;
            }
        }

        let Some(hit) = hit else {
            continue;
        };
        spawning_files += 1;
        matched_files.push(rel.clone());

        // Comment-excluded, exactly like the spawn check above. It was
        // `body.contains(g)` on the RAW text, so a file with a genuinely
        // unguarded spawn plus any comment mentioning `offline::enabled` —
        // `// TODO: call offline::enabled here` — counted as guarded and was
        // skipped. Verified by mutation: byoc.rs with every real guard removed
        // and one such comment left behind passed this test.
        let guarded = body
            .lines()
            .any(|line| !is_comment(line) && GUARD_MARKERS.iter().any(|g| line.contains(g)));
        if guarded {
            continue;
        }
        if ALLOWED_UNGUARDED.iter().any(|(f, _)| rel.contains(f)) {
            continue;
        }
        violations.push(format!("  {rel}\n      {}", hit.trim()));
    }

    // A stale marker is the failure mode this test cannot see in its own
    // result: it would pass while enforcing less than it claims. A file-count
    // floor is NOT enough — `byoc.rs` matches both the ssh and kubectl markers,
    // so breaking one of them alone left the count unchanged and the check
    // silent. Verified by mutation: that is exactly what happened on the first
    // version of this test. So every ACTIVE marker must still match something.
    let stale: Vec<&str> = ACTIVE_MARKERS
        .iter()
        .zip(&active_hits)
        .filter(|(_, hits)| **hits == 0)
        .map(|(m, _)| *m)
        .collect();
    assert!(
        stale.is_empty(),
        "these markers no longer match any source, so this test silently stopped \
         enforcing them: {stale:?}. Either the last use was removed (delete the \
         marker deliberately, or move it to FORWARD_MARKERS) or it drifted."
    );
    let vanished: Vec<&str> = EXPECTED_SPAWNERS
        .iter()
        .filter(|expected| !matched_files.iter().any(|m| m.contains(*expected)))
        .copied()
        .collect();
    assert!(
        vanished.is_empty(),
        "these files are known to spawn a network tool but no longer match any \
         marker, so they silently left this test's scope: {vanished:?}. Either the \
         spawn moved (update EXPECTED_SPAWNERS) or it is now invoked through a \
         variable and needs a new marker."
    );
    assert!(
        spawning_files >= EXPECTED_SPAWNERS.len(),
        "only {spawning_files} files matched a network-tool marker; expected at \
         least {}.",
        EXPECTED_SPAWNERS.len()
    );

    assert!(
        violations.is_empty(),
        "these files shell out to a network tool but never consult \
         `prism_runtime::offline` — under PRISM_OFFLINE=1 the tool runs anyway, and \
         several of them carry a credential (ssh key, gh token, HF token, registry \
         login):\n{}\n\nGuard the spawn, or add the file to ALLOWED_UNGUARDED with a \
         reason.",
        violations.join("\n")
    );
}
