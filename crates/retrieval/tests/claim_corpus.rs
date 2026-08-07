//! The two-sided claim corpus: one table, one test, both directions.
//!
//! Seven consecutive rounds on `fix/claim-fabrication` fixed one
//! direction and broke the other, each time passing a green suite,
//! because every reviewer measured regressions against an OUT-OF-TREE
//! corpus that evaporated, while the in-tree suite asserted only the
//! direction the latest patch cared about (45 -> 81 tests, zero of the
//! seven regressions caught). This file is the corpus rebuilt in-tree
//! from everything measured this session: the label/sample/run family,
//! the unit-glyph family, designation suffixes, scientific notation,
//! ranges, citations, both table models (both JATS models emit
//! newline-separated rows at the text level), sign handling, and the
//! recall forms.
//!
//! Each case is (prose, subject, object, value, expectation, reason).
//! ONE test runs the whole table and fails with the two-sided
//! scoreboard: how many MUST_STAMP cases dropped AND how many
//! MUST_DROP cases stamped, with every failing case listed under its
//! own axis. A change to the matcher pays its cost on BOTH axes here,
//! in the normal suite run, without a reviewer rebuilding the harness.
//!
//! `known: true` marks a KNOWN failure: the expectation records what
//! the code SHOULD do but does not do yet; the case is documented
//! instead of (yet) fixed. The test does not fail on KNOWN
//! failures — but it DOES fail when a KNOWN case starts passing: the
//! marker must then be removed, so the record cannot go stale in
//! either direction. KNOWN failures are documented, not hidden.

use prism_retrieval::claims::supporting_quote;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Expect {
    MustStamp,
    MustDrop,
}

struct CorpusCase {
    prose: &'static str,
    subject: &'static str,
    object: &'static str,
    value: f64,
    expect: Expect,
    reason: &'static str,
    /// KNOWN failure of the current code; see the module docs.
    known: bool,
}

fn case(
    prose: &'static str,
    subject: &'static str,
    object: &'static str,
    value: f64,
    expect: Expect,
    reason: &'static str,
) -> CorpusCase {
    CorpusCase {
        prose,
        subject,
        object,
        value,
        expect,
        reason,
        known: false,
    }
}

fn known(
    prose: &'static str,
    subject: &'static str,
    object: &'static str,
    value: f64,
    expect: Expect,
    reason: &'static str,
) -> CorpusCase {
    CorpusCase {
        prose,
        subject,
        object,
        value,
        expect,
        reason,
        known: true,
    }
}

fn corpus() -> Vec<CorpusCase> {
    vec![
        // ---------------- MUST_STAMP: label/sample/run family -------
        // The unit exemption at the top of `preceding_word_is_label`:
        // a label number never carries a spaced unit, a measurement
        // always does.
        case(
            "Each Ti-6Al-4V sample 3 mm thick was ground and polished.",
            "Ti-6Al-4V",
            "thickness",
            3.0,
            Expect::MustStamp,
            "methods prose: spaced unit exempts the label word sample",
        ),
        case(
            "Each Inconel 718 run 30 min at 980 \u{b0}C was quenched.",
            "Inconel 718",
            "duration",
            30.0,
            Expect::MustStamp,
            "recall form pinned at 2363837a: run 30 min",
        ),
        case(
            "The AlSi10Mg samples 5 mm thick were sectioned.",
            "AlSi10Mg",
            "thickness",
            5.0,
            Expect::MustStamp,
            "recall form pinned at 2363837a: samples 5 mm",
        ),
        case(
            "A cross-section 10 mm above the build plate was examined for AlSi10Mg.",
            "AlSi10Mg",
            "height",
            10.0,
            Expect::MustStamp,
            "the exemption also frees the label word section",
        ),
        // ---------------- MUST_STAMP: glued-unit family --------------
        case(
            "The Ti-6Al-4V UTS is 950MPa.",
            "Ti-6Al-4V",
            "UTS",
            950.0,
            Expect::MustStamp,
            "glued unit: boundary redeems the number",
        ),
        case(
            "Ti-6Al-4V was annealed at 1073K.",
            "Ti-6Al-4V",
            "temperature",
            1073.0,
            Expect::MustStamp,
            "glued unit: kelvin",
        ),
        case(
            "CoCrFeNi grains of 50um were observed.",
            "CoCrFeNi",
            "grain_size",
            50.0,
            Expect::MustStamp,
            "glued unit: ascii um",
        ),
        case(
            "The CoCrFeNi alloy contains 5wt% Cr.",
            "CoCrFeNi",
            "content",
            5.0,
            Expect::MustStamp,
            "glued percent is non-alphanumeric and never broke",
        ),
        case(
            "AlSi10Mg was built with a 30\u{3bc}m layer thickness.",
            "AlSi10Mg",
            "layer_thickness",
            30.0,
            Expect::MustStamp,
            "unit glyph: U+03BC GREEK MU, the form PDF extractors emit",
        ),
        case(
            "Inconel 718 was solution treated at 980oC.",
            "Inconel 718",
            "temperature",
            980.0,
            Expect::MustStamp,
            "unit glyph: the o mangle of the degree sign; o rides on the ohm token",
        ),
        case(
            "The Inconel 718 powder was blended at 1000rpm for 30 min.",
            "Inconel 718",
            "rotation_speed",
            1000.0,
            Expect::MustStamp,
            "glued rpm; the r initial is derived from the rpm token",
        ),
        case(
            "The Ti-6Al-4V beta lattice parameter was 2.95\u{c5}.",
            "Ti-6Al-4V",
            "lattice_parameter",
            2.95,
            Expect::MustStamp,
            "unit glyph: angstrom, lowercased by containment normalization",
        ),
        case(
            "The Ti-6Al-4V chamber was held at 5bar of argon.",
            "Ti-6Al-4V",
            "pressure",
            5.0,
            Expect::MustStamp,
            "bar had NO test before round 8; 5bar is a round-7 recall win",
        ),
        // ---------------- MUST_STAMP: sign handling ------------------
        case(
            "The CoCrFeNi Seebeck coefficient was \u{2212}11.5 uV/K.",
            "CoCrFeNi",
            "Seebeck coefficient",
            -11.5,
            Expect::MustStamp,
            "signed negative: U+2212 is a needle glyph",
        ),
        case(
            "The Ti-6Al-4V samples were tested at \u{2212}196 \u{b0}C.",
            "Ti-6Al-4V",
            "test_temperature",
            -196.0,
            Expect::MustStamp,
            "cryogenic control pinned at e41f7199",
        ),
        case(
            "The residual stress in Ti-6Al-4V was \u{2212}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            -350.0,
            Expect::MustStamp,
            "the true negative claim stamps through the U+2212 needle",
        ),
        case(
            "From Fig. 6, \u{2212}950 MPa was the Ti-6Al-4V surface stress.",
            "Ti-6Al-4V",
            "surface_stress",
            -950.0,
            Expect::MustStamp,
            "signed value after a label comma still stamps",
        ),
        // ---------------- MUST_STAMP: ranges and lists ---------------
        case(
            "Ti-6Al-4V powder layers of 30-50um were deposited.",
            "Ti-6Al-4V",
            "layer_thickness",
            50.0,
            Expect::MustStamp,
            "ASCII-dash range with glued unit: the high endpoint stamps",
        ),
        case(
            "The Ti-6Al-4V batches 950-1100 were tested.",
            "Ti-6Al-4V",
            "batch_id",
            1100.0,
            Expect::MustStamp,
            "ASCII hyphen keeps compound-friendly behaviour",
        ),
        case(
            "In Table 5, 950, 960 and 970 MPa were measured for Ti-6Al-4V.",
            "Ti-6Al-4V",
            "UTS",
            960.0,
            Expect::MustStamp,
            "value list after a label locator: mid value",
        ),
        case(
            "In Table 5, 950, 960 and 970 MPa were measured for Ti-6Al-4V.",
            "Ti-6Al-4V",
            "UTS",
            970.0,
            Expect::MustStamp,
            "value list after a label locator: the unit-bearing tail",
        ),
        case(
            "The Ti-6Al-4V batches were 3.1 and 4.",
            "Ti-6Al-4V",
            "batch_id",
            4.0,
            Expect::MustStamp,
            "dotted value list walks to its real head word, not a label",
        ),
        case(
            "The residual stress in Ti-6Al-4V was \u{2013}350 MPa as built and \
             400 MPa after annealing.",
            "Ti-6Al-4V",
            "stress",
            400.0,
            Expect::MustStamp,
            "a genuine point value beside an en-dash minus still stamps",
        ),
        case(
            "Figure 3 shows a UTS of 950 MPa.",
            "Ti-6Al-4V",
            "UTS",
            950.0,
            Expect::MustStamp,
            "a value after a label locator in the same sentence stamps",
        ),
        // ---------------- MUST_STAMP: table rows ---------------------
        case(
            "Table 1 UTS of Ti-6Al-4V and Inconel 718\n\
             Alloy UTS (MPa)\n\
             Ti-6Al-4V 950\n\
             Inconel 718 1375",
            "Ti-6Al-4V",
            "UTS",
            950.0,
            Expect::MustStamp,
            "both JATS table models emit newline rows: a genuine row stamps",
        ),
        // ---------------- MUST_STAMP: H2 spaced controls -------------
        case(
            "The Ti-6Al-4V band gap was 5 ev.",
            "Ti-6Al-4V",
            "band_gap",
            5.0,
            Expect::MustStamp,
            "spaced ev still stamps after denying the glued e initial",
        ),
        known(
            "The Ti-6Al-4V coupons were stored at 72F.",
            "Ti-6Al-4V",
            "storage_temperature",
            72.0,
            Expect::MustStamp,
            "recall lost untested when round 7 switched to pure derivation \
             and dropped the f initial (Fahrenheit)",
        ),
        known(
            "The Ti-6Al-4V powder tank holds 50l.",
            "Ti-6Al-4V",
            "tank_volume",
            50.0,
            Expect::MustStamp,
            "recall lost untested when round 7 switched to pure derivation \
             and dropped the l initial (litres)",
        ),
        // ---------------- MUST_DROP: citations -----------------------
        case(
            "Ti-6Al-4V has been studied extensively in prior work [1140].",
            "Ti-6Al-4V",
            "UTS",
            1140.0,
            Expect::MustDrop,
            "a bracketed citation marker is not evidence",
        ),
        case(
            "Ti-6Al-4V has been widely studied (1140).",
            "Ti-6Al-4V",
            "UTS",
            1140.0,
            Expect::MustDrop,
            "a bare parenthesised number is a citation",
        ),
        // ---------------- MUST_DROP: labels --------------------------
        case(
            "Ti-6Al-4V properties are listed in Table 3.",
            "Ti-6Al-4V",
            "UTS",
            3.0,
            Expect::MustDrop,
            "a Table label number is not a measurement",
        ),
        case(
            "Ti-6Al-4V data appear in Figure 2.",
            "Ti-6Al-4V",
            "UTS",
            2.0,
            Expect::MustDrop,
            "a Figure label number is not a measurement",
        ),
        case(
            "Figure 2a shows the AlSi10Mg porosity.",
            "AlSi10Mg",
            "porosity",
            2.0,
            Expect::MustDrop,
            "H1 closed round 8: a GLUED sub-panel letter must not exempt the label word",
        ),
        case(
            "Sample 5 of Ti-6Al-4V was tested.",
            "Ti-6Al-4V",
            "UTS",
            5.0,
            Expect::MustDrop,
            "a specimen label number carries no unit",
        ),
        case(
            "Run 12 of the Inconel 718 build failed.",
            "Inconel 718",
            "build_failure",
            12.0,
            Expect::MustDrop,
            "a batch label number carries no unit",
        ),
        case(
            "Ti-6Al-4V is discussed in Refs. 25, 26.",
            "Ti-6Al-4V",
            "UTS",
            26.0,
            Expect::MustDrop,
            "comma reference lists walk back to the head word",
        ),
        case(
            "The Ti-6Al-4V data are listed in Tables 1 and 2.",
            "Ti-6Al-4V",
            "UTS",
            2.0,
            Expect::MustDrop,
            "label lists across a conjunction are labels",
        ),
        case(
            "Inconel 718 data are in Sections 3.1 and 4.",
            "Inconel 718",
            "UTS",
            4.0,
            Expect::MustDrop,
            "dotted label lists are labels",
        ),
        // ---------------- MUST_DROP: designation digits --------------
        case(
            "The Ti-6Al-4V samples were annealed and examined.",
            "Ti-6Al-4V",
            "UTS",
            6.0,
            Expect::MustDrop,
            "digits inside a glued designation are not prose",
        ),
        case(
            "The Ti-6Al-4V samples were annealed and examined.",
            "Ti-6Al-4V",
            "UTS",
            4.0,
            Expect::MustDrop,
            "digits inside a glued designation are not prose",
        ),
        case(
            "The Ti-6Al-4V UTS is 950 MPa.",
            "Ti-6Al-4V",
            "UTS",
            95.0,
            Expect::MustDrop,
            "95 is a substring of 950",
        ),
        // ---------------- MUST_DROP: H2 — glued e and x --------------
        known(
            "The Ti-6Al-4V strain rate was 2e5 per second.",
            "Ti-6Al-4V",
            "strain_rate",
            2.0,
            Expect::MustDrop,
            "H2: 2e5 is scientific notation, not 2 + a unit; round 7's \
             derivation admits e via the ev token",
        ),
        known(
            "Ti-6Al-4V ran 1e6 cycles to failure.",
            "Ti-6Al-4V",
            "cycles_to_failure",
            1.0,
            Expect::MustDrop,
            "H2: 1e6 is scientific notation; same e initial",
        ),
        case(
            "The Ti-6Al-4V coupon was imaged at 950x magnification.",
            "Ti-6Al-4V",
            "UTS",
            950.0,
            Expect::MustDrop,
            "H2 pin: 950x is magnification, not 950 + a unit",
        ),
        known(
            "The Ti-6Al-4V tensile tests followed ASTM E8-16e1.",
            "Ti-6Al-4V",
            "elongation",
            16.0,
            Expect::MustDrop,
            "designation suffix: denying e closes this one of the five; \
             round 7's e initial stamps it",
        ),
        // ---------------- MUST_DROP: ranges --------------------------
        case(
            "The Ti-6Al-4V UTS ranged from 950\u{2013}1100 MPa.",
            "Ti-6Al-4V",
            "UTS",
            950.0,
            Expect::MustDrop,
            "an en-dash range low endpoint is not a point value",
        ),
        case(
            "The Ti-6Al-4V UTS ranged from 950\u{2013}1100 MPa.",
            "Ti-6Al-4V",
            "UTS",
            1100.0,
            Expect::MustDrop,
            "an en-dash range high endpoint is not a point value",
        ),
        // ---------------- MUST_DROP: sign flips ----------------------
        case(
            "The residual stress in Ti-6Al-4V was \u{2212}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            350.0,
            Expect::MustDrop,
            "the sign-flipped twin of a U+2212 negative must not stamp",
        ),
        case(
            "The residual stress in Ti-6Al-4V was \u{2013}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            350.0,
            Expect::MustDrop,
            "the round-7 fix: the U+2013-minus sign flip drops",
        ),
        case(
            "The residual stress in Ti-6Al-4V was \u{2013}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            -350.0,
            Expect::MustDrop,
            "accepted drop: U+2013 is not a needle glyph, the true negative has no needle",
        ),
        case(
            "The residual stress in Ti-6Al-4V was \u{2014}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            -350.0,
            Expect::MustDrop,
            "accepted drop: U+2014 is not a needle glyph either",
        ),
        // ---------------- MUST_DROP: tables --------------------------
        case(
            "Table 1 UTS of Ti-6Al-4V and Inconel 718\n\
             Alloy UTS (MPa)\n\
             Ti-6Al-4V 950\n\
             Inconel 718 1375",
            "Ti-6Al-4V",
            "UTS",
            1375.0,
            Expect::MustDrop,
            "rows are separate spans: Inconel's number cannot support Ti-6Al-4V",
        ),
        case(
            "Table 1 UTS of Ti-6Al-4V and Inconel 718\n\
             Alloy UTS (MPa)\n\
             Ti-6Al-4V 950\n\
             Inconel 718 1375",
            "Ti-6Al-4V",
            "UTS",
            1.0,
            Expect::MustDrop,
            "the 1 of Table 1 is a label, not a UTS value",
        ),
        // ---------------- KNOWN failures ------------------------------
        // Documented, deliberately not fixed. If one starts passing,
        // this test fails until the marker is removed.
        known(
            "Table 4 K values for the Inconel 718 conductivity are listed.",
            "Inconel 718",
            "conductivity",
            4.0,
            Expect::MustDrop,
            "KNOWN: a SPACED single-letter unit still exempts the label word; \
             the round-8 space fix closes the glued form only",
        ),
        known(
            "In Eq. 3 n denotes the Ti-6Al-4V cycle count.",
            "Ti-6Al-4V",
            "cycle_count",
            3.0,
            Expect::MustDrop,
            "KNOWN: same residue — spaced single-letter unit exempts a label number",
        ),
        known(
            "The residual stress in Ti-6Al-4V was \u{2014}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            350.0,
            Expect::MustDrop,
            "KNOWN: U+2014 as minus is the unrecorded twin of the U+2013 fix; \
             the sign-flipped twin still stamps +350",
        ),
        known(
            "The Ti-6Al-4V fracture tests followed ASTM E1820-20b.",
            "Ti-6Al-4V",
            "fracture_toughness",
            20.0,
            Expect::MustDrop,
            "KNOWN: a digit-joined standard designator with a unit-initial suffix; \
             closing it needs a standard-designator guard, not dash surgery",
        ),
    ]
}

/// The two-sided scoreboard. Every future change to the matcher shows
/// its cost on BOTH axes here: how many MUST_STAMP cases it dropped,
/// and how many MUST_DROP cases it stamped. See the module docs for
/// the KNOWN-failure tripwire.
#[test]
fn claim_corpus_two_sided_scoreboard() {
    let mut stamped_when_must_drop: Vec<String> = Vec::new();
    let mut dropped_when_must_stamp: Vec<String> = Vec::new();
    let mut known_held: Vec<String> = Vec::new();
    let mut known_fixed: Vec<String> = Vec::new();

    for case in corpus() {
        let stamped =
            supporting_quote(case.subject, case.object, Some(case.value), case.prose).is_some();
        let ok = match case.expect {
            Expect::MustStamp => stamped,
            Expect::MustDrop => !stamped,
        };
        let line = format!(
            "  {} / {} = {} in {:?}\n    reason: {}",
            case.subject, case.object, case.value, case.prose, case.reason
        );
        if ok {
            if case.known {
                known_fixed.push(line);
            }
        } else if case.known {
            known_held.push(line);
        } else {
            match case.expect {
                Expect::MustStamp => dropped_when_must_stamp.push(line),
                Expect::MustDrop => stamped_when_must_drop.push(line),
            }
        }
    }

    let failed = !stamped_when_must_drop.is_empty()
        || !dropped_when_must_stamp.is_empty()
        || !known_fixed.is_empty();
    if !failed {
        return;
    }

    let mut msg = format!(
        "claim corpus scoreboard:\n  MUST_STAMP cases dropped: {}\n  \
         MUST_DROP cases stamped:  {}\n  KNOWN failures held:       {}\n  \
         KNOWN failures fixed:    {}\n",
        dropped_when_must_stamp.len(),
        stamped_when_must_drop.len(),
        known_held.len(),
        known_fixed.len()
    );
    if !dropped_when_must_stamp.is_empty() {
        msg.push_str("\nMUST_STAMP but dropped (recall lost):\n");
        msg.push_str(&dropped_when_must_stamp.join("\n"));
        msg.push('\n');
    }
    if !stamped_when_must_drop.is_empty() {
        msg.push_str("\nMUST_DROP but stamped (fabrication):\n");
        msg.push_str(&stamped_when_must_drop.join("\n"));
        msg.push('\n');
    }
    if !known_fixed.is_empty() {
        msg.push_str(
            "\nKNOWN failures that now PASS — remove their `known` marker and update the record:\n",
        );
        msg.push_str(&known_fixed.join("\n"));
        msg.push('\n');
    }
    panic!("{msg}");
}
