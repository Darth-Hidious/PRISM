//! Extracted claims typed for EMMO ingestion.
//!
//! This engine never invents claims: a claim exists only because an
//! extractor produced it from a located block of a real document, and every
//! claim must be verifiable against that exact block: a claim carries a
//! verbatim `quote`, and a claim whose quote does not occur in the block it
//! cites is dropped — a claim citing a document that does not support it is
//! a false provenance record, which is worse than no claim.
//!
//! The engine's job here is the contract and the stamping:
//!
//! * subject / predicate / object with value + QUDT unit
//! * measurement conditions (a number without conditions is not a property)
//! * provenance back to the exact document and locator
//! * an evidence class capped at `research` — literature extraction can
//!   never be `reference_validated` (mirrors `app/tools/evidence.py` and
//!   `prism_provenance::evidence_for_result(LiteratureExtraction)`)
//!
//! The schema is field-compatible with `prism_provenance::MaterialFact` so a
//! serialized claim deserializes into it at the ingest boundary. The
//! vocabulary strings are the stable machine contract ("indeterminate",
//! "research", "screening", "reference_validated").

use serde::{Deserialize, Serialize};

use crate::fulltext::Locator;

pub const EVIDENCE_INDETERMINATE: &str = "indeterminate";
pub const EVIDENCE_RESEARCH: &str = "research";
pub const EVIDENCE_SCREENING: &str = "screening";
pub const EVIDENCE_REFERENCE_VALIDATED: &str = "reference_validated";

fn rank(class: &str) -> u8 {
    match class {
        EVIDENCE_INDETERMINATE => 0,
        EVIDENCE_RESEARCH => 1,
        EVIDENCE_SCREENING => 2,
        EVIDENCE_REFERENCE_VALIDATED => 3,
        _ => 0, // unknown values are indeterminate, never trusted
    }
}

/// Apply the literature-extraction ceiling: the result can never outrank
/// `research`, and it can never outrank its input. Unknown inputs collapse
/// to indeterminate.
#[must_use]
pub fn cap_at_literature(claimed: &str) -> &'static str {
    if rank(claimed) <= rank(EVIDENCE_RESEARCH) && claimed == EVIDENCE_RESEARCH {
        EVIDENCE_RESEARCH
    } else if claimed == EVIDENCE_INDETERMINATE {
        EVIDENCE_INDETERMINATE
    } else if rank(claimed) >= rank(EVIDENCE_RESEARCH) {
        EVIDENCE_RESEARCH
    } else {
        EVIDENCE_INDETERMINATE
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConditionValue {
    Number(f64),
    Text(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasurementCondition {
    pub name: String,
    pub value: ConditionValue,
    /// QUDT unit identifier. REQUIRED for numeric conditions; a numeric
    /// condition without a unit is rejected, not defaulted.
    #[serde(default)]
    pub unit: Option<String>,
}

/// Where a claim came from — document plus locator, so a human can find the
/// sentence a number was read out of.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimProvenance {
    /// DOI / arXiv id / PMCID / source id of the document.
    pub document_id: String,
    pub document_url: String,
    /// Retrieval source that served the document.
    pub source: String,
    pub locator: Locator,
    /// Verbatim span the claim was read from, when the extractor supplies it.
    #[serde(default)]
    pub quote: Option<String>,
}

/// One extracted material claim, EMMO-shaped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedClaim {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default)]
    pub value: Option<f64>,
    /// QUDT unit identifier for `value`.
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub conditions: Vec<MeasurementCondition>,
    #[serde(default)]
    pub confidence: Option<f64>,
    /// EMMO kind hint: measurement | phase | composition | processing | ...
    #[serde(default)]
    pub kind: Option<String>,
    /// Always stamped through `cap_at_literature`; never trusted from input.
    pub evidence_class: String,
    pub provenance: ClaimProvenance,
}

/// Errors a claim can carry instead of being silently accepted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClaimRejection {
    NumericConditionWithoutUnit {
        condition: String,
    },
    NumericValueWithoutUnit,
    EmptySubject,
    /// The claim carried no verbatim quote, so nothing ties it to the block
    /// it cites. Unverifiable claims are dropped, never stamped.
    MissingQuote,
    /// The claim's quote does not occur in the block at its recorded
    /// locator. The citation is false; the claim is dropped, not downgraded.
    QuoteNotInCitedBlock,
    /// A span held the claim's subject or object and the value occurred in
    /// it, but a guard refused every occurrence: the matcher was too
    /// literal, the model did not hallucinate. Distinct from `MissingQuote`
    /// so over-refusal is measurable in the drop set. `guard` refused the
    /// last candidate occurrence scanned; `span` is the verbatim span that
    /// held it.
    NoEvidentialOccurrence {
        guard: RefusalGuard,
        span: String,
    },
}

/// The guard that refused a candidate occurrence of the value. Named so a
/// drop can say WHY a supporting span yielded no evidence — the drop set is
/// the only observable signal of how this gate behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefusalGuard {
    /// Endpoint of an en-dash digit range ("950\u{2013}1100"): the sentence
    /// asserts bounds, not a point value.
    Range,
    /// Token-boundary rule: continuation by digits/decimals/grouping, a
    /// leading minus that signs it, or a non-unit letter glued to it.
    Boundary,
    /// Inside a citation marker ("[1140]", "(1140)", "{1140}").
    Citation,
    /// Immediately after a label word (Table/Figure/Ref/...) or a
    /// continuation of such a list.
    Label,
    /// Inside an occurrence of the subject's or object's own name
    /// (the "718" of "Inconel 718").
    InsideName,
}

/// Why no supporting span was found. `NoSpan` reads as the model's fault
/// (the block does not mention the fact at all); `Guarded` is the matcher's
/// refusal and carries the guard that refused the last candidate occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SupportRefusal {
    /// No span held the subject or the object (both, for a non-numeric
    /// fact): there was nothing to scan.
    NoSpan,
    /// A span held the subject or object and at least one occurrence of the
    /// value, but every occurrence was refused by a guard.
    Guarded { guard: RefusalGuard, span: String },
}

impl From<SupportRefusal> for ClaimRejection {
    fn from(refusal: SupportRefusal) -> Self {
        match refusal {
            SupportRefusal::NoSpan => Self::MissingQuote,
            SupportRefusal::Guarded { guard, span } => Self::NoEvidentialOccurrence { guard, span },
        }
    }
}

/// Validate and stamp one claim against `block_text`, the text of the block
/// the claim cites. Returns the stamped claim or the reason it was refused —
/// refused claims are reported, not dropped silently and not fixed by
/// guessing.
///
/// Containment is part of the contract: the claim must carry a verbatim
/// quote and that quote must occur in `block_text`. An extractor's output
/// (including prompt examples it parrots back) cannot survive as a claim
/// about a document that does not contain it.
pub fn validate_and_stamp(
    mut claim: ExtractedClaim,
    block_text: &str,
) -> Result<ExtractedClaim, ClaimRejection> {
    if claim.subject.trim().is_empty() {
        return Err(ClaimRejection::EmptySubject);
    }
    if claim.value.is_some() && claim.unit.is_none() {
        return Err(ClaimRejection::NumericValueWithoutUnit);
    }
    for condition in &claim.conditions {
        if matches!(condition.value, ConditionValue::Number(_)) && condition.unit.is_none() {
            return Err(ClaimRejection::NumericConditionWithoutUnit {
                condition: condition.name.clone(),
            });
        }
    }
    let Some(quote) = claim.provenance.quote.as_deref() else {
        return Err(ClaimRejection::MissingQuote);
    };
    if !quote_in_block(quote, block_text) {
        return Err(ClaimRejection::QuoteNotInCitedBlock);
    }
    claim.evidence_class = cap_at_literature(&claim.evidence_class).to_string();
    Ok(claim)
}

/// Whitespace-collapsed lowercase form used for containment comparisons.
/// Underscores become spaces so EMMO-style identifiers (`thermal_conductivity`)
/// match their prose form.
fn normalize_for_containment(s: &str) -> String {
    s.replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Does `quote` occur (modulo whitespace and case) in the cited block?
fn quote_in_block(quote: &str, block_text: &str) -> bool {
    let needle = normalize_for_containment(quote);
    !needle.is_empty() && normalize_for_containment(block_text).contains(&needle)
}

/// Find a verbatim span of `block_text` that supports the fact described by
/// (`subject`, `object`, `value`), or say WHY no span does. The distinction
/// matters: `SupportRefusal::NoSpan` means the block does not mention the
/// fact at all (the drop is the model's), while `SupportRefusal::Guarded`
/// means a span held the fact but a guard refused every occurrence of the
/// value (the drop is the matcher's) — without it, over-refusal is
/// invisible in the output.
///
/// Support criteria (all case-insensitive, within one sentence/row span):
/// * numeric fact: the value's number appears together with the subject or
///   the object (a bare number could be a citation, so the number alone is
///   not enough). The number must occur as its own token — not as a
///   substring of a longer number and not a digit of an alloy
///   designation, glued (Ti-6Al-4V) or spaced (the claim's own Inconel 718)
///   — and the occurrence must be evidential: a number inside
///   a citation marker `[...]` or immediately after Table/Figure/Ref is a
///   label, not a measurement;
/// * non-numeric fact: both subject and object appear.
pub fn supporting_quote_or_refusal(
    subject: &str,
    object: &str,
    value: Option<f64>,
    block_text: &str,
) -> Result<String, SupportRefusal> {
    let subject_n = normalize_for_containment(subject);
    let object_n = normalize_for_containment(object);
    let mut last_refusal: Option<(RefusalGuard, String)> = None;
    for span in supporting_spans(block_text) {
        let hay = normalize_for_containment(span);
        match value {
            Some(v) => {
                let name_near = (!subject_n.is_empty() && hay.contains(&subject_n))
                    || (!object_n.is_empty() && hay.contains(&object_n));
                if name_near {
                    match scan_number_evidence(&hay, v, &subject_n, &object_n) {
                        NumberScan::Evidential => return Ok(span.trim().to_string()),
                        NumberScan::Refused(guard) => {
                            last_refusal = Some((guard, span.trim().to_string()));
                        }
                        NumberScan::Absent => {}
                    }
                }
            }
            None => {
                if !subject_n.is_empty()
                    && !object_n.is_empty()
                    && hay.contains(&subject_n)
                    && hay.contains(&object_n)
                {
                    return Ok(span.trim().to_string());
                }
            }
        }
    }
    match last_refusal {
        Some((guard, span)) => Err(SupportRefusal::Guarded { guard, span }),
        None => Err(SupportRefusal::NoSpan),
    }
}

/// `supporting_quote_or_refusal` for callers that only need the quote.
#[must_use]
pub fn supporting_quote(
    subject: &str,
    object: &str,
    value: Option<f64>,
    block_text: &str,
) -> Option<String> {
    supporting_quote_or_refusal(subject, object, value, block_text).ok()
}

/// Split `block_text` into candidate supporting spans: sentences and table
/// rows. Spans are verbatim substrings (only trimmed), so anything found
/// here can be stored as a quote and later verified by containment.
fn supporting_spans(text: &str) -> Vec<&str> {
    let mut spans = Vec::new();
    for line in text.lines() {
        let bytes = line.as_bytes();
        let mut start = 0usize;
        for (i, b) in bytes.iter().enumerate() {
            // Do not break sentences on the decimal point of a number.
            let is_decimal_point = b == &b'.'
                && i > 0
                && i + 1 < bytes.len()
                && bytes[i - 1].is_ascii_digit()
                && bytes[i + 1].is_ascii_digit();
            let is_sentence_break = matches!(b, b'.' | b'!' | b'?' | b';')
                && !is_decimal_point
                && !(*b == b'.' && period_ends_abbreviation(line, i));
            if is_sentence_break {
                spans.push(&line[start..=i]);
                start = i + 1;
            }
        }
        if start < line.len() {
            spans.push(&line[start..]);
        }
    }
    spans
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Words whose trailing period is an abbreviation, not a sentence end
/// ("Fig.", "Ref.", "Eq."). Deliberately NOT `LABEL_WORDS`: words like
/// "sample." or "run." legitimately end sentences in methods prose, and
/// refusing to split there would fuse two sentences into one span and
/// create fresh false co-occurrences.
const ABBREV_LABEL_WORDS: &[&str] = &["fig", "figs", "ref", "refs", "eq", "eqs"];

/// Does the word right before the period at byte index `dot` end in a
/// label abbreviation? Such a period does not end a span: splitting on it
/// strands the label's number in a fresh span where the label word is
/// invisible, so "Fig. 2" stamps 2 as a measurement (H7).
fn period_ends_abbreviation(line: &str, dot: usize) -> bool {
    let word: String = line[..dot]
        .chars()
        .rev()
        .take_while(|c: &char| c.is_alphanumeric())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>()
        .to_lowercase();
    ABBREV_LABEL_WORDS.contains(&word.as_str())
}

/// What a scan of the value's occurrences in one span found.
enum NumberScan {
    /// One occurrence is evidential.
    Evidential,
    /// No needle form of the value occurs in the span at all.
    Absent,
    /// Every occurrence was refused; holds the guard that refused the LAST
    /// candidate occurrence scanned.
    Refused(RefusalGuard),
}

/// Does `hay` contain the value as an evidential measurement? At least one
/// string form of the value must occur with clean token boundaries, must
/// not be a citation marker or a Table/Figure/Ref label number, must not
/// sit inside an occurrence of the subject's or object's own name (the
/// "718" of "Inconel 718"), and must not be an en-dash range endpoint.
/// When every occurrence is refused, the scan names the guard that refused
/// the last one — that name is what makes an over-refusal actionable.
fn scan_number_evidence(hay: &str, value: f64, subject_n: &str, object_n: &str) -> NumberScan {
    let mut last: Option<RefusalGuard> = None;
    for needle in number_needles(value) {
        let mut search_from = 0usize;
        while let Some(rel) = hay[search_from..].find(&needle) {
            let start = search_from + rel;
            let end = start + needle.len();
            match refusing_guard(hay, &needle, start, end, subject_n, object_n) {
                None => return NumberScan::Evidential,
                Some(guard) => last = Some(guard),
            }
            // Advance by the needle's first CHARACTER, not one byte: U+2212
            // needles lead with a 3-byte char, and a rejected occurrence that
            // advanced one byte landed the next hay[search_from..] slice
            // inside the minus (char-boundary panic, whole ingest aborted).
            // The match guarantees the needle sits at `start`, so its first
            // char is the char to skip; map_or(1, ..) keeps the loop
            // terminating even for a hypothetical empty needle.
            search_from = start + needle.chars().next().map_or(1, char::len_utf8);
        }
    }
    match last {
        Some(guard) => NumberScan::Refused(guard),
        None => NumberScan::Absent,
    }
}

/// The guard that refuses the occurrence of `needle` at [start, end), or
/// `None` when the occurrence is evidential. Checked most-specific first so
/// the NAMED refusal is the most informative one; the refuse/accept decision
/// itself does not depend on the order.
fn refusing_guard(
    hay: &str,
    needle: &str,
    start: usize,
    end: usize,
    subject_n: &str,
    object_n: &str,
) -> Option<RefusalGuard> {
    if en_dash_range_endpoint(hay, start, end) {
        return Some(RefusalGuard::Range);
    }
    if !clean_number_boundary(hay, needle, start, end) {
        return Some(RefusalGuard::Boundary);
    }
    if inside_citation_marker(hay, start) {
        return Some(RefusalGuard::Citation);
    }
    if preceding_word_is_label(hay, start, end) {
        return Some(RefusalGuard::Label);
    }
    if occurrence_inside_name(hay, start, end, subject_n)
        || occurrence_inside_name(hay, start, end, object_n)
    {
        return Some(RefusalGuard::InsideName);
    }
    None
}

/// Does the occurrence at [start, end) sit inside an occurrence of the
/// claim's own subject/object name? Space-separated designations like
/// "Inconel 718" have clean token boundaries around their trailing digits,
/// so the boundary rule cannot tell the "718" of the name from a measured
/// 718; positional containment inside the name can. An occurrence that
/// repeats OUTSIDE the name is still evidence.
fn occurrence_inside_name(hay: &str, start: usize, end: usize, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut search_from = 0usize;
    while let Some(rel) = hay[search_from..].find(name) {
        let name_start = search_from + rel;
        let name_end = name_start + name.len();
        if name_start <= start && end <= name_end {
            return true;
        }
        // Char-not-byte advance, as in evidential_number_occurrence:
        // names may lead with a multi-byte char ("\u{3b1}-phase").
        search_from = name_start + name.chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// Letters that may begin a unit token glued directly to a number in
/// table and PDF-extracted text where the space was lost: "950MPa",
/// "1073K", "50um" / "50\u{b5}m", "5wt%". Deliberately an allow-list,
/// not every letter: digit-then-letter gluing like "950x" (magnification)
/// or "2e5" (scientific notation) is not number+unit and stays rejected,
/// so 'e' and 'x' are absent on purpose. '\u{b5}' is present because
/// U+00B5 MICRO SIGN is alphabetic.
const UNIT_INITIALS: &[char] = &[
    'a', 'c', 'd', 'f', 'g', 'h', 'j', 'k', 'l', 'm', 'n', 'p', 's', 't', 'u', 'v', 'w', '\u{b5}',
];

/// Token-boundary check: the occurrence must not be adjacent to a digit, to
/// a decimal point that continues it, to a digit-adjacent comma that
/// continues a grouped number ("1,140" is one number, in both directions),
/// to a leading minus that signs it ("-950" / "\u{2212}950" are one number),
/// or to an alphanumeric. Otherwise "95" matches inside "950", "1.5" inside
/// "11.5", "140" inside "1,140", and the "6" of "Ti-6Al-4V". After the
/// number, a letter from `UNIT_INITIALS` is allowed so glued units
/// ("950MPa") still stamp. En-dash range endpoints are refused by
/// `en_dash_range_endpoint`, not here.
fn clean_number_boundary(hay: &str, needle: &str, start: usize, end: usize) -> bool {
    if let Some(before) = hay[..start].chars().next_back() {
        if before.is_alphanumeric() {
            return false;
        }
        if matches!(before, '-' | '\u{2212}') {
            // A leading minus is part of the number: an unsigned needle
            // must not match the digits of a signed token ("950" inside
            // "-950" or "\u{2212}950"), or the sign-flipped claim stamps
            // as fact — for residual stress that turns compressive into
            // tensile, worse than a miss. The minus is a sign only when
            // it does not join a compound: whitespace, line start or
            // opening punctuation before it. A letter or digit before it
            // is the hyphen of a designation ("ti-6al-4v") or a
            // digit-joined compound, which the designation guards own.
            let joins_compound = hay[..start - before.len_utf8()]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric());
            if !joins_compound {
                return false;
            }
        }
        if before == '.' && needle.starts_with(|c: char| c.is_ascii_digit()) {
            return false;
        }
        if before == ',' && hay[..start - before.len_utf8()].ends_with(|c: char| c.is_ascii_digit())
        {
            return false;
        }
    }
    if let Some(after) = hay[end..].chars().next() {
        if after.is_alphanumeric() {
            if !UNIT_INITIALS.contains(&after) {
                return false;
            }
            // A glued unit letter redeems a number, but not a digit that
            // a LETTER dash-joins into a designation (the "6" of
            // "Ti-6Al-4V", ASCII or en/em dash). A dash preceded by a
            // digit joins numeric runs instead, and the second number
            // keeps its unit: "30-50um" layers and "5-10mm" grains are
            // measurements — refusing the high endpoint while the low
            // one stamped was the asymmetry that dropped the two
            // most-quoted LPBF numbers. A dash leading the span (no char
            // before it) keeps the refusal. Deletion of this clause is
            // mutation-proven by the en-dash object-arm case in
            // digit_dash_ranges_with_glued_units_stamp_the_high_endpoint:
            // with it gone, the "6" of Ti\u{2013}6Al\u{2013}4V stamps.
            if let Some(before) = hay[..start].chars().next_back()
                && matches!(before, '-' | '\u{2013}' | '\u{2014}')
                && hay[..start - before.len_utf8()]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !c.is_ascii_digit())
            {
                return false;
            }
        }
        if after == '.' && hay[end + 1..].starts_with(|c: char| c.is_ascii_digit()) {
            return false;
        }
        if after == ',' && hay[end + after.len_utf8()..].starts_with(|c: char| c.is_ascii_digit()) {
            return false;
        }
    }
    true
}

/// En-dash range endpoints: "950\u{2013}1100 MPa" asserts a range, not
/// two point values, so a number that opens or closes a digit/en-dash/digit
/// run is refused. U+2013 only, by decision: ASCII hyphens also join
/// genuine compounds ("950-1100" batch designators, catalogue numbers) and
/// the two readings are structurally indistinguishable, so the
/// compound-friendly behaviour is kept; em-dashes are sentence dashes, not
/// range dashes. Recorded residual gaps: spaced ranges, ASCII-typed ranges
/// and negative ranges still stamp their endpoints.
fn en_dash_range_endpoint(hay: &str, start: usize, end: usize) -> bool {
    if let Some(rest) = hay[end..].strip_prefix('\u{2013}')
        && rest.starts_with(|c: char| c.is_ascii_digit())
    {
        return true;
    }
    if let Some(prefix) = hay[..start].strip_suffix('\u{2013}')
        && prefix.ends_with(|c: char| c.is_ascii_digit())
    {
        return true;
    }
    false
}

/// Is the occurrence inside a citation marker? Bracketed styles
/// (`[1140]`, `[11, 12]`, `[11–13]`) and the paren/brace styles that
/// survive as bare numbers in text (`(1140)`, `{1140, 1141}`). Walk back
/// over the digits and separators a citation range may contain; if the
/// first other character is `[`, the number is a citation, not a
/// measurement. `(` / `{` count only when the marker also CLOSES before
/// any non-number text: "(950 MPa)" is a parenthesized value, not a
/// citation, so a bracket-class rule that only looks backwards would
/// refuse legitimate values.
fn inside_citation_marker(hay: &str, start: usize) -> bool {
    let prefix = hay[..start].trim_end_matches(|c: char| {
        c.is_ascii_digit() || matches!(c, ',' | ' ' | '-' | '\u{2013}' | '\u{2014}')
    });
    let Some(open) = prefix.chars().next_back() else {
        return false;
    };
    match open {
        '[' => true,
        '(' | '{' => {
            let close = if open == '(' { ')' } else { '}' };
            let after = hay[start..].trim_start_matches(|c: char| {
                c.is_ascii_digit() || matches!(c, ',' | ' ' | '-' | '\u{2013}' | '\u{2014}')
            });
            after.starts_with(close)
        }
        _ => false,
    }
}

/// Words after which a number is a label, never a measurement.
const LABEL_WORDS: &[&str] = &[
    "table",
    "tables",
    "figure",
    "figures",
    "fig",
    "figs",
    "ref",
    "refs",
    "reference",
    "references",
    "section",
    "sections",
    "eq",
    "eqs",
    "equation",
    "equations",
    "chapter",
    "chapters",
    "sample",
    "samples",
    "run",
    "runs",
    "entry",
    "entries",
    "scheme",
    "schemes",
];

/// Words that continue a label list: the number after one of these is a
/// label when the number one step back sits a label word ("Tables 1 and 2").
const LIST_CONTINUATIONS: &[&str] = &["and", "or", "to", "through"];

/// Units a list's last item can carry. The curated set is deliberately
/// the vocabulary of measurements: reference lists never carry units, so
/// a chain that ends in one of these is a VALUE list and the walk-back
/// to a label word must stop. A token matches only at a token boundary,
/// so prose words that merely start with a unit ("for", "uts") never
/// match.
const UNIT_TOKENS: &[&str] = &[
    // pressure / stress / hardness
    "pa", "kpa", "mpa", "gpa", "tpa", "bar", "mbar", "kbar", "atm", "torr", "psi", "ksi", "hv",
    "hrc", "hrb", // force, length, mass
    "n", "kn", "mn", "gn", "m", "mm", "cm", "nm", "um", "\u{b5}m", "pm", "km", "g", "mg", "kg",
    // time, temperature
    "s", "ms", "ns", "ps", "min", "h", "k", "\u{b0}c", "\u{b0}f",
    // energy, power, frequency
    "j", "kj", "mj", "gj", "ev", "kev", "mev", "gev", "tev", "w", "mw", "kw", "hz", "khz", "mhz",
    "ghz", "thz", "rpm", // electrical, magnetic
    "v", "mv", "kv", "a", "ma", "ohm", "t", // fractions
    "%", "wt%", "at%", "vol%", "mol", "ppm", "ppb",
];

/// Does the text after `end` carry a unit for the number — one optional
/// space, then a unit token at a token boundary ("970 mpa", "1140mpa")?
/// `hay` is normalized: lowercase, single spaces.
fn unit_follows(hay: &str, end: usize) -> bool {
    let rest = hay[end..].strip_prefix(' ').unwrap_or(&hay[end..]);
    UNIT_TOKENS.iter().any(|u| {
        rest.strip_prefix(u)
            .is_some_and(|tail| tail.chars().next().is_none_or(|c| !c.is_alphanumeric()))
    })
}

/// Byte length of the leading number in `s`, reading through
/// thousands-grouping commas ("1,140"). A comma followed by a space is a
/// list separator, not grouping, and ends the number.
fn leading_number_len(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len()
        && (bytes[i].is_ascii_digit()
            || (bytes[i] == b',' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)))
    {
        i += 1;
    }
    i
}

/// Does the list chain continuing after `end` — ", <number>" items and
/// "<and|or|to|through> <number>" steps — close with a unit? A chain that
/// ends in a unit is a value list ("950, 960 and 970 MPa"); reference
/// lists never carry one ("Refs. 25, 26 and 27"). A unit directly after
/// the occurrence is a chain of length zero and counts too.
fn chain_ends_in_unit(hay: &str, end: usize) -> bool {
    let mut pos = end;
    loop {
        if unit_follows(hay, pos) {
            return true;
        }
        let rest = &hay[pos..];
        if let Some(item) = rest.strip_prefix(", ") {
            let n = leading_number_len(item);
            if n == 0 {
                return false;
            }
            pos += ", ".len() + n;
        } else if let Some(item) = rest.strip_prefix(' ')
            && let Some(conj) = LIST_CONTINUATIONS
                .iter()
                .find(|c| item.starts_with(*c) && item.as_bytes().get(c.len()) == Some(&b' '))
        {
            let skip = 1 + conj.len() + 1;
            let n = leading_number_len(&rest[skip..]);
            if n == 0 {
                return false;
            }
            pos += skip + n;
        } else {
            return false;
        }
    }
}

/// Step back over ", <number>" items one number at a time; the word at
/// the head of the list decides.
fn walk_comma_items(prefix: &mut String, word: &mut String) {
    while word.is_empty() && prefix.ends_with(',') {
        let before_comma = prefix[..prefix.len() - 1].trim_end_matches(' ');
        let number = trailing_word(before_comma);
        if number.is_empty() || !number.chars().all(|c: char| c.is_ascii_digit()) {
            break;
        }
        *prefix = before_comma[..before_comma.len() - number.len()]
            .trim_end_matches([' ', '.', ':'])
            .to_string();
        *word = trailing_word(prefix);
    }
}

/// Byte length of the trailing run of digits and range dashes in
/// `prefix` ("25\u{2013}27", "26"), or 0 when it holds no digit. One
/// conjunction step consumes exactly one such run — the old unbounded
/// trim that ate every digit, comma and space backwards is gone.
fn trailing_digit_run_len(prefix: &str) -> usize {
    let mut len = 0;
    let mut saw_digit = false;
    for c in prefix.chars().rev() {
        if c.is_ascii_digit() {
            saw_digit = true;
            len += c.len_utf8();
        } else if matches!(c, '-' | '\u{2013}' | '\u{2014}') {
            len += c.len_utf8();
        } else {
            break;
        }
    }
    if saw_digit { len } else { 0 }
}

/// Does the occurrence sit right after Table/Figure/Ref ("Table 1",
/// "Figure 2", "Ref. 25")? Such a number labels a document object; it is
/// not evidence for a property value. Continuations of a label list are
/// caught by stepping back over them to the head word: ", <number>"
/// items repeatedly ("Refs. 25, 26"), then one conjunction and one
/// number-run ("Tables 1 and 2", "Refs. 25\u{2013}27 and 28").
///
/// The walk runs only for REFERENCE lists: the chain continuing after
/// the occurrence decides. It ends in a unit -> value list -> the label
/// word before it is just the sentence's locator ("In Table 5, 950, 960
/// and 970 MPa"), and the walk must not reach it; no unit -> reference
/// list -> walk to the head word ("Refs. 25, 26 and 27"). The unit is
/// the discriminator the walk never looked at; without it the walk
/// stepped from a value back over the locator label and dropped every
/// value in the list.
fn preceding_word_is_label(hay: &str, start: usize, end: usize) -> bool {
    let mut prefix = hay[..start].trim_end_matches([' ', '.', ':']).to_string();
    let mut word = trailing_word(&prefix);
    if !chain_ends_in_unit(hay, end) {
        walk_comma_items(&mut prefix, &mut word);
        if LIST_CONTINUATIONS.contains(&word.as_str()) {
            prefix = prefix[..prefix.len() - word.len()]
                .trim_end_matches(' ')
                .to_string();
            let run = trailing_digit_run_len(&prefix);
            if run > 0 {
                prefix = prefix[..prefix.len() - run]
                    .trim_end_matches([' ', '.', ':'])
                    .to_string();
                word = trailing_word(&prefix);
                walk_comma_items(&mut prefix, &mut word);
            } else {
                word = trailing_word(&prefix);
            }
        }
    }
    LABEL_WORDS.contains(&word.as_str())
}

/// The trailing alphanumeric word of `prefix`, empty when `prefix` ends
/// in a non-word character.
fn trailing_word(prefix: &str) -> String {
    prefix
        .chars()
        .rev()
        .take_while(|c: &char| c.is_alphanumeric())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// String forms under which a numeric value may legitimately appear in a
/// paper: the plain rendering plus the comma-grouped integer form. A
/// negative value appears under both minus glyphs, ASCII hyphen and
/// U+2212 MINUS SIGN (the glyph typeset PDFs carry); without both, the
/// true negative claim is dropped while its sign-flipped twin stamps.
fn number_needles(value: f64) -> Vec<String> {
    let plain = format!("{value}");
    let mut out = vec![plain.clone()];
    if value < 0.0 && value.fract() != 0.0 {
        // Negative decimals never reach the grouped branch below, so this
        // is their only U+2212 rendering.
        out.push(format!("\u{2212}{}", &plain[1..]));
    }
    if value.fract() == 0.0 && value.abs() < 1e15 {
        let digits = (value as i64).abs().to_string();
        let grouped: String = digits
            .chars()
            .rev()
            .enumerate()
            .flat_map(|(i, c)| {
                let mut v = Vec::with_capacity(2);
                if i > 0 && i % 3 == 0 {
                    v.push(',');
                }
                v.push(c);
                v
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let signs: &[&str] = if value < 0.0 {
            &["-", "\u{2212}"]
        } else {
            &[""]
        };
        for sign in signs {
            let needle = format!("{sign}{grouped}");
            if !out.contains(&needle) {
                out.push(needle);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fulltext::{BlockKind, Locator};

    fn locator() -> Locator {
        Locator {
            kind: BlockKind::Body,
            section_path: vec!["2. Results".to_string()],
            label: None,
            char_offset: 0,
        }
    }

    const BLOCK: &str = "We measured CoCrFeNi. Its thermal conductivity is 11.5 W/(m K) \
                         at room temperature.";
    const QUOTE: &str = "thermal conductivity is 11.5 W/(m K)";

    fn claim(value: Option<f64>, unit: Option<&str>, evidence: &str) -> ExtractedClaim {
        claim_with_quote(value, unit, evidence, Some(QUOTE))
    }

    fn claim_with_quote(
        value: Option<f64>,
        unit: Option<&str>,
        evidence: &str,
        quote: Option<&str>,
    ) -> ExtractedClaim {
        ExtractedClaim {
            subject: "CoCrFeNi".to_string(),
            predicate: "has_measurement".to_string(),
            object: "thermal_conductivity".to_string(),
            value,
            unit: unit.map(str::to_string),
            conditions: vec![],
            confidence: Some(0.9),
            kind: Some("measurement".to_string()),
            evidence_class: evidence.to_string(),
            provenance: ClaimProvenance {
                document_id: "10.1234/hea".to_string(),
                document_url: "https://doi.org/10.1234/hea".to_string(),
                source: "openalex".to_string(),
                locator: locator(),
                quote: quote.map(str::to_string),
            },
        }
    }

    /// The extractor prompt's own Ti-6Al-4V example fact, verbatim
    /// (crates/ingest/src/text_extract.rs). It is the canonical fabrication
    /// vector: a model parroting the example back must never produce a
    /// stamped claim pointing at a document that does not contain it.
    fn prompt_example_claim() -> ExtractedClaim {
        ExtractedClaim {
            subject: "Ti-6Al-4V".to_string(),
            predicate: "has_measurement".to_string(),
            object: "UTS".to_string(),
            value: Some(1140.0),
            unit: Some("QUDT:MegaPA".to_string()),
            conditions: vec![
                MeasurementCondition {
                    name: "temperature".to_string(),
                    value: ConditionValue::Number(298.15),
                    unit: Some("QUDT:K".to_string()),
                },
                MeasurementCondition {
                    name: "atmosphere".to_string(),
                    value: ConditionValue::Text("air".to_string()),
                    unit: None,
                },
            ],
            confidence: Some(0.9),
            kind: Some("measurement".to_string()),
            evidence_class: "research".to_string(),
            provenance: ClaimProvenance {
                document_id: "10.1234/unrelated".to_string(),
                document_url: "https://doi.org/10.1234/unrelated".to_string(),
                source: "openalex".to_string(),
                locator: locator(),
                quote: None,
            },
        }
    }

    #[test]
    fn literature_can_never_be_promoted() {
        assert_eq!(cap_at_literature("reference_validated"), "research");
        assert_eq!(cap_at_literature("screening"), "research");
        assert_eq!(cap_at_literature("research"), "research");
        assert_eq!(cap_at_literature("indeterminate"), "indeterminate");
        assert_eq!(cap_at_literature("garbage"), "indeterminate");
    }

    #[test]
    fn stamped_claim_keeps_research_ceiling() {
        let promoted = claim(Some(11.5), Some("QUDT:W-PER-M-K"), "reference_validated");
        let stamped = validate_and_stamp(promoted, BLOCK).unwrap();
        assert_eq!(stamped.evidence_class, "research");
    }

    #[test]
    fn numeric_value_without_unit_is_refused() {
        let bare = claim(Some(11.5), None, "research");
        assert_eq!(
            validate_and_stamp(bare, BLOCK).unwrap_err(),
            ClaimRejection::NumericValueWithoutUnit
        );
    }

    #[test]
    fn numeric_condition_without_unit_is_refused() {
        let mut c = claim(Some(11.5), Some("QUDT:W-PER-M-K"), "research");
        c.conditions.push(MeasurementCondition {
            name: "temperature".to_string(),
            value: ConditionValue::Number(300.0),
            unit: None,
        });
        assert_eq!(
            validate_and_stamp(c, BLOCK).unwrap_err(),
            ClaimRejection::NumericConditionWithoutUnit {
                condition: "temperature".to_string()
            }
        );
    }

    #[test]
    fn textual_condition_without_unit_is_allowed() {
        let mut c = claim(Some(11.5), Some("QUDT:W-PER-M-K"), "research");
        c.conditions.push(MeasurementCondition {
            name: "atmosphere".to_string(),
            value: ConditionValue::Text("air".to_string()),
            unit: None,
        });
        assert!(validate_and_stamp(c, BLOCK).is_ok());
    }

    /// F2 regression. The reviewer fed the extractor prompt's own Ti-6Al-4V
    /// example through the validator and it was accepted at confidence 0.9
    /// with a locator pointing at a document that never contained it. The
    /// containment contract must refuse it: without a quote it is
    /// unverifiable, and even the example text itself is not in the block.
    #[test]
    fn prompt_example_fact_is_dropped_not_stamped() {
        let unrelated_block = "We report a novel magnesium alloy with 60 HV hardness.";

        // As the reviewer reproduced it: no quote at all.
        let as_reviewed = prompt_example_claim();
        assert_eq!(as_reviewed.confidence, Some(0.9));
        assert_eq!(
            validate_and_stamp(as_reviewed, unrelated_block).unwrap_err(),
            ClaimRejection::MissingQuote
        );

        // Even attaching the example sentence as a quote cannot fake
        // containment: the cited block does not contain it.
        let mut with_quote = prompt_example_claim();
        with_quote.provenance.quote =
            Some("Ti-6Al-4V has a UTS of 1140 MPa at 298.15 K in air".to_string());
        assert_eq!(
            validate_and_stamp(with_quote, unrelated_block).unwrap_err(),
            ClaimRejection::QuoteNotInCitedBlock
        );

        // And the supporting-quote finder finds nothing for it in the block.
        assert!(supporting_quote("Ti-6Al-4V", "UTS", Some(1140.0), unrelated_block).is_none());
    }

    #[test]
    fn claim_whose_quote_is_not_in_its_block_is_dropped() {
        let mut c = claim(Some(11.5), Some("QUDT:W-PER-M-K"), "research");
        c.provenance.quote = Some("thermal conductivity is 99 W/(m K)".to_string());
        assert_eq!(
            validate_and_stamp(c, BLOCK).unwrap_err(),
            ClaimRejection::QuoteNotInCitedBlock
        );
    }

    #[test]
    fn claim_without_quote_is_dropped_not_stamped() {
        let bare = claim_with_quote(Some(11.5), Some("QUDT:W-PER-M-K"), "research", None);
        assert_eq!(
            validate_and_stamp(bare, BLOCK).unwrap_err(),
            ClaimRejection::MissingQuote
        );
    }

    #[test]
    fn containment_tolerates_whitespace_and_case_only() {
        let mut c = claim(Some(11.5), Some("QUDT:W-PER-M-K"), "research");
        c.provenance.quote = Some("Thermal  Conductivity\nis 11.5 W/(m K)".to_string());
        assert!(validate_and_stamp(c, BLOCK).is_ok());
    }

    #[test]
    fn supporting_quote_finds_the_sentence_with_the_value() {
        let found = supporting_quote("CoCrFeNi", "thermal_conductivity", Some(11.5), BLOCK);
        assert_eq!(
            found.as_deref(),
            Some("Its thermal conductivity is 11.5 W/(m K) at room temperature.")
        );
    }

    #[test]
    fn supporting_quote_does_not_split_decimal_numbers() {
        let block = "See results. Conductivity of CoCrFeNi was 11.5 W/(m K). Done.";
        let found = supporting_quote("CoCrFeNi", "conductivity", Some(11.5), block);
        assert!(found.unwrap().contains("11.5"));
    }

    #[test]
    fn supporting_quote_needs_a_salient_token_beside_the_number() {
        // The number alone could be a citation number; it must appear with
        // the subject or the object to count as support.
        let block = "Discussion of prior work [1140] follows. No alloy data here.";
        assert!(supporting_quote("Ti-6Al-4V", "UTS", Some(1140.0), block).is_none());
    }

    #[test]
    fn supporting_quote_matches_comma_grouped_numbers() {
        let block = "The Ti-6Al-4V billet showed a UTS of 1,140 MPa.";
        assert!(supporting_quote("Ti-6Al-4V", "UTS", Some(1140.0), block).is_some());
    }

    #[test]
    fn non_numeric_fact_needs_subject_and_object_in_one_span() {
        let block = "The Ti-6Al-4V microstructure contained an alpha-beta phase.";
        assert!(supporting_quote("Ti-6Al-4V", "alpha-beta", None, block).is_some());
        // Object absent from the block: no support.
        assert!(supporting_quote("Ti-6Al-4V", "omega phase", None, block).is_none());
    }

    // ------------------------------------------------------------------
    // Fabrication-path regression tests. Each one reproduces a confirmed
    // false-stamp path and asserts the claim is DROPPED: no supporting
    // quote found, and therefore refused at validate_and_stamp exactly as
    // papers.rs runs it (supporting_quote -> quote None -> MissingQuote).
    // ------------------------------------------------------------------

    /// Mirrors the production flow in crates/cli/src/papers.rs: the quote
    /// is whatever `supporting_quote` finds in the block, and a claim with
    /// no quote must be refused by `validate_and_stamp`.
    fn assert_dropped_end_to_end(subject: &str, object: &str, value: f64, block: &str) {
        let quote = supporting_quote(subject, object, Some(value), block);
        assert!(
            quote.is_none(),
            "fabricated support found for {subject}/{object}={value}: {quote:?}"
        );
        let claim = ExtractedClaim {
            subject: subject.to_string(),
            predicate: "has_measurement".to_string(),
            object: object.to_string(),
            value: Some(value),
            unit: Some("QUDT:MegaPA".to_string()),
            conditions: vec![],
            confidence: Some(0.9),
            kind: Some("measurement".to_string()),
            evidence_class: "research".to_string(),
            provenance: ClaimProvenance {
                document_id: "10.1234/doc".to_string(),
                document_url: "https://doi.org/10.1234/doc".to_string(),
                source: "openalex".to_string(),
                locator: locator(),
                quote,
            },
        };
        assert_eq!(
            validate_and_stamp(claim, block).unwrap_err(),
            ClaimRejection::MissingQuote
        );
    }

    /// Fabrication path 2: substring number matching. The matched number
    /// must stand on its own token boundary, or an order-of-magnitude-wrong
    /// number passes the gate.
    #[test]
    fn substring_number_match_is_dropped() {
        let uts_block = "The Ti-6Al-4V sample showed a UTS of 950 MPa.";
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 95.0, uts_block);

        let conductivity_block = "The thermal conductivity of CoCrFeNi is 11.5 W/(m K).";
        assert_dropped_end_to_end("CoCrFeNi", "thermal_conductivity", 1.5, conductivity_block);

        // The genuine values in the same blocks still stamp.
        assert!(supporting_quote("Ti-6Al-4V", "UTS", Some(950.0), uts_block).is_some());
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "thermal_conductivity",
                Some(11.5),
                conductivity_block
            )
            .is_some()
        );
    }

    /// The two decimal-point guards in `clean_number_boundary` each do
    /// work no other guard covers: the fractional digit of "11.5" matches
    /// value 5 (only the BEFORE-'.' guard refuses it), and the integer
    /// part matches value 11 (only the AFTER-'.' guard refuses it). The
    /// pre-existing decimal case (1.5 vs 11.5) is caught by the
    /// alphanumeric guards instead, so without these two asserts both
    /// guards could be deleted with the whole suite green. Each guard is
    /// mutation-proven red independently.
    #[test]
    fn decimal_point_guards_refuse_partial_number_matches() {
        let block = "The CoCrFeNi conductivity is 11.5 W/(m K).";
        // The fractional digit: only the before-'.' guard refuses it.
        assert!(supporting_quote("CoCrFeNi", "conductivity", Some(5.0), block).is_none());
        // The integer part: only the after-'.' guard refuses it.
        assert!(supporting_quote("CoCrFeNi", "conductivity", Some(11.0), block).is_none());
        // Positive control: 11.5 itself still stamps.
        assert!(supporting_quote("CoCrFeNi", "conductivity", Some(11.5), block).is_some());
    }

    /// The sign is the finding: for residual stress, -950 vs +950 is the
    /// difference between compressive and tensile. Both halves of the
    /// round-4 repro are pinned. (a) The true negative claim stamps under
    /// U+2212 MINUS SIGN prose — killed by removing either U+2212 producer
    /// in `number_needles` (the decimal push owns the decimal assert; the
    /// signs loop owns the integer asserts). (b) The sign-flipped positive
    /// claim is dropped — killed by removing the before-minus guard in
    /// `clean_number_boundary`. The last assert pins the symmetry: a
    /// negative claim never stamps against positive prose either.
    #[test]
    fn negative_value_claims_match_negative_prose_and_refuse_the_flip() {
        let unicode_minus = "The residual stress in Ti-6Al-4V was \u{2212}950 MPa.";
        let ascii_minus = "The residual stress in Ti-6Al-4V was -950 MPa.";

        // The true negative claim stamps under both minus glyphs.
        assert_eq!(
            supporting_quote("Ti-6Al-4V", "residual_stress", Some(-950.0), unicode_minus)
                .as_deref(),
            Some(unicode_minus)
        );
        assert!(
            supporting_quote("Ti-6Al-4V", "residual_stress", Some(-950.0), ascii_minus).is_some()
        );
        // Grouped negative integer under U+2212 (the signs loop).
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "residual_stress",
                Some(-1140.0),
                "The residual stress in Ti-6Al-4V was \u{2212}1,140 MPa."
            )
            .is_some()
        );
        // Negative decimal under U+2212 (the decimal push).
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "seebeck_coefficient",
                Some(-11.5),
                "The CoCrFeNi Seebeck coefficient was \u{2212}11.5 uV/K."
            )
            .is_some()
        );

        // The sign-flipped claim is dropped, not stamped: the prose says
        // compressive, +950 says tensile. Both glyphs.
        assert_dropped_end_to_end("Ti-6Al-4V", "residual_stress", 950.0, unicode_minus);
        assert_dropped_end_to_end("Ti-6Al-4V", "residual_stress", 950.0, ascii_minus);

        // Symmetry: the negative claim against positive prose is dropped.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "residual_stress",
            -950.0,
            "The residual stress in Ti-6Al-4V was 950 MPa.",
        );

        // The compound condition pins the hyphen/minus distinction: a
        // hyphen that joins a digit compound is not a sign, so the
        // pre-round-4 behaviour of "950-1100" is unchanged. Removing the
        // joins_compound condition (making every preceding hyphen a sign)
        // turns this red.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1100.0),
                "The Ti-6Al-4V batches 950-1100 were tested."
            )
            .is_some()
        );
    }

    /// Round 5: after a REJECTED U+2212-prefixed needle, the scan must
    /// advance by the needle's first character, not one byte. "950x" is
    /// rejected because 'x' is deliberately not a unit initial; the old
    /// `start + 1` advance then landed `hay[search_from..]` inside the
    /// 3-byte U+2212 and panicked the whole ingest run on a non-char
    /// boundary. The correct outcome is a drop: the zoom factor is not
    /// evidence for a UTS of -950.
    #[test]
    fn rejected_unicode_minus_needle_advances_by_char_not_byte() {
        assert_eq!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(-950.0),
                "Ti-6Al-4V at \u{2212}950x zoom had UTS."
            ),
            None
        );
    }

    /// Round 5: same advance bug via the range-endpoint guard — a U+2212
    /// value that opens an en-dash range ("\u{2212}950\u{2013}1100 MPa")
    /// is rejected as a range endpoint, and the reject-then-advance must
    /// clear the 3-byte minus without slicing inside it.
    #[test]
    fn rejected_unicode_minus_range_endpoint_advances_by_char_not_byte() {
        assert_eq!(
            supporting_quote(
                "Ti-6Al-4V",
                "stress",
                Some(-950.0),
                "Ti-6Al-4V stress \u{2212}950\u{2013}1100 MPa."
            ),
            None
        );
    }

    /// Round 5 audit, same bug class: `occurrence_inside_name` also
    /// advanced one byte after a non-containing name match. A subject name
    /// that leads with a multi-byte char ("\u{3b1}-phase", U+03B1 GREEK
    /// SMALL LETTER ALPHA is 2 bytes) made the next slice panic. The
    /// occurrence is outside the name, so the value stamps.
    #[test]
    fn multibyte_leading_subject_name_does_not_panic_the_name_scan() {
        assert!(
            supporting_quote(
                "\u{3b1}-phase",
                "strength",
                Some(950.0),
                "\u{3b1}-phase strength was 950 MPa."
            )
            .is_some()
        );
    }

    /// F-1: a thousands comma adjacent to a digit is part of the number,
    /// in both directions. "1,140" must behave byte-for-byte like "1140":
    /// searching for 140 or 1 inside it finds nothing, exactly the
    /// guarantee `substring_number_match_is_dropped` asserts for 95/950.
    #[test]
    fn comma_grouped_number_digits_are_not_token_boundaries() {
        let block = "The Ti-6Al-4V UTS is 1,140 MPa.";
        assert!(supporting_quote("Ti-6Al-4V", "UTS", Some(140.0), block).is_none());
        assert!(supporting_quote("Ti-6Al-4V", "UTS", Some(1.0), block).is_none());
        // Control: the same fact written without grouping already drops 140.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(140.0),
                "The Ti-6Al-4V UTS is 1140 MPa."
            )
            .is_none()
        );
        // The real value still stamps in the grouped form.
        assert!(supporting_quote("Ti-6Al-4V", "UTS", Some(1140.0), block).is_some());

        assert!(supporting_quote("A", "UTS", Some(12.0), "A UTS is 12,345 MPa.").is_none());
        assert!(supporting_quote("A", "UTS", Some(345.0), "A UTS is 12,345 MPa.").is_none());
        assert!(supporting_quote("A", "UTS", Some(12345.0), "A UTS is 12,345 MPa.").is_some());

        // Row form: the leading "1" is not a standalone value either.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1.0),
                "Alloy UTS\nTi-6Al-4V 1,140\n..."
            )
            .is_none()
        );

        // A list comma is NOT number continuation: a value followed by a
        // comma and a space must still stamp (pins the digit-adjacency
        // condition against a blanket comma reject).
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(12.0),
                "The Ti-6Al-4V samples measured 12, 15 and 950 MPa."
            )
            .is_some()
        );
    }

    /// F-2: in materials tables and PDF-extracted text the space between
    /// a number and its unit is often lost. A number glued to a
    /// unit-initial letter must still stamp; a number glued to any other
    /// letter must not.
    #[test]
    fn numbers_glued_to_their_unit_still_stamp() {
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "The Ti-6Al-4V UTS is 950MPa."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "temperature",
                Some(1073.0),
                "Ti-6Al-4V was annealed at 1073K."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "grain_size",
                Some(50.0),
                "CoCrFeNi grains of 50um were observed."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "grain_size",
                Some(50.0),
                "CoCrFeNi grains of 50\u{b5}m were observed."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "content",
                Some(5.0),
                "The CoCrFeNi alloy contains 5wt% Cr."
            )
            .is_some()
        );

        // Degree/percent glue was never broken (non-alphanumeric) and
        // must keep stamping.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "temperature",
                Some(500.0),
                "Ti-6Al-4V was held at 500 \u{b0}C."
            )
            .is_some()
        );

        // Glued NON-unit letter: still dropped ("950x" is magnification,
        // not 950 + a unit).
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "The Ti-6Al-4V image at 950x magnification."
            )
            .is_none()
        );
        // Digit glue is still the substring reject: 95 inside 950MPa.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(95.0),
                "The Ti-6Al-4V UTS is 950MPa."
            )
            .is_none()
        );

        // A glued unit letter never redeems a digit inside a hyphen-joined
        // designation, ASCII or en dash (the "6" of "Ti-6Al-4V").
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(6.0),
                "The Ti-6Al-4V billets were 950MPa rated."
            )
            .is_none()
        );
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(6.0),
                "The Ti\u{2013}6Al\u{2013}4V billets were 950MPa rated."
            )
            .is_none()
        );

        // En-dash positive control: the subject's hyphens do not match en
        // dashes, so the fact survives ONLY through the object arm of the
        // subject-OR-object rule. Do not flip that OR to AND.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "The Ti\u{2013}6Al\u{2013}4V UTS is 950 MPa."
            )
            .is_some()
        );
    }

    /// Range endpoints are not point values: "950\u{2013}1100 MPa"
    /// asserts a range, and stamping UTS = 950 AND UTS = 1100 fabricates
    /// two facts the sentence never states. The dash rule is U+2013-only
    /// by decision (ASCII hyphens also join genuine compounds; em-dashes
    /// are sentence dashes), and it refuses the adjacent OCCURRENCE, not
    /// the number: an endpoint that recurs elsewhere as a genuine point
    /// value still stamps. Mutation-proven per side: swapping the
    /// after-side U+2013 for any other char reddens the 950 assert;
    /// swapping the before-side U+2013 reddens the 1100 assert.
    #[test]
    fn en_dash_range_endpoints_are_not_point_values() {
        let range = "The Ti-6Al-4V UTS ranged from 950\u{2013}1100 MPa.";
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 950.0, range);
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 1100.0, range);

        // Grouped endpoints are refused too.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            1140.0,
            "The Ti-6Al-4V UTS ranged from 1,140\u{2013}1,375 MPa.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            1375.0,
            "The Ti-6Al-4V UTS ranged from 1,140\u{2013}1,375 MPa.",
        );

        // A different point value in the same sentence still stamps.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1150.0),
                "The Ti-6Al-4V UTS ranged from 950\u{2013}1100 MPa and reached 1150 MPa \
                 after annealing."
            )
            .is_some()
        );

        // An endpoint that recurs outside the range as a genuine point
        // value still stamps: the rule refuses the range-adjacent
        // occurrence only.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "The Ti-6Al-4V UTS ranged from 950\u{2013}1100 MPa and the annealed \
                 sample reached 950 MPa."
            )
            .is_some()
        );

        // ASCII hyphen keeps its pre-round-4 behaviour (the dash may be
        // a genuine compound); pinned here and in the sign test.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1100.0),
                "The Ti-6Al-4V batches 950-1100 were tested."
            )
            .is_some()
        );
    }

    /// Fabrication path 3: a citation marker is not evidence. The exact
    /// sentence the extractor prompt uses as its example must never stamp
    /// the example's number.
    #[test]
    fn citation_marker_is_not_support() {
        let block = "Ti-6Al-4V has been studied extensively in prior work [1140].";
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 1140.0, block);
    }

    /// Fabrication path 3 (label form): a number immediately after
    /// Table/Figure/Ref is a label, not a measurement.
    #[test]
    fn table_figure_ref_label_numbers_are_not_support() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            3.0,
            "Ti-6Al-4V properties are listed in Table 3.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            2.0,
            "Ti-6Al-4V data appear in Figure 2.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            25.0,
            // No period after "Ref": with "Ref." the span split strands
            // the 25 in a span holding neither subject nor object, so the
            // label rule never fires. This form exercises it for real:
            // mutation-proven red when ref/refs are removed from
            // LABEL_WORDS.
            "UTS data for Ti-6Al-4V appears in Ref 25.",
        );
    }

    /// F-3: the label vocabulary also covers Section/Eq/Chapter/Sample/
    /// Run/Entry/Scheme labels. Mutation-proven: removing "section" from
    /// LABEL_WORDS turns the first assert red.
    #[test]
    fn section_and_kindred_label_numbers_are_not_support() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            4.0,
            "The Ti-6Al-4V results are in Section 4.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            7.0,
            "The Ti-6Al-4V model is given in Eq 7.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            2.0,
            "The Ti-6Al-4V route is shown in Scheme 2.",
        );
    }

    /// F-4: the same protection for SPACE-separated designations, where
    /// the boundary rule cannot help: the trailing digits of the claim's
    /// own subject/object are part of the name, not a measurement.
    /// Mutation-proven red when the inside-designation check is removed.
    /// An occurrence that is genuinely repeated OUTSIDE the name still
    /// stamps (last assert), so this is not a blanket digit-phobia.
    #[test]
    fn spaced_designation_digits_of_the_claims_own_name_are_not_support() {
        let table = "Alloy UTS (MPa)\nTi-6Al-4V 950\nInconel 718 1375";
        assert_dropped_end_to_end("Inconel 718", "UTS", 718.0, table);
        // The digits recur as a real measurement beside the name: stamp.
        assert!(
            supporting_quote(
                "Inconel 718",
                "UTS",
                Some(718.0),
                "Inconel 718 showed a UTS of 718 MPa."
            )
            .is_some()
        );
        // Positive control: the genuine row still stamps verbatim.
        assert_eq!(
            supporting_quote("Inconel 718", "UTS", Some(1375.0), table).as_deref(),
            Some("Inconel 718 1375")
        );
    }

    /// H6, prose side: parenthesized and braced citation numbers are
    /// markers, not measurements. The closing-side check is what keeps
    /// the legitimate parenthesized value "UTS (950 MPa)" stamping: a
    /// bracket-class rule that only looks backwards would refuse it, so
    /// '(' / '{' count as citation openers only when the marker closes
    /// before any non-number text.
    #[test]
    fn paren_and_brace_citation_numbers_are_not_support() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            1140.0,
            "Ti-6Al-4V has been widely studied (1140).",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            1140.0,
            "Ti-6Al-4V has been widely studied {1140}.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            1140.0,
            "Ti-6Al-4V has been widely studied (1140, 1141).",
        );
        // Positive control: a parenthesized value WITH its unit is not a
        // citation and must stamp.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "The Ti-6Al-4V UTS (950 MPa) was reproducible."
            )
            .is_some()
        );
    }

    /// H6 end to end through parse_jats: superscript bibr xrefs parse to
    /// a bare number, and `prefix.ends_with('[')` never sees them. The
    /// parser wraps bibr xref text in [...], so the bracketed-citation
    /// guard refuses it: the extractor prompt's own example fact
    /// (Ti-6Al-4V / UTS / 1140) must never stamp against this sentence.
    /// A real value in the same sentence still stamps (the required
    /// "real value after a citation" positive control).
    #[test]
    fn jats_bibr_xref_number_is_not_support() {
        let body = r#"<?xml version="1.0"?>
<article xmlns:xlink="http://www.w3.org/1999/xlink">
  <front>
    <article-meta>
      <title-group><article-title>Citations</article-title></title-group>
      <abstract><p>Abstract text.</p></abstract>
    </article-meta>
  </front>
  <body>
    <sec>
      <title>1. Section</title>
      <p>Ti-6Al-4V has been widely studied<sup><xref ref-type="bibr" rid="b1">1140</xref></sup>; its UTS is 950 MPa.</p>
    </sec>
  </body>
</article>"#;
        let ft = crate::fulltext::parse_jats(body.as_bytes()).unwrap();
        for block in &ft.blocks {
            assert!(
                supporting_quote("Ti-6Al-4V", "UTS", Some(1140.0), &block.text).is_none(),
                "block {:?} fabricated support from a superscript citation: {:?}",
                block.locator.kind,
                block.text
            );
        }
        let body_block = ft
            .blocks
            .iter()
            .find(|b| b.locator.kind == BlockKind::Body)
            .unwrap();
        assert!(
            supporting_quote("Ti-6Al-4V", "UTS", Some(950.0), &body_block.text).is_some(),
            "the real value after the citation must still stamp: {:?}",
            body_block.text
        );
    }

    /// The list form of label references: in "Tables 1 and 2" the
    /// number after the conjunction is a label too, but only the
    /// immediately preceding word was checked, so 2 stamped. Walk back
    /// over the conjunction and the number before it, exactly once.
    /// (Mid-list comma items like "Refs. 25, 26" stay an open gap.)
    #[test]
    fn label_list_numbers_after_a_conjunction_are_not_support() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            2.0,
            "Ti-6Al-4V data are listed in Tables 1 and 2.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            26.0,
            "Ti-6Al-4V is discussed in Refs. 25 and 26.",
        );
        // Positive control: a real measurement after "and <number>"
        // still stamps when the word one number back is not a label.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(960.0),
                "The Ti-6Al-4V samples measured 950 and 960 MPa."
            )
            .is_some()
        );
    }

    /// Comma-separated reference lists: "Refs. 25, 26" stamped 26 in
    /// round 3 because the walk-back covered one conjunction but not
    /// comma items. The walk now steps back over ", <number>" repeatedly
    /// before the conjunction step, and the label word at the head of
    /// the list decides. Positive controls pin the distinguishing
    /// feature: values in a comma list walk back to a non-label head
    /// word and stamp — they are values, not references. Mutation-proven
    /// red by deleting the comma walk-back loop (the two Refs asserts)
    /// and by a one-token dash on its `ends_with(',')` condition.
    #[test]
    fn comma_separated_reference_list_numbers_are_not_support() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            26.0,
            "Ti-6Al-4V is discussed in Refs. 25, 26 for UTS data.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            27.0,
            "Ti-6Al-4V is discussed in Refs. 25, 26, 27 for UTS data.",
        );
        // Comma list ending in a conjunction: the walk-backs compose.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            27.0,
            "Ti-6Al-4V is discussed in Refs. 25, 26 and 27 for UTS data.",
        );
        // The first number after the label word stays refused too.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            25.0,
            "Ti-6Al-4V is discussed in Refs. 25, 26 for UTS data.",
        );

        // Positive controls: value lists are NOT reference lists; the
        // head word, not the commas, is the distinguishing feature.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(970.0),
                "The Ti-6Al-4V samples measured 950, 960 and 970 MPa."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(970.0),
                "The Ti-6Al-4V samples measured 950, 960, 970 MPa."
            )
            .is_some()
        );
        // A mid-list value also stamps.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(960.0),
                "The Ti-6Al-4V samples measured 950, 960, 970 MPa."
            )
            .is_some()
        );
    }

    /// H7: an abbreviated label ("Fig. 2", "Ref. 25") used to defeat the
    /// label rule: the abbreviating period ended the span, stranding the
    /// number in a fresh span where the label word was invisible, so
    /// `UTS = 2` stamped. The period of such an abbreviation must not end
    /// a span. ("Table 3" / "Figure 2" without the period were already
    /// blocked; the round-2 `Ref 25` fixture only worked because the
    /// period was removed from it.)
    #[test]
    fn abbreviated_label_numbers_are_not_support() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            2.0,
            "As shown in Fig. 2 Ti-6Al-4V was tested to failure.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            25.0,
            "As reported in Ref. 25 Ti-6Al-4V is widely used.",
        );
        // "Eqs." joins the label family: the first number after it is a
        // label, and the conjunction step takes the "and 8" tail with it
        // (the walk is bounded to one number-run per conjunction, but a
        // reference list carries no units, so it still walks).
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            7.0,
            "The fits are given in Eqs. 7 and 8 for Ti-6Al-4V UTS.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            8.0,
            "The fits are given in Eqs. 7 and 8 for Ti-6Al-4V UTS.",
        );
        // Positive control: a real value in the same sentence as an
        // abbreviated label still stamps.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "As shown in Fig. 2 the Ti-6Al-4V UTS is 950 MPa."
            )
            .is_some()
        );
    }

    /// Fabrication path 4: digits inside alloy designations are not
    /// numeric prose. Alloy names are numeric by convention, so a block
    /// with no measurement must not stamp the digits of the name.
    #[test]
    fn alloy_designation_digits_are_not_support() {
        let block = "The Ti-6Al-4V samples were annealed and examined.";
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 6.0, block);
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 4.0, block);
    }

    /// Fabrication path 1: a table must not act as one giant span. Rows
    /// are separate spans, so a number in one row cannot support a claim
    /// whose subject lives in another row (the 1375 assert, mutation-proven
    /// by fusing all rows into one span). The caption line also exercises
    /// the label rule: the "1" of "Table 1" sits in a span that holds the
    /// subject AND the object, so only the label rule keeps it from
    /// stamping (the 1.0 assert, mutation-proven by disabling
    /// `preceding_word_is_label`).
    #[test]
    fn properties_table_rows_are_separate_spans() {
        let table = "Table 1 UTS of Ti-6Al-4V and Inconel 718\n\
                     Alloy UTS (MPa)\n\
                     Ti-6Al-4V 950\n\
                     Inconel 718 1375";

        // Inconel's number cannot support a claim about Ti-6Al-4V: the
        // subject and 1375 never share a row.
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 1375.0, table);
        // The "1" of "Table 1" is a label, not a UTS value, even though
        // the caption span holds both the subject and the object.
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 1.0, table);

        // Genuine rows still stamp: the alloy and its own number share a row.
        assert_eq!(
            supporting_quote("Ti-6Al-4V", "UTS", Some(950.0), table).as_deref(),
            Some("Ti-6Al-4V 950")
        );
        assert_eq!(
            supporting_quote("Inconel 718", "UTS", Some(1375.0), table).as_deref(),
            Some("Inconel 718 1375")
        );
    }

    /// Fabrication path 1, end to end through the real JATS sink: from one
    /// properties table, neither `Ti-6Al-4V UTS = 1375` (Inconel's number)
    /// nor `Ti-6Al-4V UTS = 1` (the "1" of "Table 1", or the digit inside
    /// "718") may find support in ANY block of the document.
    #[test]
    fn jats_properties_table_supports_no_cross_row_claim() {
        let body = r#"<?xml version="1.0"?>
<article xmlns:xlink="http://www.w3.org/1999/xlink">
  <front>
    <article-meta>
      <title-group><article-title>Properties</article-title></title-group>
      <abstract><p>Abstract text.</p></abstract>
    </article-meta>
  </front>
  <body>
    <sec>
      <title>1. Section</title>
      <p>Mechanical properties of Ti-6Al-4V and Inconel 718 are shown in Table 1.</p>
      <table-wrap>
        <label>Table 1</label>
        <caption><p>Mechanical properties.</p></caption>
        <table>
          <tr><th>Alloy</th><th>UTS (MPa)</th></tr>
          <tr><td>Ti-6Al-4V</td><td>950</td></tr>
          <tr><td>Inconel 718</td><td>1375</td></tr>
        </table>
      </table-wrap>
    </sec>
  </body>
</article>"#;
        let ft = crate::fulltext::parse_jats(body.as_bytes()).unwrap();
        for block in &ft.blocks {
            assert!(
                supporting_quote("Ti-6Al-4V", "UTS", Some(1375.0), &block.text).is_none(),
                "block {:?} fabricated support for Inconel's number: {:?}",
                block.locator.kind,
                block.text
            );
            assert!(
                supporting_quote("Ti-6Al-4V", "UTS", Some(1.0), &block.text).is_none(),
                "block {:?} fabricated support from a label digit: {:?}",
                block.locator.kind,
                block.text
            );
        }
        // The genuine row still stamps in the table block.
        let table = ft
            .blocks
            .iter()
            .find(|b| b.locator.kind == BlockKind::Table)
            .unwrap();
        assert_eq!(
            supporting_quote("Ti-6Al-4V", "UTS", Some(950.0), &table.text).as_deref(),
            Some("Ti-6Al-4V 950")
        );
    }

    /// Fabrication path 1, OASIS/CALS table model (H1): JATS permits two
    /// table models; a paper using <tgroup>/<row>/<entry> is no less
    /// protected than one using <tr>/<td>. From ONE OASIS table,
    /// `Ti-6Al-4V UTS = 1375` (Inconel's number) must find no support in
    /// any block; both genuine rows still stamp. Mutation-proven red when
    /// `row` is removed from the row-boundary match in parse_jats.
    #[test]
    fn jats_oasis_table_supports_no_cross_row_claim() {
        let body = r#"<?xml version="1.0"?>
<article xmlns:xlink="http://www.w3.org/1999/xlink">
  <front>
    <article-meta>
      <title-group><article-title>Properties</article-title></title-group>
      <abstract><p>Abstract text.</p></abstract>
    </article-meta>
  </front>
  <body>
    <sec>
      <title>1. Section</title>
      <p>Mechanical properties are shown in Table 1.</p>
      <table-wrap>
        <label>Table 1</label>
        <table>
          <tgroup cols="2">
            <tbody>
              <row><entry>Ti-6Al-4V</entry><entry>950</entry></row>
              <row><entry>Inconel 718</entry><entry>1375</entry></row>
            </tbody>
          </tgroup>
        </table>
      </table-wrap>
    </sec>
  </body>
</article>"#;
        let ft = crate::fulltext::parse_jats(body.as_bytes()).unwrap();
        for block in &ft.blocks {
            assert!(
                supporting_quote("Ti-6Al-4V", "UTS", Some(1375.0), &block.text).is_none(),
                "block {:?} fabricated support for Inconel's number: {:?}",
                block.locator.kind,
                block.text
            );
        }
        let table = ft
            .blocks
            .iter()
            .find(|b| b.locator.kind == BlockKind::Table)
            .unwrap();
        assert_eq!(
            supporting_quote("Ti-6Al-4V", "UTS", Some(950.0), &table.text).as_deref(),
            Some("Ti-6Al-4V 950")
        );
        assert_eq!(
            supporting_quote("Inconel 718", "UTS", Some(1375.0), &table.text).as_deref(),
            Some("Inconel 718 1375")
        );
    }

    /// Round 6: the dash-redemption clause in `clean_number_boundary`
    /// refused EVERY digit with a dash before it and a unit letter after
    /// it. That killed the high endpoint of digit-dash-digit ranges with
    /// glued units — "30-50um" stamped 30 but dropped 50, "5-10mm"
    /// dropped 10, same token, low endpoint lives, high one dies.
    /// Letter-hyphenated designation digits must stay refused, and the
    /// en-dash object-arm assert is the deletion killer: with the clause
    /// gone, the "6" of Ti\u{2013}6Al\u{2013}4V stamps via the object
    /// arm (the subject's ASCII hyphens mismatch the en-dash text, and
    /// occurrence_inside_name sees nothing), so the clause survives in
    /// narrowed form instead of being deleted. Mutations: deleting the
    /// clause reddens the en-dash assert; swapping the digit condition
    /// (`!c.is_ascii_digit()` -> `c.is_ascii_digit()`) reddens the harm
    /// asserts.
    #[test]
    fn digit_dash_ranges_with_glued_units_stamp_the_high_endpoint() {
        // The measured harms: the high endpoint is a measurement.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "layer_thickness",
                Some(50.0),
                "Ti-6Al-4V powder layers of 30-50um were deposited."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "grain_size",
                Some(10.0),
                "CoCrFeNi grains of 5-10mm were observed."
            )
            .is_some()
        );
        // Letter-dash designation digits stay refused. In the en-dash
        // form ONLY this clause refuses the "6" — deletion turns it red.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            6.0,
            "The Ti\u{2013}6Al\u{2013}4V UTS is 950 MPa.",
        );
        // ASCII form: double-covered by occurrence_inside_name; kept as
        // documentation of the class.
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 6.0, "The Ti-6Al-4V UTS is 950 MPa.");
    }

    /// Round 6: the walk-back used to compose comma steps with an
    /// unbounded conjunction trim, so a VALUE after a sentence's locator
    /// label ("In Table 5,") walked back over the label number and
    /// dropped the whole value list — measured drops, all stamped at
    /// base. The chain continuing after the occurrence decides: it ends
    /// in a unit -> value list, no walk; no unit -> reference list,
    /// walk. Mutation-proven red by removing the gate, by removing the
    /// comma step or the conjunction step of the forward chain scan, or
    /// by removing the head-of-chain unit check. The reference-list
    /// asserts above (Refs. 25, 26 / and 27 / Eqs. 7 and 8) kill
    /// mutations that over-broaden `UNIT_TOKENS` ("for" as a unit would
    /// stamp them).
    #[test]
    fn value_lists_after_a_label_locator_still_stamp() {
        // Comma + conjunction list after "Table 5,": all three values.
        let list = "In Table 5, 950, 960 and 970 MPa were measured for Ti-6Al-4V.";
        for v in [950.0, 960.0, 970.0] {
            assert!(
                supporting_quote("Ti-6Al-4V", "UTS", Some(v), list).is_some(),
                "value {v} dropped from: {list}"
            );
        }
        // Longer list: the tail value that carries the unit AND the four
        // mid values the unbounded trim used to eat.
        let long = "Per Table 2, 950, 960, 970, 980 and 990 MPa were recorded for Ti-6Al-4V.";
        for v in [950.0, 960.0, 970.0, 980.0, 990.0] {
            assert!(
                supporting_quote("Ti-6Al-4V", "UTS", Some(v), long).is_some(),
                "value {v} dropped from: {long}"
            );
        }
        // "Fig. N," clause opener — guard interaction: the H7 period fix
        // keeps the value in one span with "fig", and the comma walk then
        // traversed it. Neither guard alone did this.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "As shown in Fig. 5, 950 MPa was the peak Ti-6Al-4V UTS."
            )
            .is_some()
        );
        // Comma-grouped value right after the label comma.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1140.0),
                "Per Table 3, 1,140 MPa was the Ti-6Al-4V peak."
            )
            .is_some()
        );
        // Signed value after the label comma.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "surface_stress",
                Some(-950.0),
                "From Fig. 6, \u{2212}950 MPa was the Ti-6Al-4V surface stress."
            )
            .is_some()
        );

        // Positive controls that must keep stamping exactly as before.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "Figure 3 shows a UTS of 950 MPa."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "alloy",
                "strength",
                Some(1100.0),
                "in Table 4 the alloy reached 1100 MPa"
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "In Fig. 4, the Ti-6Al-4V UTS of 950 MPa is marked."
            )
            .is_some()
        );
    }

    // ------------------------------------------------------------------
    // Over-refusal instrumentation: a drop caused by a guard must name
    // the guard, not read as the model's hallucination. Each assert
    // below is a mutation target: removing the matching guard check in
    // `refusing_guard` turns its occurrence evidential (Ok instead of
    // Guarded), and removing the refusal recording in
    // `supporting_quote_or_refusal` collapses every Guarded into
    // NoSpan.
    // ------------------------------------------------------------------

    #[test]
    fn guarded_refusals_name_the_guard_that_dropped_the_value() {
        // Label: span holds subject + object + value; the label walk
        // refuses the only occurrence.
        let label = "Ti-6Al-4V properties are listed in Table 3.";
        assert_eq!(
            supporting_quote_or_refusal("Ti-6Al-4V", "properties", Some(3.0), label),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Label,
                span: label.to_string(),
            })
        );

        // Boundary: "95" inside "950" — digit continuation.
        let boundary = "The Ti-6Al-4V UTS is 950 MPa.";
        assert_eq!(
            supporting_quote_or_refusal("Ti-6Al-4V", "UTS", Some(95.0), boundary),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Boundary,
                span: boundary.to_string(),
            })
        );

        // Citation: the number sits in a bracketed marker.
        let citation = "Ti-6Al-4V has been studied extensively in prior work [1140].";
        assert_eq!(
            supporting_quote_or_refusal("Ti-6Al-4V", "UTS", Some(1140.0), citation),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Citation,
                span: citation.to_string(),
            })
        );

        // InsideName: the "718" of the claim's own subject name.
        let table = "Alloy UTS (MPa)\nTi-6Al-4V 950\nInconel 718 1375";
        assert_eq!(
            supporting_quote_or_refusal("Inconel 718", "UTS", Some(718.0), table),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::InsideName,
                span: "Inconel 718 1375".to_string(),
            })
        );

        // Range: en-dash digit range endpoint.
        let range = "The Ti-6Al-4V UTS ranged from 950\u{2013}1100 MPa.";
        assert_eq!(
            supporting_quote_or_refusal("Ti-6Al-4V", "UTS", Some(950.0), range),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Range,
                span: range.to_string(),
            })
        );
    }

    /// A block that never mentions the fact's salient tokens is NoSpan,
    /// which maps to MissingQuote — that drop IS the model's fault and
    /// must stay indistinguishable from a hallucinated quote.
    #[test]
    fn no_span_refusal_stays_missing_quote() {
        let block = "Discussion of prior work [1140] follows. No alloy data here.";
        assert_eq!(
            supporting_quote_or_refusal("Ti-6Al-4V", "UTS", Some(1140.0), block),
            Err(SupportRefusal::NoSpan)
        );
        assert_eq!(
            ClaimRejection::from(SupportRefusal::NoSpan),
            ClaimRejection::MissingQuote
        );
        assert_eq!(
            ClaimRejection::from(SupportRefusal::Guarded {
                guard: RefusalGuard::Label,
                span: "s".to_string(),
            }),
            ClaimRejection::NoEvidentialOccurrence {
                guard: RefusalGuard::Label,
                span: "s".to_string(),
            }
        );
    }
}
