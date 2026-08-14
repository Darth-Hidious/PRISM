//! Controlled-vocabulary normalisation of unit spellings to QUDT identifiers.
//!
//! Extraction models — especially the small local ones the default setup
//! runs — write units the way papers print them (`MPa`, `g/cm³`, `W/(m·K)`),
//! not as QUDT identifiers, no matter how firmly the prompt asks. Rejecting
//! those spellings rejects real measurements whose units are unambiguous.
//! This module maps the common materials-science spellings onto the QUDT
//! identifiers PRISM stores ([`QudtUnit`]).
//!
//! It is deliberately a hand-written, auditable TABLE — a controlled
//! vocabulary, not a unit parser: an unknown spelling resolves to nothing
//! rather than to a guess, and every row can be reviewed line by line.
//! The identifiers are QUDT unit-vocabulary local names
//! (`http://qudt.org/vocab/unit/<NAME>`), each cross-checked against the
//! unit ontology vendored with EMMO 1.0.3 (`disciplines/units/*.ttl`).
//! Deriving the table from that ontology instead of maintaining it by hand
//! is a reasonable follow-up; the hand-written form keeps this crate free
//! of an ontology-parsing dependency and the mapping reviewable.

use crate::QudtUnit;

/// Folded spelling → QUDT identifier. Keys are in [`fold`]ed form (lowercase,
/// separators and spaces removed, `³`→`3`, `°`→`deg`, …). Every row is a
/// spelling that is unambiguous in a materials-science context.
///
/// Case folding knowingly maps `mPa` (millipascal) onto megapascal: the
/// sources here are materials papers, where a strength or modulus in
/// millipascals does not occur and `mpa` is always a case-sloppy megapascal.
///
/// `wt%` / `at%` / `vol%` are deliberately ABSENT: mapping them to
/// `QUDT:PERCENT` would erase the weight/atomic/volume basis — real
/// information the percent sign alone does not carry — so those facts are
/// dropped loudly instead of stored blurred.
const UNIT_SPELLINGS: &[(&str, &str)] = &[
    // Dimensionless quantities.
    //
    // A dimensionless quantity is NOT a missing unit — it is a specific one,
    // and QUDT names it. Without this row the ingest rule "a value with no
    // unit is a wrong number" (correct: 880 GPa must not be confused with 880
    // MPa) silently discarded every quantity that is dimensionless BY
    // DEFINITION. Measured on a polymer tribology paper: 33 of 74 extracted
    // facts were coefficients of friction, and every one was dropped as
    // malformed. Coefficient of friction is the customer's second
    // requirement, so PRISM could not store the property it was being asked
    // about. Poisson's ratio, relative permittivity, Weibull modulus and
    // refractive index all share the shape.
    ("unitless", "QUDT:UNITLESS"),
    ("dimensionless", "QUDT:UNITLESS"),
    ("none", "QUDT:UNITLESS"),
    ("n/a", "QUDT:UNITLESS"),
    ("-", "QUDT:UNITLESS"),
    ("1", "QUDT:UNITLESS"),
    // Rate / time
    ("/s", "QUDT:PER-SEC"),
    ("1/s", "QUDT:PER-SEC"),
    ("s-1", "QUDT:PER-SEC"),
    ("persec", "QUDT:PER-SEC"),
    ("s", "QUDT:SEC"),
    ("sec", "QUDT:SEC"),
    ("second", "QUDT:SEC"),
    ("min", "QUDT:MIN"),
    ("h", "QUDT:HR"),
    ("hr", "QUDT:HR"),
    ("hour", "QUDT:HR"),
    // Speed — LPBF scan speeds live here
    ("mm/s", "QUDT:MilliM-PER-SEC"),
    ("m/s", "QUDT:M-PER-SEC"),
    // DESCRIPTIVE spellings of the same two units. Measured on a real
    // 36-page LPBF paper through the live pipeline (Gemma 4 12B): of 58
    // extracted facts only 4 were stored, and 27 of the 54 drops were this
    // one unit — the model wrote `QUDT:Meter-Per-Second`, the human-readable
    // form of an identifier this table ALREADY vouches for. Half the loss on
    // that paper was spelling, not data.
    //
    // These add SPELLINGS for identifiers already in the table; no new QUDT
    // identifier is invented here, so the rule that an unknown `QUDT:` local
    // name is refused is untouched.
    ("meterpersecond", "QUDT:M-PER-SEC"),
    ("meter-per-second", "QUDT:M-PER-SEC"),
    ("metrepersecond", "QUDT:M-PER-SEC"),
    ("metre-per-second", "QUDT:M-PER-SEC"),
    ("millimeterpersecond", "QUDT:MilliM-PER-SEC"),
    ("millimeter-per-second", "QUDT:MilliM-PER-SEC"),
    ("millimetrepersecond", "QUDT:MilliM-PER-SEC"),
    ("millimetre-per-second", "QUDT:MilliM-PER-SEC"),
    // Counts, count densities and event rates.
    //
    // These are spellings papers PRINT, not spellings a model invented — the
    // distinction that separates this from chasing a generator. The resolver
    // reads the DOCUMENT, so the set of forms it must know is bounded by how
    // journals typeset quantities, which is finite. Adding rows for a model's
    // invented identifiers (`QUDT:MM-PER-S`) is the losing game; three
    // consecutive runs of the same paper invented three DIFFERENT sets.
    //
    // Identifiers verified live against `http://qudt.org/vocab/unit/<name>`.
    // `CYC-PER-MIN` is NOT a QUDT unit (404): a cycle is dimensionless, so a
    // cycle rate is PER-MIN and a cycle count is NUM.
    //
    // Bare `n`/`no` are deliberately ABSENT as count spellings: `n` is
    // already newton above, and a sample count `N` next to a number is
    // exactly the homograph the span gate exists to refuse.
    ("cycle", "QUDT:NUM"),
    ("cycles", "QUDT:NUM"),
    ("count", "QUDT:NUM"),
    ("counts", "QUDT:NUM"),
    // Number density — pores per cubic millimetre. `mm⁻³` and `mm^-3` both
    // fold to `mm-3`; `1/mm3` and `/mm3` are the slashed forms.
    ("/mm3", "QUDT:NUM-PER-MilliM3"),
    ("1/mm3", "QUDT:NUM-PER-MilliM3"),
    ("mm-3", "QUDT:NUM-PER-MilliM3"),
    ("permm3", "QUDT:NUM-PER-MilliM3"),
    ("pores/mm3", "QUDT:NUM-PER-MilliM3"),
    // Event rate. `min` alone stays the time unit above; only the explicitly
    // reciprocal forms are a frequency.
    ("/min", "QUDT:PER-MIN"),
    ("1/min", "QUDT:PER-MIN"),
    ("min-1", "QUDT:PER-MIN"),
    ("permin", "QUDT:PER-MIN"),
    ("cycles/min", "QUDT:PER-MIN"),
    ("cpm", "QUDT:PER-MIN"),
    // Force / power / electrical
    ("n", "QUDT:N"),
    ("newton", "QUDT:N"),
    ("kn", "QUDT:KiloN"),
    ("w", "QUDT:W"),
    ("watt", "QUDT:W"),
    ("kw", "QUDT:KiloW"),
    ("v", "QUDT:V"),
    ("volt", "QUDT:V"),
    ("kv", "QUDT:KiloV"),
    ("a", "QUDT:A"),
    ("ka", "QUDT:KiloA"),
    ("ampere", "QUDT:A"),
    // Mass / amount
    ("g", "QUDT:GM"),
    ("gram", "QUDT:GM"),
    ("kg", "QUDT:KiloGM"),
    ("mg", "QUDT:MilliGM"),
    ("mol", "QUDT:MOL"),
    // Length
    ("m", "QUDT:M"),
    ("metre", "QUDT:M"),
    ("meter", "QUDT:M"),
    ("mm", "QUDT:MilliM"),
    ("millimetre", "QUDT:MilliM"),
    ("millimeter", "QUDT:MilliM"),
    ("um", "QUDT:MicroM"),
    ("micron", "QUDT:MicroM"),
    ("micrometre", "QUDT:MicroM"),
    ("micrometer", "QUDT:MicroM"),
    ("nm", "QUDT:NanoM"),
    ("nanometre", "QUDT:NanoM"),
    ("nanometer", "QUDT:NanoM"),
    // Energy per mass, molar mass, frequency, charge density
    ("j/g", "QUDT:J-PER-GM"),
    ("jperg", "QUDT:J-PER-GM"),
    ("g/mol", "QUDT:GM-PER-MOL"),
    ("hz", "QUDT:HZ"),
    ("hertz", "QUDT:HZ"),
    ("c/m2", "QUDT:C-PER-M2"),
    // Electric field strength — the HV-insulation workhorses
    ("kv/mm", "QUDT:KiloV-PER-MilliM"),
    ("mv/m", "QUDT:MegaV-PER-M"),
    ("v/m", "QUDT:V-PER-M"),
    // Transport
    ("w/mk", "QUDT:W-PER-M-K"),
    ("s/m", "QUDT:S-PER-M"),
    ("ohmm", "QUDT:OHM-M"),
    // Pressure / stress / elastic moduli
    ("pa", "QUDT:PA"),
    ("pascal", "QUDT:PA"),
    ("kpa", "QUDT:KiloPA"),
    ("kilopascal", "QUDT:KiloPA"),
    ("mpa", "QUDT:MegaPA"),
    ("megapascal", "QUDT:MegaPA"),
    // 1 N/mm² = 1 MPa exactly; the DIN/EN spelling for strength values.
    ("n/mm2", "QUDT:MegaPA"),
    ("gpa", "QUDT:GigaPA"),
    ("gigapascal", "QUDT:GigaPA"),
    // Temperature
    ("k", "QUDT:K"),
    ("kelvin", "QUDT:K"),
    ("degc", "QUDT:DEG_C"),
    ("celsius", "QUDT:DEG_C"),
    ("degreecelsius", "QUDT:DEG_C"),
    ("degreescelsius", "QUDT:DEG_C"),
    // Density
    ("g/cm3", "QUDT:GM-PER-CentiM3"),
    ("g/cc", "QUDT:GM-PER-CentiM3"),
    ("gcm-3", "QUDT:GM-PER-CentiM3"),
    ("kg/m3", "QUDT:KiloGM-PER-M3"),
    ("kgm-3", "QUDT:KiloGM-PER-M3"),
    // Thermal conductivity
    ("w/mk", "QUDT:W-PER-M-K"),
    ("w/m/k", "QUDT:W-PER-M-K"),
    ("wm-1k-1", "QUDT:W-PER-M-K"),
    // Dimensionless fractions
    ("%", "QUDT:PERCENT"),
    ("percent", "QUDT:PERCENT"),
    ("pct", "QUDT:PERCENT"),
    // Energy
    ("ev", "QUDT:EV"),
    ("electronvolt", "QUDT:EV"),
    ("electron-volt", "QUDT:EV"),
    ("kev", "QUDT:KiloEV"),
];

/// Resolve a unit spelling to a validated [`QudtUnit`].
///
/// Accepts what [`QudtUnit::new`] accepts (a `QUDT:`-prefixed identifier)
/// plus the controlled vocabulary above — the newtype's validation contract
/// is not weakened, normalisation happens BEFORE construction. Returns
/// `None` for everything else, never a guess: the caller decides what an
/// unresolvable unit costs. For extraction that cost is the whole fact,
/// because a number stored without its unit is a wrong number (unit-less
/// floats once made 880 GPa indistinguishable from 880 MPa here).
///
/// A `QUDT:`-prefixed local name is looked up in the table FIRST, so a
/// QUDT-prefixed SPELLING is canonicalised rather than stored as a
/// pseudo-identifier — observed live: `qwen2.5:3b` writes `QUDT:MPa`,
/// which satisfies the prefix check while naming no QUDT unit (the
/// megapascal's local name is `MegaPA`). Every canonical identifier in the
/// table maps to itself under this rule (`K`→`QUDT:K`, `PA`→`QUDT:PA`, …),
/// so no valid identifier is ever rewritten to a different one; unknown
/// local names still pass through untouched, keeping QUDT's identifier
/// space open.
#[must_use]
pub fn resolve_unit(raw: &str) -> Option<QudtUnit> {
    let raw = raw.trim();
    let prefix_len = "QUDT:".len();
    if raw
        .get(..prefix_len)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("QUDT:"))
    {
        let local = &raw[prefix_len..];
        if let Some(unit) = lookup(local) {
            return Some(unit);
        }
        // A `QUDT:` PREFIX IS NOT A QUDT IDENTIFIER. Passing an unknown local
        // name through meant the gate only ever caught bare spellings, and the
        // extraction prompt instructs the model to always write the prefix —
        // so compliance BYPASSED the check. Measured in a live store:
        // `QUDT:UM` (the real name is `MicroM`, and it arrived because
        // `Ra = 0.025 µm` lost its µ), `QUDT:HRC` (Rockwell C is not a QUDT
        // unit at all), and `QUDT:nm` alongside `QUDT:Nanometer` — one unit
        // under two identities, in the store whose whole purpose is one
        // identity per unit. Meanwhile the user was told any unresolvable
        // unit had been dropped.
        return KNOWN_IDENTIFIERS
            .iter()
            .find(|identifier| identifier.eq_ignore_ascii_case(raw))
            .map(|identifier| {
                QudtUnit::new(*identifier).expect("KNOWN_IDENTIFIERS holds valid identifiers")
            });
    }
    lookup(raw)
}

/// Return whether `span` contains a controlled unit spelling that resolves to
/// exactly `expected`.
///
/// Matching uses [`resolve_unit`] rather than a second spelling table, so the
/// span gate accepts the same printed forms as conversion (`GPa`,
/// `W/(m·K)`, `g/cm³`, `%`, and known canonical `QUDT:` identifiers) and
/// cannot drift from it. Candidates must occupy a complete lexical unit: a
/// single-letter unit cannot be found inside a word, and a component of a
/// compound unit cannot masquerade as the whole unit. Every candidate must
/// also occupy quantity position after a number, so homographs such as the
/// article `a`, ordinal `second`, and sample-count `N` are not unit evidence.
///
/// Bare `1` and `-` are intentionally not unit evidence here even though
/// [`resolve_unit`] accepts them as model spellings for `QUDT:UNITLESS`. In a
/// source span those glyphs are indistinguishable from a numeric value or a
/// sign. Explicit `unitless` and `dimensionless` spellings are found when
/// printed in quantity position.
#[must_use]
pub fn span_contains_resolved_unit(span: &str, expected: &QudtUnit) -> bool {
    span_contains_unit_matching(span, |resolved, _, _| resolved == expected)
}

/// Return whether `span` contains any lexically bounded controlled unit.
///
/// This shares all spelling, boundary, and ambiguous-glyph rules with
/// [`span_contains_resolved_unit`]. In particular, numeric `1`, a minus sign,
/// articles, ordinals, and author/sample labels do not become unit evidence
/// merely because the model-side resolver accepts a homographic spelling.
#[must_use]
pub fn span_contains_any_resolved_unit(span: &str) -> bool {
    span_contains_unit_matching(span, |_, _, _| true)
}

/// Return whether `expected` occurs in quantity position immediately after
/// the numeric lexeme ending at byte offset `value_end`.
///
/// Only whitespace may separate the value and unit; parenthesized units still
/// work because the unit candidate itself begins at `(`. This binds a unit to
/// one value rather than borrowing another quantity's unit from elsewhere in
/// the same sentence.
#[must_use]
pub fn span_value_has_resolved_unit(span: &str, value_end: usize, expected: &QudtUnit) -> bool {
    if value_end > span.len() || !span.is_char_boundary(value_end) {
        return false;
    }
    span_contains_unit_matching(span, |resolved, start, _| {
        resolved == expected
            && start >= value_end
            && span[value_end..start].chars().all(char::is_whitespace)
    })
}

/// Return whether any controlled unit occurs immediately after the numeric
/// lexeme ending at `value_end`.
///
/// This is the absence check for an implicit `QUDT:UNITLESS` value: units on
/// other quantities later in the sentence do not contaminate it.
#[must_use]
pub fn span_value_has_any_resolved_unit(span: &str, value_end: usize) -> bool {
    if value_end > span.len() || !span.is_char_boundary(value_end) {
        return false;
    }
    span_contains_unit_matching(span, |_, start, _| {
        start >= value_end && span[value_end..start].chars().all(char::is_whitespace)
    })
}

/// The controlled unit occupying quantity position immediately after the
/// numeric lexeme ending at byte offset `value_end`, or `None` when nothing
/// adjacent resolves.
///
/// Same adjacency contract as [`span_value_has_resolved_unit`] — only
/// whitespace may separate the value and the unit — but this returns WHICH
/// unit sits there instead of confirming a caller's expectation. It exists
/// for the repair tier's unit re-resolution: the corrected identifier must
/// come from the document's own printed spelling, never from the model's
/// claim, so the two paths (checking a claimed unit, reading the printed
/// one) share one scanner and cannot drift.
#[must_use]
pub fn span_value_resolved_adjacent_unit(span: &str, value_end: usize) -> Option<QudtUnit> {
    if value_end > span.len() || !span.is_char_boundary(value_end) {
        return None;
    }
    let mut found = None;
    span_contains_unit_matching(span, |resolved, start, _| {
        let adjacent =
            start >= value_end && span[value_end..start].chars().all(char::is_whitespace);
        if adjacent {
            found = Some(resolved.clone());
        }
        adjacent
    });
    found
}

/// Longest folded candidate the scanner can resolve. This is derived from
/// the spelling table (including `QUDT:`-prefixed spellings and canonical
/// identifiers), so adding a longer controlled spelling expands the scanner
/// automatically.
static MAX_FOLDED_UNIT_CANDIDATE_LEN: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
    UNIT_SPELLINGS
        .iter()
        .flat_map(|(spelling, identifier)| {
            [
                spelling.len(),
                "qudt:".len() + spelling.len(),
                fold(identifier).len(),
            ]
        })
        .max()
        .expect("UNIT_SPELLINGS is non-empty")
});

fn span_contains_unit_matching(
    span: &str,
    mut matches: impl FnMut(&QudtUnit, usize, usize) -> bool,
) -> bool {
    for (start, first) in span.char_indices() {
        if first.is_whitespace() || !has_unit_start_boundary(span, start, first) {
            continue;
        }

        for (relative_end, last) in span[start..].char_indices() {
            let end = start + relative_end + last.len_utf8();
            let candidate = &span[start..end];
            let folded = fold(candidate);
            if folded.len() > *MAX_FOLDED_UNIT_CANDIDATE_LEN {
                break;
            }
            if last.is_whitespace() || !has_unit_end_boundary(span, end) {
                continue;
            }

            if let Some(resolved) = resolve_unit_candidate(candidate) {
                if !unit_candidate_has_quantity_context(span, start, end) {
                    continue;
                }
                if matches(&resolved, start, end) {
                    return true;
                }
            }
        }
    }
    false
}

/// A controlled spelling is unit evidence only in quantity position: after a
/// printed number (including `4 (A)`). This uniform rule covers symbolic and
/// word homographs without another vocabulary (`A` as an article, `N` as an
/// author initial, `second` as an ordinal, `none` as prose). It deliberately
/// refuses bare table-header units: without the matched value's position,
/// `(N)` and the plural suffix in `result(s)` are indistinguishable.
fn unit_candidate_has_quantity_context(span: &str, start: usize, end: usize) -> bool {
    if span[end..].starts_with('.')
        && span[end + 1..]
            .chars()
            .find(|c| !c.is_whitespace())
            .is_some_and(char::is_alphabetic)
    {
        // An author initial (`Ref. 5 N. Smith`) is not a newton. Sentence
        // splitting means a genuine sentence-final `5 N.` has no following
        // alphabetic text in the span.
        return false;
    }

    span[..start]
        .chars()
        .rev()
        .find(|c| !c.is_whitespace())
        .is_some_and(|c| c.is_ascii_digit())
}

fn resolve_unit_candidate(candidate: &str) -> Option<QudtUnit> {
    let folded = fold(candidate);

    // These table entries are useful at the model-conversion boundary, but
    // the same glyphs inside source text are ordinarily values/punctuation.
    if matches!(folded.as_str(), "1" | "-") {
        return None;
    }

    resolve_unit(candidate)
}

fn has_unit_start_boundary(span: &str, start: usize, first: char) -> bool {
    let Some(previous) = span[..start].chars().next_back() else {
        return true;
    };

    if previous == '.' {
        // A dot immediately before a candidate continues a scientific unit
        // (`at.%`, `MPa.m^0.5`); it is not a token boundary.
        return false;
    }
    if matches!(previous, '\'' | '\u{2019}') && matches!(first, 's' | 'S') {
        // The `s` in an English possessive ("alloy's" / "alloy’s") is
        // not a printed second unit. Treating an apostrophe as punctuation
        // here would let a fabricated `QUDT:SEC` pass grounding.
        return false;
    }
    if is_unit_connector(previous) {
        return false;
    }
    if previous.is_alphabetic() || previous == '_' {
        // An opening parenthesis can itself delimit a following unit, as in
        // `value(GPa)`. Starting at the `G` is disallowed; starting at `(`
        // lets the resolver consume the complete parenthesised spelling.
        return first == '(';
    }
    !(previous.is_numeric() && first.is_numeric())
}

fn has_unit_end_boundary(span: &str, end: usize) -> bool {
    let Some(next) = span[end..].chars().next() else {
        return true;
    };
    if next == '.' {
        return span[end + next.len_utf8()..]
            .chars()
            .next()
            .is_none_or(|after| !after.is_alphanumeric());
    }
    !next.is_alphanumeric() && next != '_' && !is_unit_connector(next)
}

fn is_unit_connector(c: char) -> bool {
    matches!(
        c,
        '/' | ':'
            | '-'
            | '−'
            | '–'
            | '^'
            | '⁻'
            | '¹'
            | '²'
            | '³'
            | '·'
            | '⋅'
            | '∙'
            | '×'
            | '*'
            | '°'
            | '('
            | ')'
    )
}

/// Every identifier the spelling table can produce — the set a `QUDT:` value
/// is checked against. Derived from `UNIT_SPELLINGS` so the two cannot drift:
/// adding a spelling adds its identifier automatically.
static KNOWN_IDENTIFIERS: std::sync::LazyLock<std::collections::HashSet<&'static str>> =
    std::sync::LazyLock::new(|| UNIT_SPELLINGS.iter().map(|(_, id)| *id).collect());

/// Table lookup under [`fold`]ing. The identifiers in the table are known
/// valid, so construction cannot fail.
fn lookup(spelling: &str) -> Option<QudtUnit> {
    let folded = fold(spelling);
    UNIT_SPELLINGS
        .iter()
        .find(|(candidate, _)| *candidate == folded)
        .map(|(_, identifier)| {
            QudtUnit::new(*identifier).expect("UNIT_SPELLINGS holds only valid QUDT identifiers")
        })
}

/// Canonical form a spelling is looked up under: lowercase; whitespace,
/// parentheses, carets and multiplication dots removed; superscript and
/// typographic characters mapped to ASCII (`³`→`3`, `⁻¹`→`-1`, `−`→`-`,
/// `°`→`deg`, `℃`→`degc`, `µ`→`u`).
fn fold(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.trim().chars() {
        match c {
            c if c.is_whitespace() => {}
            '(' | ')' | '^' | '·' | '⋅' | '∙' | '×' | '*' => {}
            '²' => out.push('2'),
            '³' => out.push('3'),
            '¹' => out.push('1'),
            '⁻' | '−' | '–' => out.push('-'),
            '°' => out.push_str("deg"),
            '℃' => out.push_str("degc"),
            'µ' | 'μ' => out.push('u'),
            _ => out.extend(c.to_lowercase()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every table row must construct — a typo'd identifier here would only
    /// surface as an extraction-time panic otherwise.
    #[test]
    fn every_table_entry_is_a_valid_qudt_identifier() {
        for (spelling, identifier) in UNIT_SPELLINGS {
            QudtUnit::new(*identifier).unwrap_or_else(|e| {
                panic!("table row ({spelling:?}, {identifier:?}) is not constructible: {e}")
            });
        }
    }

    /// Table keys must already be in folded form, or the row is unreachable.
    #[test]
    fn every_table_key_is_already_folded() {
        for (spelling, _) in UNIT_SPELLINGS {
            assert_eq!(
                fold(spelling),
                *spelling,
                "table key {spelling:?} is not in folded form — the lookup can never hit it"
            );
        }
    }

    #[test]
    fn common_paper_spellings_resolve() {
        for (raw, expected) in [
            ("MPa", "QUDT:MegaPA"),
            (" MPa ", "QUDT:MegaPA"),
            ("megapascal", "QUDT:MegaPA"),
            ("Mega Pascal", "QUDT:MegaPA"),
            ("N/mm²", "QUDT:MegaPA"),
            ("GPa", "QUDT:GigaPA"),
            ("Pa", "QUDT:PA"),
            ("kPa", "QUDT:KiloPA"),
            ("K", "QUDT:K"),
            ("kelvin", "QUDT:K"),
            ("°C", "QUDT:DEG_C"),
            ("deg C", "QUDT:DEG_C"),
            ("℃", "QUDT:DEG_C"),
            ("g/cm³", "QUDT:GM-PER-CentiM3"),
            ("g/cm3", "QUDT:GM-PER-CentiM3"),
            ("g/cm^3", "QUDT:GM-PER-CentiM3"),
            ("g/cc", "QUDT:GM-PER-CentiM3"),
            ("g cm−3", "QUDT:GM-PER-CentiM3"),
            ("kg/m3", "QUDT:KiloGM-PER-M3"),
            ("kg/m³", "QUDT:KiloGM-PER-M3"),
            ("W/(m·K)", "QUDT:W-PER-M-K"),
            ("W/m·K", "QUDT:W-PER-M-K"),
            ("W/m/K", "QUDT:W-PER-M-K"),
            ("W m⁻¹ K⁻¹", "QUDT:W-PER-M-K"),
            ("%", "QUDT:PERCENT"),
            ("percent", "QUDT:PERCENT"),
            ("eV", "QUDT:EV"),
            ("keV", "QUDT:KiloEV"),
        ] {
            let resolved =
                resolve_unit(raw).unwrap_or_else(|| panic!("{raw:?} must resolve to {expected}"));
            assert_eq!(resolved.as_str(), expected, "for spelling {raw:?}");
        }
    }

    #[test]
    fn source_spans_find_printed_aliases_and_canonical_units() {
        for (span, expected) in [
            ("The elastic modulus was 1.20 GPa.", "QUDT:GigaPA"),
            ("Yield strength reached 950 MPa.", "QUDT:MegaPA"),
            ("Thermal conductivity was 18 W/(m·K).", "QUDT:W-PER-M-K"),
            ("The measured density was 7.9 g/cm³.", "QUDT:GM-PER-CentiM3"),
            ("Porosity remained below 2%.", "QUDT:PERCENT"),
            ("The normalized value is 1 QUDT:GigaPA.", "QUDT:GigaPA"),
        ] {
            let expected = QudtUnit::new(expected).unwrap();
            assert!(
                span_contains_resolved_unit(span, &expected),
                "span did not ground {}: {span:?}",
                expected.as_str()
            );
            assert!(span_contains_any_resolved_unit(span));
        }
    }

    #[test]
    fn source_span_requires_the_exact_resolved_unit() {
        let gigapascal = QudtUnit::new("QUDT:GigaPA").unwrap();
        let megapascal = QudtUnit::new("QUDT:MegaPA").unwrap();

        assert!(span_contains_resolved_unit(
            "Yield strength reached 950 MPa.",
            &megapascal
        ));
        assert!(!span_contains_resolved_unit(
            "Yield strength reached 950 MPa.",
            &gigapascal
        ));

        let mixed = "UTS reached 950 MPa at 300 K.";
        let value_end = mixed.find("950").unwrap() + "950".len();
        assert!(span_value_has_resolved_unit(mixed, value_end, &megapascal));
        assert!(!span_value_has_resolved_unit(
            mixed,
            value_end,
            &QudtUnit::new("QUDT:K").unwrap()
        ));
    }

    #[test]
    fn source_span_unit_matches_respect_lexical_boundaries() {
        let kelvin = QudtUnit::new("QUDT:K").unwrap();
        let ampere = QudtUnit::new("QUDT:A").unwrap();
        let second = QudtUnit::new("QUDT:SEC").unwrap();
        let watt = QudtUnit::new("QUDT:W").unwrap();

        assert!(!span_contains_resolved_unit(
            "PEEK retained its stiffness.",
            &kelvin
        ));
        assert!(!span_contains_resolved_unit(
            "The alloy was selected from a sample.",
            &ampere
        ));
        assert!(!span_contains_resolved_unit(
            "A specimen had conductivity 5 S/m.",
            &ampere
        ));
        assert!(!span_contains_resolved_unit("The rate was 1/s.", &second));
        assert!(!span_contains_resolved_unit(
            "Alloy's UTS was 950 MPa.",
            &second
        ));
        assert!(!span_contains_resolved_unit(
            "The alloy’s UTS was 950 MPa.",
            &second
        ));
        assert!(!span_contains_resolved_unit(
            "Conductivity was 18 W/(m·K).",
            &watt
        ));

        assert!(span_contains_resolved_unit(
            "The temperature was 300 K.",
            &kelvin
        ));
        assert!(span_contains_resolved_unit(
            "The current was held at 4 A.",
            &ampere
        ));
        assert!(!span_contains_resolved_unit(
            "UTS was 950 MPa (N = 3).",
            &QudtUnit::new("QUDT:N").unwrap()
        ));
        assert!(!span_contains_resolved_unit(
            "The result(s) were reproducible.",
            &second
        ));
        assert!(!span_contains_resolved_unit(
            "See Ref. 5 N. Smith for details.",
            &QudtUnit::new("QUDT:N").unwrap()
        ));
        assert!(!span_contains_resolved_unit(
            "Nickel content was 5 at.% Ni.",
            &QudtUnit::new("QUDT:PERCENT").unwrap()
        ));
        assert!(!span_contains_resolved_unit(
            "Fracture toughness was 20 MPa.m^0.5.",
            &QudtUnit::new("QUDT:MegaPA").unwrap()
        ));
        assert!(!span_contains_any_resolved_unit(
            "The ratio was reported as -1."
        ));
    }

    /// Reading the printed unit off a value shares the checking scanner:
    /// only the unit in quantity position immediately after THIS value is
    /// returned, compound spellings are read whole, and a span with nothing
    /// resolvable adjacent returns nothing rather than a guess.
    #[test]
    fn adjacent_unit_is_read_from_quantity_position() {
        let mixed = "UTS reached 950 MPa at 300 K.";
        let after_950 = mixed.find("950").unwrap() + "950".len();
        assert_eq!(
            span_value_resolved_adjacent_unit(mixed, after_950)
                .unwrap()
                .as_str(),
            "QUDT:MegaPA"
        );
        let after_300 = mixed.find("300").unwrap() + "300".len();
        assert_eq!(
            span_value_resolved_adjacent_unit(mixed, after_300)
                .unwrap()
                .as_str(),
            "QUDT:K"
        );

        let compound = "The scan speed was 1250 mm/s.";
        let after_1250 = compound.find("1250").unwrap() + "1250".len();
        assert_eq!(
            span_value_resolved_adjacent_unit(compound, after_1250)
                .unwrap()
                .as_str(),
            "QUDT:MilliM-PER-SEC"
        );

        let bare = "The count was 950 samples overall.";
        let after_count = bare.find("950").unwrap() + "950".len();
        assert!(span_value_resolved_adjacent_unit(bare, after_count).is_none());
        // Out-of-range offsets fail closed.
        assert!(span_value_resolved_adjacent_unit(mixed, mixed.len() + 1).is_none());
    }

    #[test]
    fn qudt_identifiers_pass_through_untouched() {
        assert_eq!(
            resolve_unit("QUDT:W-PER-M-K").unwrap().as_str(),
            "QUDT:W-PER-M-K"
        );
        assert_eq!(resolve_unit("QUDT:MegaPA").unwrap().as_str(), "QUDT:MegaPA");
        assert_eq!(resolve_unit("qudt:gigapa").unwrap().as_str(), "QUDT:GigaPA");
    }

    /// Every canonical identifier in the table must be a FIXED POINT of
    /// resolution — the local-name canonicalisation below must never turn
    /// one valid identifier into a different one.
    #[test]
    fn canonical_table_identifiers_are_fixed_points() {
        for (_, identifier) in UNIT_SPELLINGS {
            assert_eq!(
                resolve_unit(identifier).unwrap().as_str(),
                *identifier,
                "{identifier:?} was rewritten by its own resolver"
            );
        }
    }

    /// Observed in a live ingest: `qwen2.5:3b` writes `QUDT:MPa` — the
    /// prefix satisfies `QudtUnit::new`, but `MPa` names no QUDT unit
    /// (megapascal's local name is `MegaPA`), so the pseudo-identifier
    /// reached the store verbatim. A QUDT-prefixed KNOWN SPELLING must
    /// canonicalise; unknown local names still pass through.
    #[test]
    fn qudt_prefixed_spellings_are_canonicalised() {
        assert_eq!(resolve_unit("QUDT:MPa").unwrap().as_str(), "QUDT:MegaPA");
        assert_eq!(resolve_unit("QUDT:GPa").unwrap().as_str(), "QUDT:GigaPA");
        assert_eq!(resolve_unit("QUDT:kelvin").unwrap().as_str(), "QUDT:K");
        // An unknown local name is REFUSED. This reverses an earlier
        // decision to pass it through ("QUDT's identifier space stays
        // open"), because the open space was measured in a live store and
        // it was carrying wrong data: `QUDT:UM` (the identifier is `MicroM`,
        // and it arrived because `Ra = 0.025 µm` had lost its µ),
        // `QUDT:HRC` (Rockwell C is not a QUDT unit at all), and `QUDT:nm`
        // sitting beside `QUDT:Nanometer` — one unit under two identities.
        // A prefix is not a namespace check, and the ingest log meanwhile
        // told users that any unresolvable unit had been dropped.
        //
        // The cost is real and accepted: a genuine QUDT unit absent from the
        // table is refused until it is added. A refused fact is reported
        // with its reason; a wrongly-typed number is not.
        assert!(resolve_unit("QUDT:N-PER-M2").is_none());
        // Rockwell C is a real hardness scale and NOT a QUDT unit; refused
        // rather than stored under an identifier that does not exist.
        assert!(resolve_unit("QUDT:HRC").is_none());
        // Measured on a live ingest: the model writes the DESCRIPTIVE form of
        // a unit this table already knows. Both spellings must land on the
        // SAME identifier — one unit, one identity.
        assert_eq!(
            resolve_unit("QUDT:Meter-Per-Second").map(|u| u.as_str().to_string()),
            resolve_unit("m/s").map(|u| u.as_str().to_string()),
        );
        assert_eq!(
            resolve_unit("QUDT:Meter-Per-Second").unwrap().as_str(),
            "QUDT:M-PER-SEC"
        );
        // A descriptive spelling for a unit NOT in the vocabulary is still
        // refused — no identifier is invented to make an extraction fit.
        assert!(resolve_unit("QUDT:Inverse-Cubic-Meter").is_none());
        assert!(resolve_unit("QUDT:Furlong").is_none());
        assert!(resolve_unit("QUDT:Rankine").is_none());
        // `QUDT:Cycle` USED to sit in the list above. It moved here when the
        // count vocabulary was added for a measured LPBF fatigue paper: a
        // cycle is dimensionless, so a cycle count is QUDT:NUM. The rule did
        // not change — an invented identifier is still never coined — the
        // VOCABULARY grew, and `Cycle` is now a known spelling that
        // canonicalises like `QUDT:UM` does below.
        assert_eq!(resolve_unit("QUDT:Cycle").unwrap().as_str(), "QUDT:NUM");
        assert_eq!(
            resolve_unit("QUDT:Cycle").map(|u| u.as_str().to_string()),
            resolve_unit("cycles").map(|u| u.as_str().to_string()),
        );
        // A KNOWN spelling wearing the prefix is canonicalised, not refused —
        // `QUDT:UM` means micrometre and now lands on the one identifier for
        // it instead of being stored as its own unit.
        assert_eq!(resolve_unit("QUDT:UM").unwrap().as_str(), "QUDT:MicroM");
        // …and the spelling that MEANS micrometre still resolves, to the
        // one identifier QUDT actually uses.
        assert_eq!(resolve_unit("um").unwrap().as_str(), "QUDT:MicroM");
        assert_eq!(resolve_unit("µm").unwrap().as_str(), "QUDT:MicroM");
        assert_eq!(resolve_unit("nm").unwrap().as_str(), "QUDT:NanoM");
        assert_eq!(
            resolve_unit("QUDT:Nanometer").unwrap().as_str(),
            "QUDT:NanoM"
        );
    }

    /// Unknown spellings resolve to NOTHING — never a guess. `wt%` is here
    /// on purpose: mapping it to bare percent would erase the weight basis.
    #[test]
    fn unknown_or_ambiguous_spellings_do_not_resolve() {
        for raw in ["banana", "", "QUDT:", "units", "wt%", "at.%", "furlongs"] {
            assert!(
                resolve_unit(raw).is_none(),
                "{raw:?} must not resolve to anything"
            );
        }
    }
}
