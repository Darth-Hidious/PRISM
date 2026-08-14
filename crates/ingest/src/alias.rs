//! Verified alias pass: `same_as` edges between entity NAMES the document
//! itself defines as the same thing.
//!
//! Chunked extraction splits entity IDENTITY, not facts: "Ti-6Al-4V" in
//! methods and "Ti64" in results become two unconnected nodes, because the
//! store keys on the canonical name and a small model does not normalise
//! names across tens of thousands of characters. This pass runs ONCE per
//! document, over the distinct entity names that were actually written — no
//! document text goes to the model. The model only PROPOSES pairs; CODE
//! verifies every proposal and accepts exactly two kinds of evidence:
//!
//! 1. **Deterministic normalisation** — the two names are one string modulo
//!    Unicode dash variants, case, and whitespace.
//! 2. **A defining span in the document** — parenthetical adjacency
//!    (`Ti-6Al-4V (Ti64)`) or an explicit definition keyword ("hereafter",
//!    "denoted", "also known as") in one span naming both.
//!
//! **Co-occurrence is NOT alias evidence.** "IN718 outperformed IN625" is two
//! names in one sentence and two different materials; a wrong merge is worse
//! than no merge. Verified pairs become `same_as` EDGES — ordinary value-less
//! facts connecting the two nodes — never destructive node merges, so a wrong
//! accept is one deletable edge, not a corrupted node. Rejected proposals are
//! reported, never silently discarded.

use prism_llm::LlmClient;
use prism_provenance::{EvidenceSource, MaterialFact, evidence_for_result};
use serde::Deserialize;

use crate::text_extract::{extract_json_block, sentence_spans, span_contains_term};

/// The predicate every verified alias edge is written under.
pub const SAME_AS_PREDICATE: &str = "same_as";

/// Definition keywords whose presence in a span naming BOTH entities counts
/// as a defining span. Kept short and explicit: every entry is a way papers
/// DEFINE a name, not a way they merely relate two things.
const DEFINITION_KEYWORDS: &[&str] = &["hereafter", "denoted", "also known as"];

/// How one proposed pair was verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasEvidence {
    /// The names are one string under deterministic normalisation
    /// (Unicode dashes, case, whitespace) — no model judgement involved.
    Normalisation,
    /// The document defines one as the other; the span is verbatim from the
    /// grounding corpus.
    DefiningSpan(String),
}

impl AliasEvidence {
    pub fn describe(&self) -> String {
        match self {
            AliasEvidence::Normalisation => {
                "deterministic normalisation (dashes/case/whitespace)".to_string()
            }
            AliasEvidence::DefiningSpan(span) => format!("defining span: {span:?}"),
        }
    }
}

/// One accepted pair: the `same_as` fact to write, plus the evidence that
/// justified it (for the report — the store has no per-fact span field).
#[derive(Debug, Clone)]
pub struct VerifiedAlias {
    pub fact: MaterialFact,
    pub evidence: AliasEvidence,
}

/// One rejected proposal, with the reason code verification refused it.
#[derive(Debug, Clone)]
pub struct RejectedAlias {
    pub a: String,
    pub b: String,
    pub reason: String,
}

/// What the alias pass produced. `error` set means the model call or its
/// parse failed and the pass was skipped — a degraded run, reported, never a
/// failed ingest: every extracted fact is already stored by the time this
/// runs.
#[derive(Debug, Default)]
pub struct AliasPass {
    pub accepted: Vec<VerifiedAlias>,
    pub rejected: Vec<RejectedAlias>,
    pub usage: Option<prism_llm::UsageInfo>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct AliasEnvelope {
    #[serde(default)]
    aliases: Vec<AliasProposal>,
}

#[derive(Deserialize)]
struct AliasProposal {
    a: String,
    b: String,
}

/// Run the alias pass: ONE model call over `names` (the distinct entity
/// names of the facts actually written for this document — never document
/// text), then code verification of every proposal against `document`, the
/// same grounding corpus extraction used (`unwrap_soft_line_breaks` of the
/// whole text).
///
/// Fewer than two names cannot alias: the pass returns empty without a call.
pub async fn link_aliases(llm: &LlmClient, names: &[String], document: &str) -> AliasPass {
    let mut pass = AliasPass::default();
    if names.len() < 2 {
        return pass;
    }

    let prompt = build_alias_prompt(names);
    let (raw, usage) = match llm.generate_json_with_usage(&prompt).await {
        Ok(response) => response,
        Err(error) => {
            pass.error = Some(format!("alias proposal call failed: {error:#}"));
            return pass;
        }
    };
    pass.usage = usage;
    let envelope: AliasEnvelope = match serde_json::from_str(extract_json_block(&raw)) {
        Ok(envelope) => envelope,
        Err(error) => {
            pass.error = Some(format!(
                "alias proposals were not valid JSON — pass skipped: {error}"
            ));
            return pass;
        }
    };

    let known: std::collections::HashSet<&str> = names.iter().map(String::as_str).collect();
    let mut seen_pairs: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for proposal in envelope.aliases {
        let (a, b) = (proposal.a.trim(), proposal.b.trim());
        let reject = |reason: String, pass: &mut AliasPass| {
            pass.rejected.push(RejectedAlias {
                a: a.to_string(),
                b: b.to_string(),
                reason,
            });
        };
        if a.is_empty() || b.is_empty() {
            reject("empty name".into(), &mut pass);
            continue;
        }
        if !known.contains(a) || !known.contains(b) {
            // The model may only pair names it was handed. A name of its own
            // invention has no node to connect — and accepting it would let
            // the alias pass mint entities, which is extraction's job under
            // grounding, not this pass's.
            reject(
                "not among this document's extracted entity names".into(),
                &mut pass,
            );
            continue;
        }
        if a == b {
            reject("a name is not an alias of itself".into(), &mut pass);
            continue;
        }
        let pair_key = if a < b {
            (a.to_string(), b.to_string())
        } else {
            (b.to_string(), a.to_string())
        };
        if !seen_pairs.insert(pair_key) {
            reject("duplicate proposal for this pair".into(), &mut pass);
            continue;
        }
        match verify_alias(a, b, document) {
            Ok(evidence) => {
                let confidence = match &evidence {
                    // Deterministic string identity — code proved it.
                    AliasEvidence::Normalisation => 1.0,
                    // Span-verified but pattern-matched: parenthetical
                    // adjacency and definition keywords are strong, not
                    // infallible.
                    AliasEvidence::DefiningSpan(_) => 0.9,
                };
                pass.accepted.push(VerifiedAlias {
                    fact: MaterialFact {
                        subject: a.to_string(),
                        predicate: SAME_AS_PREDICATE.to_string(),
                        object: b.to_string(),
                        value: None,
                        unit: None,
                        conditions: Vec::new(),
                        confidence: Some(confidence),
                        // A generic value-less edge, NEVER a destructive
                        // merge: kind None keeps the store from minting any
                        // measurement shape for it.
                        kind: None,
                        evidence_class: evidence_for_result(
                            EvidenceSource::LiteratureExtraction,
                            [Default::default()],
                        ),
                    },
                    evidence,
                });
            }
            Err(reason) => reject(reason, &mut pass),
        }
    }
    pass
}

/// Code verification of one proposed pair. Accepts deterministic
/// normalisation or a defining span; everything else — co-occurrence
/// included — is a rejection with the reason.
fn verify_alias(a: &str, b: &str, document: &str) -> Result<AliasEvidence, String> {
    if normalise_name(a) == normalise_name(b) {
        return Ok(AliasEvidence::Normalisation);
    }
    if let Some(span) = defining_span(a, b, document) {
        return Ok(AliasEvidence::DefiningSpan(span));
    }
    Err(
        "no defining span: the document never defines one name as the other \
         (co-occurrence is not alias evidence)"
            .to_string(),
    )
}

/// Deterministic name normalisation: Unicode dash variants to `-` and
/// whitespace runs collapsed to one space, trimmed. Exactly the rewrites
/// that cannot change WHICH entity a materials name denotes — no stemming,
/// no abbreviation guessing, and NO case folding: letter case is
/// semantically load-bearing in chemistry (`Co` is cobalt, `CO` is carbon
/// monoxide; `In` is indium, `IN` prefixes Inconel grades), and this path
/// accepts with zero document evidence, so it must be airtight. A genuine
/// case variant can still be linked — through a defining span the document
/// itself provides.
fn normalise_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_space = false;
    for c in name.trim().chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        match c {
            // Hyphen, non-breaking hyphen, figure dash, en dash, em dash,
            // horizontal bar, minus sign.
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => out.push('-'),
            _ => out.push(c),
        }
    }
    out
}

/// A span of `document` that DEFINES `a` and `b` as the same thing, verbatim
/// from the corpus: parenthetical adjacency in either order, or a sentence
/// span where a definition keyword BINDS the pair.
fn defining_span(a: &str, b: &str, document: &str) -> Option<String> {
    if let Some(span) = parenthetical_span(a, b, document) {
        return Some(span);
    }
    if let Some(span) = parenthetical_span(b, a, document) {
        return Some(span);
    }
    document
        .lines()
        .flat_map(sentence_spans)
        .find(|span| keyword_binds_pair(span, a, b))
        .map(|span| span.trim().to_string())
}

/// The keyword must sit in the SHORT GAP between the two names — "Laser
/// Powder Bed Fusion, hereafter L-PBF" — never merely co-occur in the same
/// sentence. "Ti-6Al-4V (hereafter Ti64), IN718, and IN625 were compared"
/// contains IN718, IN625, and "hereafter" in one span, but the keyword
/// defines a THIRD name; the gap between IN718 and IN625 (", and ") carries
/// no keyword, so the pair is refused.
const MAX_DEFINITION_GAP_BYTES: usize = 48;

fn keyword_binds_pair(span: &str, a: &str, b: &str) -> bool {
    let (Some(pos_a), Some(pos_b)) = (find_word(span, a), find_word(span, b)) else {
        return false;
    };
    let (first_start, first_len, second_start) = if pos_a <= pos_b {
        (pos_a, a.len(), pos_b)
    } else {
        (pos_b, b.len(), pos_a)
    };
    let gap_start = first_start + first_len;
    if gap_start > second_start {
        return false; // overlapping matches — never a definition
    }
    let gap = &span[gap_start..second_start];
    gap.len() <= MAX_DEFINITION_GAP_BYTES
        && DEFINITION_KEYWORDS
            .iter()
            .any(|keyword| span_contains_term(gap, keyword))
}

/// Byte offset of the first WORD-BOUNDED, ASCII-case-insensitive occurrence
/// of `needle` in `haystack`. Only the first occurrence is considered — if a
/// name recurs, a later occurrence adjacent to the other name is missed,
/// which fails CLOSED (a missed true alias, never a false one).
fn find_word(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let hay = haystack.to_ascii_lowercase();
    let need = needle.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(offset) = hay[from..].find(&need) {
        let start = from + offset;
        let end = start + need.len();
        let left_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|before| !before.is_alphanumeric());
        let right_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|after| !after.is_alphanumeric());
        if left_ok && right_ok {
            return Some(start);
        }
        from = start + 1;
    }
    None
}

/// Find `"{name} ({alias})"` in `document`, ASCII-case-insensitively, and
/// return the matched text verbatim. ASCII folding keeps byte offsets
/// aligned between the folded haystack and the original, so the returned
/// span is exactly what the document says.
///
/// The match must start at a WORD BOUNDARY: `name` may not be the tail of a
/// longer token, or "AlSi10Mg (AM)" would count as a defining parenthetical
/// for the pair ("Mg", "AM") — magnesium declared identical to additive
/// manufacturing, with an "evidence" slice cut out of the middle of a
/// different name. The right edge needs no check — the literal `(` and `)`
/// anchor the alias.
fn parenthetical_span(name: &str, alias: &str, document: &str) -> Option<String> {
    let needle = format!("{name} ({alias})").to_ascii_lowercase();
    let haystack = document.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(offset) = haystack[from..].find(&needle) {
        let start = from + offset;
        let boundary = document[..start]
            .chars()
            .next_back()
            .is_none_or(|before| !before.is_alphanumeric());
        if boundary {
            return Some(document[start..start + needle.len()].to_string());
        }
        from = start + 1;
    }
    None
}

fn build_alias_prompt(names: &[String]) -> String {
    let list: String = names.iter().map(|name| format!("- {name}\n")).collect();
    format!(
        r#"You are an alias auditor for materials-science entity names.

Below are entity NAMES extracted from one document. Propose pairs that are two spellings of the SAME entity — an abbreviation, a shorthand, or a formatting variant (e.g. "Ti-6Al-4V" and "Ti64"). Do NOT pair distinct entities that are merely related, compared, or co-occurring: "IN718" and "IN625" are two different alloys, never aliases.

NAMES:
{list}
Reply with ONLY this JSON (an empty list is a valid answer; use names from the list verbatim):
{{"aliases":[{{"a":"Ti-6Al-4V","b":"Ti64"}}]}}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    struct Scripted {
        responses: Vec<String>,
        calls: AtomicUsize,
    }

    impl Respond for Scripted {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            let content = self
                .responses
                .get(index.min(self.responses.len().saturating_sub(1)))
                .cloned()
                .unwrap_or_default();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {"role": "assistant", "content": content},
                    "finish_reason": "stop"
                }]
            }))
        }
    }

    async fn scripted_client(responses: Vec<String>) -> (MockServer, LlmClient) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(Scripted {
                responses,
                calls: AtomicUsize::new(0),
            })
            .mount(&server)
            .await;
        let client = LlmClient::new(prism_llm::LlmConfig {
            base_url: format!("{}/v1", server.uri()),
            model: "test-alias".into(),
            ..Default::default()
        });
        (server, client)
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// THE load-bearing contrast: a parenthetical definition is accepted with
    /// the document's own span as evidence; two names that merely share a
    /// sentence — "IN718 outperformed IN625" — are REJECTED. Co-occurrence is
    /// not alias evidence, and a wrong merge is worse than no merge.
    #[tokio::test]
    async fn parenthetical_is_accepted_and_co_occurrence_is_rejected() {
        let document = "The alloy Ti-6Al-4V (Ti64) was printed by L-PBF.\n\
                        IN718 outperformed IN625 in creep resistance.";
        let (_server, llm) = scripted_client(vec![
            r#"{"aliases":[{"a":"Ti-6Al-4V","b":"Ti64"},{"a":"IN718","b":"IN625"}]}"#.into(),
        ])
        .await;

        let pass = link_aliases(
            &llm,
            &names(&["Ti-6Al-4V", "Ti64", "IN718", "IN625"]),
            document,
        )
        .await;

        assert!(pass.error.is_none(), "{:?}", pass.error);
        assert_eq!(pass.accepted.len(), 1, "{:?}", pass.accepted);
        let accepted = &pass.accepted[0];
        assert_eq!(accepted.fact.subject, "Ti-6Al-4V");
        assert_eq!(accepted.fact.predicate, SAME_AS_PREDICATE);
        assert_eq!(accepted.fact.object, "Ti64");
        assert_eq!(accepted.fact.value, None, "same_as edges carry no value");
        assert_eq!(accepted.fact.kind, None, "an edge, never a measurement");
        match &accepted.evidence {
            AliasEvidence::DefiningSpan(span) => {
                assert_eq!(
                    span, "Ti-6Al-4V (Ti64)",
                    "evidence is the document's own words"
                );
            }
            other => panic!("expected a defining span, got {other:?}"),
        }

        assert_eq!(pass.rejected.len(), 1, "{:?}", pass.rejected);
        assert_eq!(pass.rejected[0].a, "IN718");
        assert_eq!(pass.rejected[0].b, "IN625");
        assert!(
            pass.rejected[0].reason.contains("co-occurrence"),
            "the rejection must say why: {}",
            pass.rejected[0].reason
        );
    }

    /// Unicode-dash spellings of one name are accepted by CODE alone —
    /// deterministic normalisation, no document span required.
    #[tokio::test]
    async fn dash_normalisation_is_accepted_without_a_span() {
        // En dashes vs ASCII hyphens; the document never defines them —
        // normalisation is the evidence.
        let (_server, llm) = scripted_client(vec![
            "{\"aliases\":[{\"a\":\"Ti\u{2013}6Al\u{2013}4V\",\"b\":\"Ti-6Al-4V\"}]}".into(),
        ])
        .await;
        let pass = link_aliases(
            &llm,
            &names(&["Ti\u{2013}6Al\u{2013}4V", "Ti-6Al-4V"]),
            "unrelated text",
        )
        .await;
        assert_eq!(pass.accepted.len(), 1, "{:?}", pass.rejected);
        assert_eq!(pass.accepted[0].evidence, AliasEvidence::Normalisation);
        assert_eq!(pass.accepted[0].fact.confidence, Some(1.0));
    }

    /// "hereafter"/"denoted"/"also known as" spans are defining; the same
    /// two names WITHOUT a keyword are not.
    #[tokio::test]
    async fn definition_keywords_make_a_span_defining() {
        let reply = r#"{"aliases":[{"a":"Laser Powder Bed Fusion","b":"L-PBF"}]}"#;
        let (_server, llm) = scripted_client(vec![reply.into(), reply.into()]).await;
        let subjects = names(&["Laser Powder Bed Fusion", "L-PBF"]);

        let defined = "Laser Powder Bed Fusion, hereafter L-PBF, dominates metal AM.";
        let pass = link_aliases(&llm, &subjects, defined).await;
        assert_eq!(pass.accepted.len(), 1, "{:?}", pass.rejected);

        let merely_mentioned = "Laser Powder Bed Fusion and L-PBF studies disagree.";
        let pass = link_aliases(&llm, &subjects, merely_mentioned).await;
        assert!(pass.accepted.is_empty(), "{:?}", pass.accepted);
        assert_eq!(pass.rejected.len(), 1);
    }

    /// The model may only pair names it was handed: an invented name has no
    /// node to connect and is rejected, reported.
    #[tokio::test]
    async fn a_name_the_model_invented_is_rejected() {
        let (_server, llm) = scripted_client(vec![
            r#"{"aliases":[{"a":"Ti-6Al-4V","b":"Inventium"}]}"#.into(),
        ])
        .await;
        let pass = link_aliases(
            &llm,
            &names(&["Ti-6Al-4V", "Ti64"]),
            "Ti-6Al-4V (Inventium) — even a defining span cannot save it.",
        )
        .await;
        assert!(pass.accepted.is_empty(), "{:?}", pass.accepted);
        assert_eq!(pass.rejected.len(), 1);
        assert!(
            pass.rejected[0].reason.contains("extracted entity names"),
            "{}",
            pass.rejected[0].reason
        );
    }

    /// Fewer than two names: no call is made at all (the mock would panic on
    /// contact — the client points at a dead port).
    #[tokio::test]
    async fn fewer_than_two_names_skips_the_model_call() {
        let llm = LlmClient::new(prism_llm::LlmConfig {
            base_url: "http://127.0.0.1:9/v1".into(),
            model: "unused".into(),
            ..Default::default()
        });
        let pass = link_aliases(&llm, &names(&["Ti-6Al-4V"]), "text").await;
        assert!(pass.accepted.is_empty() && pass.rejected.is_empty());
        assert!(pass.error.is_none());
    }

    /// A failed call degrades the pass, never the ingest: error reported,
    /// nothing accepted.
    #[tokio::test]
    async fn a_failed_call_is_a_reported_degradation() {
        let llm = LlmClient::new(prism_llm::LlmConfig {
            base_url: "http://127.0.0.1:9/v1".into(),
            model: "unreachable".into(),
            ..Default::default()
        });
        let pass = link_aliases(&llm, &names(&["a", "b"]), "text").await;
        assert!(pass.accepted.is_empty());
        assert!(pass.error.is_some());
    }

    #[test]
    fn normalisation_folds_dashes_and_whitespace_only_never_case() {
        assert_eq!(
            normalise_name("Ti\u{2013}6Al\u{2014}4V"),
            normalise_name("Ti-6Al-4V")
        );
        assert_eq!(normalise_name("  Inconel   718 "), "Inconel 718");
        // Case is CHEMISTRY: cobalt is not carbon monoxide, indium is not
        // the Inconel prefix. The zero-evidence path must never fold case.
        assert_ne!(normalise_name("Co"), normalise_name("CO"));
        assert_ne!(normalise_name("In"), normalise_name("IN"));
        assert_ne!(normalise_name("Ti-6Al-4V"), normalise_name("ti-6al-4v"));
        // Distinct materials stay distinct; no abbreviation guessing.
        assert_ne!(normalise_name("IN718"), normalise_name("IN625"));
        assert_ne!(normalise_name("Ti-6Al-4V"), normalise_name("Ti64"));
    }

    /// Cobalt is not carbon monoxide, end to end: a case-only pair with no
    /// defining span in the document is REJECTED — the normalisation path
    /// accepts with zero document evidence, so it must never fold case.
    #[tokio::test]
    async fn a_case_only_pair_without_a_defining_span_is_rejected() {
        let (_server, llm) =
            scripted_client(vec![r#"{"aliases":[{"a":"Co","b":"CO"}]}"#.into()]).await;
        let pass = link_aliases(
            &llm,
            &names(&["Co", "CO"]),
            "Co catalysts were tested for CO oxidation at 250 C.",
        )
        .await;
        assert!(pass.accepted.is_empty(), "{:?}", pass.accepted);
        assert_eq!(pass.rejected.len(), 1, "{:?}", pass.rejected);
    }

    /// A keyword that defines a THIRD name must not bind an unrelated pair:
    /// "Ti-6Al-4V (hereafter Ti64), IN718, and IN625" contains both names
    /// and "hereafter" in one sentence — and the pair is still refused,
    /// because the keyword does not sit between THEM.
    #[tokio::test]
    async fn a_keyword_defining_a_third_name_does_not_bind_the_pair() {
        let document = "Ti-6Al-4V (hereafter Ti64), IN718, and IN625 were compared.";
        let reply = r#"{"aliases":[{"a":"IN718","b":"IN625"},{"a":"Ti-6Al-4V","b":"Ti64"}]}"#;
        let (_server, llm) = scripted_client(vec![reply.into()]).await;
        let pass = link_aliases(
            &llm,
            &names(&["Ti-6Al-4V", "Ti64", "IN718", "IN625"]),
            document,
        )
        .await;
        // The pair the keyword actually defines is accepted…
        assert_eq!(pass.accepted.len(), 1, "{:?}", pass.accepted);
        assert_eq!(pass.accepted[0].fact.subject, "Ti-6Al-4V");
        assert_eq!(pass.accepted[0].fact.object, "Ti64");
        // …and the bystander pair is rejected.
        assert_eq!(pass.rejected.len(), 1, "{:?}", pass.rejected);
        assert_eq!(pass.rejected[0].a, "IN718");
        assert_eq!(pass.rejected[0].b, "IN625");
    }

    /// The parenthetical must start at a word boundary: "AlSi10Mg (AM)" is
    /// a defining parenthetical for AlSi10Mg — never for the pair
    /// ("Mg", "AM"), whose "evidence" would be a slice cut out of the middle
    /// of a different name.
    #[tokio::test]
    async fn a_parenthetical_inside_a_longer_name_is_not_evidence() {
        let document = "AlSi10Mg (AM) samples were fabricated by laser powder bed fusion. \
                        The Mg content was 9 percent.";
        let reply = r#"{"aliases":[{"a":"Mg","b":"AM"},{"a":"AlSi10Mg","b":"AM"}]}"#;
        let (_server, llm) = scripted_client(vec![reply.into()]).await;
        let pass = link_aliases(&llm, &names(&["AlSi10Mg", "Mg", "AM"]), document).await;
        // The whole-token parenthetical is accepted…
        assert_eq!(pass.accepted.len(), 1, "{:?}", pass.accepted);
        assert_eq!(pass.accepted[0].fact.subject, "AlSi10Mg");
        // …the mid-token slice is not.
        assert_eq!(pass.rejected.len(), 1, "{:?}", pass.rejected);
        assert_eq!(pass.rejected[0].a, "Mg");
        assert_eq!(pass.rejected[0].b, "AM");
    }
}
