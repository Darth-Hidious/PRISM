// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
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
//! Deliberately NOT covered here: task substitution (fix→explain) and the
//! ≥3-attempts budget. Both need intent, not string matching; a regex gate on
//! them would fire on chit-chat and teach the model to write around the gate.
//! Those stay prompt-side until there is a verifier pass to hang them on.

/// Verbs whose PAST tense asserts that the agent performed an action.
/// Matched as `"i <verb>"`, so the negated forms ("I did not run",
/// "I have not verified") are not substrings and never match — the negation
/// exclusion is structural, not a second heuristic.
const PAST_TENSE_CLAIMS: &[&str] = &[
    "ran",
    "tested",
    "executed",
    "searched",
    "checked",
    "verified",
    "inspected",
    "edited",
    "modified",
    "deployed",
    "queried",
    "ingested",
    "fixed",
    "benchmarked",
];

/// Same verbs in the perfect form, matched as `"i have <verb>"` / `"i've
/// <verb>"`. `run` only appears here: bare `"i run"` is present-tense and
/// collides with ordinary prose ("I run the risk of...").
const PERFECT_CLAIMS: &[&str] = &[
    "run",
    "ran",
    "tested",
    "executed",
    "searched",
    "checked",
    "verified",
    "inspected",
    "edited",
    "modified",
    "deployed",
    "queried",
    "ingested",
    "fixed",
    "benchmarked",
];

/// The reminder injected when the gate rejects a finalization. Phrased as a
/// fork, not an accusation: the model either does the work it claimed, or
/// restates honestly. Both exits are acceptable; silently shipping the
/// unsupported claim is not.
pub const UNSUPPORTED_CLAIM_REMINDER: &str = "<system-reminder>\
EXECUTION CONTRACT — finalization rejected. Your answer claims you performed \
an action (ran / tested / searched / checked / verified / edited / deployed / \
ingested), but no tool executed in this turn, so there is no evidence for it. \
Do one of two things now, and nothing else:\n\
1. If the action is what the user asked for, CALL THE TOOL and do it.\n\
2. If you were describing earlier work or what the user should do, rewrite the \
answer so it does not claim you just did it — say plainly what you did not check.\
</system-reminder>";

/// Return the first unsupported execution claim in `answer` when no tool ran
/// in this turn. `None` means the finalization is allowed through.
///
/// Precision over recall by design: one extra model turn on a false positive
/// is cheap, a silently fabricated "I ran the tests" is not. The caller fires
/// this at most once per turn so a stubborn model can still finish.
#[must_use]
pub fn unsupported_execution_claim(answer: &str, tool_calls_this_turn: usize) -> Option<String> {
    if tool_calls_this_turn > 0 {
        return None;
    }
    let text = answer.to_ascii_lowercase();
    for verb in PAST_TENSE_CLAIMS {
        let marker = format!("i {verb}");
        if contains_claim(&text, &marker) {
            return Some(marker);
        }
    }
    for verb in PERFECT_CLAIMS {
        for marker in [format!("i have {verb}"), format!("i've {verb}")] {
            if contains_claim(&text, &marker) {
                return Some(marker);
            }
        }
    }
    None
}

/// Substring match with a word boundary on both ends, so `"i ran"` does not
/// fire inside `"multi ranking"` and `"i edited"` does not fire inside
/// `"i editedness"`.
fn contains_claim(haystack: &str, marker: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = haystack[from..].find(marker) {
        let start = from + rel;
        let end = start + marker.len();
        let left_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let right_ok = end == bytes.len() || !is_word_byte(bytes[end]);
        if left_ok && right_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_without_any_tool_call_is_rejected() {
        for answer in [
            "I ran the test suite and everything passes.",
            "I've verified the deployment is healthy.",
            "I have searched the knowledge graph for nickel superalloys.",
            "Done — I edited crates/agent/src/prompts.rs.",
            "I checked the mesh peers; three are online.",
            "I fixed the failing assertion.",
        ] {
            assert!(
                unsupported_execution_claim(answer, 0).is_some(),
                "gate must reject: {answer}"
            );
        }
    }

    #[test]
    fn same_claim_passes_once_a_tool_actually_ran() {
        assert!(unsupported_execution_claim("I ran the test suite.", 1).is_none());
        assert!(unsupported_execution_claim("I've verified the deploy.", 4).is_none());
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
        ] {
            assert_eq!(
                unsupported_execution_claim(answer, 0),
                None,
                "gate must NOT reject: {answer}"
            );
        }
    }

    /// Word boundaries: no firing inside longer words.
    #[test]
    fn word_boundaries_prevent_substring_false_positives() {
        assert_eq!(
            unsupported_execution_claim("multi ranking is hard", 0),
            None
        );
        assert_eq!(unsupported_execution_claim("The api rant is long", 0), None);
    }

    /// `"i run"` is present tense and must not be treated as a claim; only the
    /// perfect form is.
    #[test]
    fn present_tense_run_is_not_a_claim() {
        assert_eq!(
            unsupported_execution_claim("If I run this, I run the risk of data loss.", 0),
            None
        );
        assert!(unsupported_execution_claim("I have run it.", 0).is_some());
    }

    #[test]
    fn reminder_offers_both_exits() {
        assert!(UNSUPPORTED_CLAIM_REMINDER.contains("CALL THE TOOL"));
        assert!(UNSUPPORTED_CLAIM_REMINDER.contains("rewrite the answer"));
    }
}
