// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Deterministic execution-contract enforcement — the structural half of the
//! Agent Execution Contract.
//!
//! Prompt text is the weakest form of enforcement: a model that decides
//! answering is cheaper than acting will ignore it. This module is the part
//! that cannot be talked out of. It runs in the harness, needs no LLM, and
//! fires on the ONE claim class that is both deterministic to detect and the
//! contract's headline failure:
//!
//!   > Never claim to have inspected/tested/searched/modified/verified
//!   > anything unless a corresponding tool result exists in the current run.
//!
//! Same shape as the platform's `deterministic_grounding_gate`
//! (marc27-core `b8a70fe3`): a no-LLM check that rejects a finalization and
//! sends the model back, rather than an instruction it may choose to follow.
//!
//! Claims are matched against the CLASS of evidence they assert, not against a
//! turn-wide "did any tool run" flag. Reading a file is not evidence that the
//! tests were run; in a real session some tool almost always ran, so a boolean
//! would be close to toothless.
//!
//! Deliberately NOT covered here: task substitution (fix→explain) and the
//! ≥3-attempts budget. Both need intent, not string matching; a regex gate on
//! them would fire on chit-chat and teach the model to write around the gate.
//! Those stay prompt-side until there is a verifier pass to hang them on.

/// What kind of evidence a claim demands. A claim is unsupported unless a tool
/// of the SAME class ran — "I ran the tests" is not evidenced by having read a
/// file. A turn-wide "did any tool run" counter would be nearly toothless in
/// practice, because in a real session something almost always ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Evidence {
    /// Something was executed: shell, python, notebook, workflow, compute job.
    Execution,
    /// Something was retrieved or read: search, query, read, list, status.
    Retrieval,
    /// Something was written: file edit/write, ingest, skill/notebook authoring.
    Write,
    /// Something left the machine: deploy, publish, mesh publication.
    Deploy,
}

impl Evidence {
    /// Substrings of tool NAMES that count as evidence for this class. Matched
    /// as substrings so the classifier survives catalog growth (`execute_bash`,
    /// `execute_python`, `notebook_execute`, … all match `execute`) without
    /// enumerating a catalog that changes every release.
    fn tool_name_markers(self) -> &'static [&'static str] {
        match self {
            Evidence::Execution => &[
                "execute",
                "bash",
                "python",
                "notebook",
                "run",
                "workflow",
                "test",
                "compute",
                "job",
                "skill",
                "discourse",
                "subagent",
            ],
            Evidence::Retrieval => &[
                "read",
                "search",
                "query",
                "list",
                "status",
                "find",
                "knowledge",
                "research",
                "web",
                "fetch",
                "get",
                "show",
                "inspect",
                "probe",
                "health",
                "predict",
                "recall",
                "models",
                "marketplace",
                "capabilities",
                "peers",
                "discover",
            ],
            Evidence::Write => &[
                "edit", "write", "create", "ingest", "save", "append", "install", "import",
                "commit", "publish", "upload", "skill",
            ],
            Evidence::Deploy => &["deploy", "publish", "mesh", "submit", "serve", "push"],
        }
    }
}

/// Verbs that assert the agent performed an action, with the evidence class
/// each one demands.
///
/// `.0` is the past-simple form (reached directly from `i`), `.1` the past
/// participle (reached only after a perfect auxiliary). They differ only for
/// `ran`/`run`, and keeping both columns is what stops bare "I run" — present
/// tense, common in ordinary prose ("I run the risk of…") — from ever counting
/// as a claim.
///
/// Deliberately excluded: `analyzed`, `reviewed`, `considered`, `looked`,
/// `thought`. Those describe reasoning, which the model genuinely does without
/// a tool, and gating them would punish honest narration.
const CLAIM_VERBS: &[(&str, &str, Evidence)] = &[
    ("ran", "run", Evidence::Execution),
    ("executed", "executed", Evidence::Execution),
    ("tested", "tested", Evidence::Execution),
    ("benchmarked", "benchmarked", Evidence::Execution),
    ("compiled", "compiled", Evidence::Execution),
    ("searched", "searched", Evidence::Retrieval),
    ("queried", "queried", Evidence::Retrieval),
    ("inspected", "inspected", Evidence::Retrieval),
    ("checked", "checked", Evidence::Retrieval),
    ("verified", "verified", Evidence::Retrieval),
    ("confirmed", "confirmed", Evidence::Retrieval),
    ("edited", "edited", Evidence::Write),
    ("modified", "modified", Evidence::Write),
    ("created", "created", Evidence::Write),
    ("wrote", "written", Evidence::Write),
    ("implemented", "implemented", Evidence::Write),
    ("installed", "installed", Evidence::Write),
    ("fixed", "fixed", Evidence::Write),
    ("ingested", "ingested", Evidence::Write),
    ("deployed", "deployed", Evidence::Deploy),
    ("published", "published", Evidence::Deploy),
];

/// The reminder injected when the gate rejects a finalization. Phrased as a
/// fork, not an accusation: the model either does the work it claimed, or
/// restates honestly. Both exits are acceptable; silently shipping the
/// unsupported claim is not.
pub const UNSUPPORTED_CLAIM_REMINDER: &str = "<system-reminder>\
EXECUTION CONTRACT — finalization rejected. Your answer claims you performed \
an action (ran / tested / searched / checked / verified / edited / created / \
deployed / ingested), but no tool that could produce that evidence ran in this \
turn. Do one of two things now, and nothing else:\n\
1. If the action is what the user asked for, CALL THE TOOL and do it.\n\
2. If you were describing earlier work or what the user should do, rewrite the \
answer so it does not claim you just did it — say plainly what you did not check.\
</system-reminder>";

/// Return the first execution claim in `answer` for which no tool of the
/// matching evidence class ran this turn. `None` allows the finalization.
///
/// `tools_used` is the list of tool names that ACTUALLY executed this turn —
/// denied / policy-blocked calls are not evidence and must not appear.
///
/// Precision over recall by design: one extra model turn on a false positive
/// is cheap, a silently fabricated "I ran the tests" is not.
///
/// KNOWN GAP, stated rather than papered over: only first-person claims are
/// matched. Passive and third-person phrasing ("Tests pass now.", "Fix
/// applied.") bypasses this entirely, and no string list closes that — it needs
/// the contract's adversarial verifier pass (structural fix #3), which does not
/// exist in this repo yet.
#[must_use]
pub fn unsupported_execution_claim(answer: &str, tools_used: &[String]) -> Option<String> {
    let words = tokenize(answer);
    for (i, word) in words.iter().enumerate() {
        if word != "i" {
            continue;
        }
        if let Some(claim) = claim_at(&words, i)
            && !has_evidence(claim.1, tools_used)
        {
            return Some(claim.0);
        }
    }
    None
}

/// Scan the bounded window after a first-person `i` for a claim verb.
///
/// The window is what makes negation and idiom exclusion STRUCTURAL rather
/// than a pile of heuristics: the scan advances only across a closed set of
/// auxiliaries and adverbs, and stops dead on anything else. So "I did not
/// run" stops at `did`, "I have not verified" stops at `not`, and "I don't
/// know" stops at `don` — none of them can reach a verb.
fn claim_at(words: &[String], i: usize) -> Option<(String, Evidence)> {
    let mut perfect = false;
    for j in i + 1..(i + 1 + MAX_CLAIM_WINDOW).min(words.len()) {
        let w = words[j].as_str();
        if PERFECT_AUX.contains(&w) {
            perfect = true;
            continue;
        }
        if SKIPPABLE_ADVERBS.contains(&w) {
            continue;
        }
        for (past, participle, evidence) in CLAIM_VERBS {
            let matches = if perfect {
                w == *participle
            } else {
                w == *past
            };
            if matches {
                // Idiom / non-artifact guard: "I ran INTO trouble" and
                // "I checked MY assumptions" are not execution claims.
                if words
                    .get(j + 1)
                    .is_some_and(|next| NON_ARTIFACT_OBJECTS.contains(&next.as_str()))
                {
                    return None;
                }
                return Some((format!("i {w}"), *evidence));
            }
        }
        // Anything else ends the construction — no verb, no claim.
        return None;
    }
    None
}

/// How many words after `i` may be scanned for a claim verb. Three covers
/// "I have already run"; more would start crossing clause boundaries.
const MAX_CLAIM_WINDOW: usize = 3;

/// Auxiliaries that put the following verb in the perfect form. `ve`/`d` are
/// what `i've` / `i'd` tokenize to.
const PERFECT_AUX: &[&str] = &["have", "ve", "had", "d"];

/// Adverbs that may sit between the subject/auxiliary and the verb without
/// changing the claim. Anything NOT in this list stops the scan.
const SKIPPABLE_ADVERBS: &[&str] = &[
    "already",
    "just",
    "now",
    "then",
    "also",
    "successfully",
    "quickly",
    "manually",
    "finally",
];

/// Words that, immediately after a claim verb, mean the object is not an
/// artifact or execution — so the sentence is not an execution claim.
/// "I ran into confusion", "I checked my assumptions", "I've run out of ideas".
const NON_ARTIFACT_OBJECTS: &[&str] = &[
    "into", "out", "over", "through", "up", "low", "short", "across", "my", "your", "our", "his",
    "her", "their", "its",
];

/// Lowercase and split into alphabetic words.
///
/// Non-alphabetic characters are separators, so `i've` -> `["i", "ve"]` and
/// the typographic apostrophe `’` needs no special case. Splitting on
/// `char::is_alphabetic` (not `is_ascii_alphanumeric`) also means a marker
/// glued to a non-ASCII letter stays inside one token and cannot produce a
/// spurious word boundary — the Unicode boundary problem disappears instead of
/// being approximated.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Did any tool of this evidence class actually run?
fn has_evidence(evidence: Evidence, tools_used: &[String]) -> bool {
    let markers = evidence.tool_name_markers();
    tools_used.iter().any(|name| {
        let lower = name.to_ascii_lowercase();
        markers.iter().any(|marker| lower.contains(marker))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn used(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn claim_with_no_tool_at_all_is_rejected() {
        for answer in [
            "I ran the test suite and everything passes.",
            "I've verified the deployment is healthy.",
            "I have searched the knowledge graph for nickel superalloys.",
            "Done — I edited crates/agent/src/prompts.rs.",
            "I checked the mesh peers; three are online.",
            "I fixed the failing assertion.",
            "I created the workflow file for you.",
            "I have written the new module.",
            "I deployed it to staging.",
        ] {
            assert!(
                unsupported_execution_claim(answer, &[]).is_some(),
                "gate must reject: {answer}"
            );
        }
    }

    /// The claim must be backed by a tool of the MATCHING class. This is the
    /// whole point of the classifier: a turn-wide "did any tool run" counter is
    /// nearly toothless, because in a real session something almost always ran.
    #[test]
    fn wrong_class_of_evidence_does_not_excuse_a_claim() {
        // Reading a file is not evidence that the tests were run.
        assert!(
            unsupported_execution_claim("I ran the test suite.", &used(&["read_file"])).is_some(),
            "read_file must not excuse an execution claim"
        );
        // …but actually executing something is.
        assert_eq!(
            unsupported_execution_claim("I ran the test suite.", &used(&["execute_bash"])),
            None
        );
        // Searching is not evidence that a file was edited.
        assert!(
            unsupported_execution_claim("I edited the module.", &used(&["query_platform"]))
                .is_some()
        );
        assert_eq!(
            unsupported_execution_claim("I edited the module.", &used(&["edit_file"])),
            None
        );
        // Editing a file is not evidence of a deployment.
        assert!(
            unsupported_execution_claim("I deployed the service.", &used(&["write_file"]))
                .is_some()
        );
        assert_eq!(
            unsupported_execution_claim("I deployed the service.", &used(&["deploy_create"])),
            None
        );
    }

    #[test]
    fn matching_evidence_lets_the_claim_through() {
        assert_eq!(
            unsupported_execution_claim("I searched the graph.", &used(&["query_platform"])),
            None
        );
        assert_eq!(
            unsupported_execution_claim("I've verified the deploy.", &used(&["deploy_health"])),
            None
        );
        assert_eq!(
            unsupported_execution_claim("I ingested the paper.", &used(&["ingest_file"])),
            None
        );
    }

    /// The negation exclusion is structural: `"i ran"` is not a substring of
    /// `"i did not run"`, so honest reporting is never punished.
    #[test]
    fn honest_non_execution_passes() {
        for answer in [
            "I did not run the tests — no test command is configured.",
            "I have not verified this; the knowledge graph returned nothing.",
            "I could not check the deployment: the platform is unreachable.",
            "You can run `cargo test --workspace` to confirm.",
            "Hello! What would you like to work on?",
            "I don't know — that is not in the knowledge graph.",
            "I analyzed your description and here is what I think.",
        ] {
            assert_eq!(
                unsupported_execution_claim(answer, &[]),
                None,
                "gate must NOT reject: {answer}"
            );
        }
    }

    /// Tokenizing on word boundaries means a verb inside a longer word is not
    /// a claim, and there is no bare `i` to anchor on.
    #[test]
    fn word_boundaries_prevent_substring_false_positives() {
        assert_eq!(
            unsupported_execution_claim("multi ranking is hard", &[]),
            None
        );
        assert_eq!(
            unsupported_execution_claim("The api rant is long", &[]),
            None
        );
    }

    /// `"i run"` is present tense and must not be treated as a claim; only the
    /// perfect form is.
    #[test]
    fn present_tense_run_is_not_a_claim() {
        assert_eq!(
            unsupported_execution_claim("If I run this, I run the risk of data loss.", &[]),
            None
        );
        assert!(unsupported_execution_claim("I have run it.", &[]).is_some());
    }

    /// Idioms that share a verb with a claim but assert nothing about work
    /// done. Asking is now the pre-flight's job (`crate::reprompt`), not the
    /// model's, but an honest "I ran into confusion" must still not be
    /// mistaken for a fabricated execution claim — that would punish accurate
    /// narration and teach the model to write around the gate.
    #[test]
    fn idioms_and_non_artifact_objects_are_not_claims() {
        for answer in [
            "I ran into some confusion about which file you meant — could you clarify?",
            "I've run out of clear next steps without more input.",
            "I modified my estimate based on typical alloy density.",
            "I checked my assumptions and they hold.",
            "I ran through the options in my head.",
        ] {
            assert_eq!(
                unsupported_execution_claim(answer, &[]),
                None,
                "idiom must not be treated as a claim: {answer}"
            );
        }
    }

    /// An adverb between the auxiliary and the verb must not smuggle a claim
    /// past the gate.
    #[test]
    fn adverbs_do_not_evade_the_gate() {
        for answer in [
            "I have already run the tests.",
            "I've successfully verified the deployment.",
            "I have just checked the logs.",
            "I then deployed it.",
        ] {
            assert!(
                unsupported_execution_claim(answer, &[]).is_some(),
                "adverb evaded the gate: {answer}"
            );
        }
    }

    /// The typographic apostrophe must behave exactly like the ASCII one —
    /// tokenizing on non-alphabetic chars makes this structural, not a
    /// special case that a future encoding can slip past.
    #[test]
    fn smart_apostrophe_does_not_evade_the_gate() {
        assert!(unsupported_execution_claim("I\u{2019}ve verified the deployment.", &[]).is_some());
        assert!(unsupported_execution_claim("I'VE VERIFIED THE DEPLOYMENT.", &[]).is_some());
    }

    /// Multi-byte input must neither panic nor be mis-bounded.
    #[test]
    fn non_ascii_text_is_handled() {
        assert_eq!(
            unsupported_execution_claim("Das Gefüge ist grobkörnig — 900 °C, 1 h.", &[]),
            None
        );
        assert!(
            unsupported_execution_claim("Kurz: I ran the suite — alles grün. ✅", &[]).is_some()
        );
    }

    #[test]
    fn reminder_offers_both_exits() {
        assert!(UNSUPPORTED_CLAIM_REMINDER.contains("CALL THE TOOL"));
        assert!(UNSUPPORTED_CLAIM_REMINDER.contains("rewrite the answer"));
    }
}
