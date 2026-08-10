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
    if let Some(local) = raw.strip_prefix("QUDT:") {
        if let Some(unit) = lookup(local) {
            return Some(unit);
        }
        return QudtUnit::new(raw).ok();
    }
    lookup(raw)
}

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
    fn qudt_identifiers_pass_through_untouched() {
        assert_eq!(
            resolve_unit("QUDT:W-PER-M-K").unwrap().as_str(),
            "QUDT:W-PER-M-K"
        );
        assert_eq!(resolve_unit("QUDT:MegaPA").unwrap().as_str(), "QUDT:MegaPA");
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
        // Unknown local name: passthrough, not rejection — QUDT's
        // identifier space stays open.
        assert_eq!(
            resolve_unit("QUDT:N-PER-M2").unwrap().as_str(),
            "QUDT:N-PER-M2"
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
