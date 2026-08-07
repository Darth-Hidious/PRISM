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
//!
//! A SECOND small table covers the one class the (prose, subject,
//! object, value) tuple cannot express: round-3 tautological
//! containment. It lives in `validate_and_stamp` -> `quote_in_block`,
//! which needs an `ExtractedClaim` carrying an LLM-supplied quote —
//! claim + block -> stamped/dropped, same two scoreboard axes. Round
//! 10: this table carries `known` markers under the same tripwire too —
//! the unit-mismatch gap (the claim's unit is never checked against the
//! block) is its first entry, expressible only here because only this
//! tuple has a unit field.

use prism_retrieval::claims::{
    ClaimProvenance, EVIDENCE_RESEARCH, ExtractedClaim, supporting_quote, validate_and_stamp,
};
use prism_retrieval::fulltext::{BlockKind, Locator};

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
        known(
            "A cross-section 10 mm above the build plate was examined for AlSi10Mg.",
            "AlSi10Mg",
            "thickness",
            10.0,
            Expect::MustDrop,
            "KNOWN: 10 mm is WHERE the cross-section was cut — a position in \
             the build, not a property of the alloy. Stamped as an AlSi10Mg \
             property through the label-word exemption on 'section', it is a \
             fabricated property record. Round 10 flipped this row: it \
             certified recall for a claim that is not supported. The \
             exemption's genuine recall job stays pinned by the spaced-unit \
             rows (sample 3 mm, run 30 min, samples 5 mm)",
        ),
        case(
            "A cross-section 10mm above the build plate was examined for AlSi10Mg.",
            "AlSi10Mg",
            "thickness",
            10.0,
            Expect::MustDrop,
            "round 10: the glued twin of the spaced cross-section KNOWN row — \
             a position, not a property, so ground truth is MustDrop in BOTH \
             spellings. It drops today only because H1's space requirement \
             keeps the label guard firing; if the spaceless exemption ever \
             returns, this row turns red",
        ),
        case(
            "The Ti-6Al-4V batch 25kg was melted.",
            "Ti-6Al-4V",
            "mass",
            25.0,
            Expect::MustDrop,
            "round 11: 25 kg is the mass of ONE POWDER LOT — extensive, not \
             a property of the alloy, so ground truth is MustDrop. Round 10 \
             carried it as a KNOWN MustStamp recall loss, asking the engine \
             to fabricate: the day H1's space requirement relaxes that row \
             would go green, the tripwire would strip the marker, and the \
             fabrication would be permanently certified. Flipped to MustDrop \
             like the glued cross-section twin; it drops today via the \
             'batch' label guard and this row turns red if that changes",
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
            "The AlSi10Mg scan step 30 \u{3bc}m was imaged.",
            "AlSi10Mg",
            "scan_step_size",
            30.0,
            Expect::MustStamp,
            "round 10: the SPACED U+03BC form. \u{3bc}m moved from \
             EXTRA_UNIT_INITIALS (which opens only the glued boundary path) \
             into UNIT_TOKENS, which unit_follows reads — before the move \
             this dropped while its U+00B5 twin stamped",
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
        case(
            "The Ti-6Al-4V residual stress was -1350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            -1350.0,
            Expect::MustStamp,
            "round 11: the pdf-extract spelling of a compressive stress \
             beyond 1000 MPa stamps",
        ),
        case(
            "The Ti-6Al-4V residual stress was \u{2212}1350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            -1350.0,
            Expect::MustStamp,
            "round 11 item 2: the JATS spelling \u{2212}1350 (un-grouped, \
             |value| >= 1000) stamps too — the sign now attaches to the \
             PLAIN form, closing the route divergence where pdf-extract's \
             -1350 stamped while the JATS form dropped NoSpan -> \
             MissingQuote, misfiled as the model's fault",
        ),
        case(
            "The Ti-6Al-4V residual stress was \u{2212}1,350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            -1350.0,
            Expect::MustStamp,
            "round 11 item 2: the comma-grouped \u{2212}1,350 stamps — \
             every rendering of the same stress agrees across routes",
        ),
        case(
            "The Ti-6Al-4V residual stress was \u{2212}1350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            1350.0,
            Expect::MustDrop,
            "round 11 item 2: the sign-flipped twin of the un-grouped \
             \u{2212}1350 form still drops — the boundary clause owns it",
        ),
        // ---------------- MUST_STAMP: ranges and lists ---------------
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
        known(
            "The Ti-6Al-4V batches were 3.1 and 4.",
            "Ti-6Al-4V",
            "UTS",
            4.0,
            Expect::MustDrop,
            "KNOWN: batch identifiers again, reached through two gaps — \
             identifiers stamped as a property, and the plural head \
             'batches' absent from LABEL_WORDS (singular-only by round-9 \
             policy), so the dotted walk lands on a non-label word. The old \
             row praised the walk's CODE behaviour ('walks to its real head \
             word') as if that were ground truth. The walk mechanism itself \
             stays pinned by a genuine unitless value list in the lib tests",
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
            "Figure 3 shows a Ti-6Al-4V UTS of 950 MPa.",
            "Ti-6Al-4V",
            "UTS",
            950.0,
            Expect::MustStamp,
            "a value after a label locator in the same sentence stamps; \
             round 9: the subject is NAMED in the prose — the old text \
             stamped only through the subject-OR-object arm, certifying \
             subject-blind attribution",
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
            "In Fig. 3, 5 ev was measured for the Ti-6Al-4V band gap.",
            "Ti-6Al-4V",
            "band_gap",
            5.0,
            Expect::MustStamp,
            "spaced ev still stamps after denying the glued e initial; \
             round 9: moved under a label locator so removing \"ev\" from \
             UNIT_TOKENS reddens this — the original prose had no label \
             word, `unit_follows` was never consulted, and 47511ce1's \
             stated proof was void",
        ),
        case(
            "The Ti-6Al-4V coupons were stored at 72F.",
            "Ti-6Al-4V",
            "storage_temperature",
            72.0,
            Expect::MustStamp,
            "f recall restored round 8 (Fahrenheit); lost untested in round 7",
        ),
        case(
            "The Ti-6Al-4V powder tank holds 50l.",
            "Ti-6Al-4V",
            "tank_volume",
            50.0,
            Expect::MustStamp,
            "l recall restored round 8 (litres); lost untested in round 7",
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
        case(
            "Ti-6Al-4V has been studied (11, 13\u{2013}15).",
            "Ti-6Al-4V",
            "UTS",
            11.0,
            Expect::MustDrop,
            "round 11 item 14: the paren-citation FORWARD trim must walk \
             through the en dash of 13\u{2013}15 to reach the close paren; \
             without dash handling there the marker is never recognised \
             and 11 stamps — this row is that cannot-fail trim's only pin",
        ),
        // ---------------- MUST_DROP: citation dash walks (round 10) ---
        // The citation walk-back trimmed only '-', U+2013 and U+2014;
        // the other six glyphs stranded the trim on the dash, the
        // bracket was never seen, and the tail citation number stamped.
        // The rows use a comma-joined tail (14) that is NOT
        // dash-adjacent: dash-adjacent citation numbers are refused by
        // the Range guard first after round 10, so only this shape
        // pins the walk itself. One row per leaking glyph.
        case(
            "Ti-6Al-4V has been studied extensively [11\u{2010}12, 14].",
            "Ti-6Al-4V",
            "UTS",
            14.0,
            Expect::MustDrop,
            "citation dash class round 10: the walk reaches the bracket through a              U+2010 HYPHEN dash; 14 is not dash-adjacent, so the Range guard cannot              shadow this — the Citation walk owns it",
        ),
        case(
            "Ti-6Al-4V has been studied extensively [11\u{2011}12, 14].",
            "Ti-6Al-4V",
            "UTS",
            14.0,
            Expect::MustDrop,
            "citation dash class round 10: the walk reaches the bracket through a              U+2011 NON-BREAKING HYPHEN dash; 14 is not dash-adjacent, so the Range guard cannot              shadow this — the Citation walk owns it",
        ),
        case(
            "Ti-6Al-4V has been studied extensively [11\u{2012}12, 14].",
            "Ti-6Al-4V",
            "UTS",
            14.0,
            Expect::MustDrop,
            "citation dash class round 10: the walk reaches the bracket through a              U+2012 FIGURE DASH dash; 14 is not dash-adjacent, so the Range guard cannot              shadow this — the Citation walk owns it",
        ),
        case(
            "Ti-6Al-4V has been studied extensively [11\u{2015}12, 14].",
            "Ti-6Al-4V",
            "UTS",
            14.0,
            Expect::MustDrop,
            "citation dash class round 10: the walk reaches the bracket through a              U+2015 HORIZONTAL BAR dash; 14 is not dash-adjacent, so the Range guard cannot              shadow this — the Citation walk owns it",
        ),
        case(
            "Ti-6Al-4V has been studied extensively [11\u{2212}12, 14].",
            "Ti-6Al-4V",
            "UTS",
            14.0,
            Expect::MustDrop,
            "citation dash class round 10: the walk reaches the bracket through a              U+2212 MINUS SIGN dash; 14 is not dash-adjacent, so the Range guard cannot              shadow this — the Citation walk owns it",
        ),
        case(
            "Ti-6Al-4V has been studied extensively [11\u{fe63}12, 14].",
            "Ti-6Al-4V",
            "UTS",
            14.0,
            Expect::MustDrop,
            "citation dash class round 10: the walk reaches the bracket through a              U+FE63 SMALL HYPHEN-MINUS dash; 14 is not dash-adjacent, so the Range guard cannot              shadow this — the Citation walk owns it",
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
        // ---------------- MUST_DROP: label-list dash walks (round 10) -
        // The conjunction walk-back trimmed the same hand-picked dash
        // trio, so "Refs. 25<D>27 and 28" stranded on the dash and
        // stamped 28 for the six other glyphs. One row per glyph.
        case(
            "The Ti-6Al-4V data are listed in Refs. 25\u{2010}27 and 28.",
            "Ti-6Al-4V",
            "UTS",
            28.0,
            Expect::MustDrop,
            "label-list dash walk round 10: U+2010 joins the reference range",
        ),
        case(
            "The Ti-6Al-4V data are listed in Refs. 25\u{2011}27 and 28.",
            "Ti-6Al-4V",
            "UTS",
            28.0,
            Expect::MustDrop,
            "label-list dash walk round 10: U+2011 joins the reference range",
        ),
        case(
            "The Ti-6Al-4V data are listed in Refs. 25\u{2012}27 and 28.",
            "Ti-6Al-4V",
            "UTS",
            28.0,
            Expect::MustDrop,
            "label-list dash walk round 10: U+2012 joins the reference range",
        ),
        case(
            "The Ti-6Al-4V data are listed in Refs. 25\u{2015}27 and 28.",
            "Ti-6Al-4V",
            "UTS",
            28.0,
            Expect::MustDrop,
            "label-list dash walk round 10: U+2015 joins the reference range",
        ),
        case(
            "The Ti-6Al-4V data are listed in Refs. 25\u{2212}27 and 28.",
            "Ti-6Al-4V",
            "UTS",
            28.0,
            Expect::MustDrop,
            "label-list dash walk round 10: U+2212 joins the reference range",
        ),
        case(
            "The Ti-6Al-4V data are listed in Refs. 25\u{fe63}27 and 28.",
            "Ti-6Al-4V",
            "UTS",
            28.0,
            Expect::MustDrop,
            "label-list dash walk round 10: U+FE63 joins the reference range",
        ),
        // ---------------- MUST_DROP: specimen-label family (round 9) -
        // LABEL_WORDS held sample/run only; the rest of the family
        // stamped at HEAD. Sample 5 dropped while Specimen 5 stamped —
        // the corpus tested one twin and never the other, which is how
        // a family gets declared covered while half of it leaks. Each
        // word added to LABEL_WORDS is pinned by exactly one case
        // here: delete the word and its case reddens.
        case(
            "Specimen 5 of Ti-6Al-4V was tested.",
            "Ti-6Al-4V",
            "UTS",
            5.0,
            Expect::MustDrop,
            "specimen label: the untested twin of Sample 5 — it stamped at HEAD",
        ),
        case(
            "Batch 12 of the Ti-6Al-4V powder was recycled.",
            "Ti-6Al-4V",
            "UTS",
            12.0,
            Expect::MustDrop,
            "specimen label: a batch number is not a measurement",
        ),
        case(
            "Coupon 7 of the AlSi10Mg build was sectioned.",
            "AlSi10Mg",
            "UTS",
            7.0,
            Expect::MustDrop,
            "specimen label: a coupon number is not a measurement",
        ),
        case(
            "Test 3 on Ti-6Al-4V failed prematurely.",
            "Ti-6Al-4V",
            "UTS",
            3.0,
            Expect::MustDrop,
            "specimen label: a test number is not a measurement",
        ),
        case(
            "Trial 4 with Inconel 718 ran to completion.",
            "Inconel 718",
            "UTS",
            4.0,
            Expect::MustDrop,
            "specimen label: a trial number is not a measurement",
        ),
        case(
            "Experiment 2 used the CoCrFeNi powder.",
            "CoCrFeNi",
            "UTS",
            2.0,
            Expect::MustDrop,
            "specimen label: an experiment number is not a measurement",
        ),
        case(
            "Condition 3 of the Ti-6Al-4V creep test was skipped.",
            "Ti-6Al-4V",
            "creep_rate",
            3.0,
            Expect::MustDrop,
            "specimen label: a condition number is not a measurement",
        ),
        case(
            "Step 2 of the AlSi10Mg heat treatment was omitted.",
            "AlSi10Mg",
            "UTS",
            2.0,
            Expect::MustDrop,
            "specimen label: a protocol step number is not a measurement",
        ),
        case(
            "Panel 4 of the Ti-6Al-4V skin showed cracking.",
            "Ti-6Al-4V",
            "UTS",
            4.0,
            Expect::MustDrop,
            "specimen label: a panel number is not a measurement",
        ),
        case(
            "Column 3 lists the Inconel 718 hardness data.",
            "Inconel 718",
            "hardness",
            3.0,
            Expect::MustDrop,
            "specimen label: a table column number is not a measurement",
        ),
        case(
            "Row 2 of the properties table gives the Ti-6Al-4V values.",
            "Ti-6Al-4V",
            "UTS",
            2.0,
            Expect::MustDrop,
            "specimen label: a table row number is not a measurement",
        ),
        case(
            "Plot 2 shows the CoCrFeNi fatigue data.",
            "CoCrFeNi",
            "fatigue_life",
            2.0,
            Expect::MustDrop,
            "specimen label: a plot number is not a measurement",
        ),
        case(
            "Image 4 shows the Ti-6Al-4V fracture surface.",
            "Ti-6Al-4V",
            "UTS",
            4.0,
            Expect::MustDrop,
            "specimen label: an image number is not a measurement",
        ),
        case(
            "Micrograph 3 shows the AlSi10Mg melt pool.",
            "AlSi10Mg",
            "UTS",
            3.0,
            Expect::MustDrop,
            "specimen label: a micrograph number is not a measurement",
        ),
        case(
            "Curve 3 fits the Ti-6Al-4V fatigue data.",
            "Ti-6Al-4V",
            "fatigue_life",
            3.0,
            Expect::MustDrop,
            "specimen label: a curve number is not a measurement",
        ),
        case(
            "Inset 2 shows the Inconel 718 grain structure.",
            "Inconel 718",
            "UTS",
            2.0,
            Expect::MustDrop,
            "specimen label: an inset number is not a measurement",
        ),
        case(
            "The Ti-6Al-4V data appear on page 12.",
            "Ti-6Al-4V",
            "UTS",
            12.0,
            Expect::MustDrop,
            "specimen label: a page number is not a measurement",
        ),
        case(
            "Appendix 2 lists the CoCrFeNi composition data.",
            "CoCrFeNi",
            "UTS",
            2.0,
            Expect::MustDrop,
            "specimen label: an appendix number is not a measurement",
        ),
        case(
            "Grade 5 Ti-6Al-4V was fatigue tested.",
            "Ti-6Al-4V",
            "UTS",
            5.0,
            Expect::MustDrop,
            "designation doubly wrong: Ti-6Al-4V IS grade 5 — the number \
             names the alloy, it measures nothing",
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
        case(
            "The Ti-6Al-4V and Inconel 718 alloys were compared.",
            "Inconel 718",
            "hardness",
            6.0,
            Expect::MustDrop,
            "round-2 regression, missed until round 9: the 6 of Ti-6Al-4V \
             under a DIFFERENT subject — the existing cases use subject \
             Ti-6Al-4V, so occurrence_inside_name masks the boundary dash \
             clause; another subject in the same span exposes it",
        ),
        // ---------------- MUST_DROP: designation dash class (round 10) -
        // The designation guard matched only '-', U+2013 and U+2014; the
        // other six glyphs of the dash class stamped the "6" of a
        // dash-spelled Ti-6Al-4V under a different subject. U+2011
        // NON-BREAKING HYPHEN is the glyph a typesetter uses to keep
        // Ti-6Al-4V on one line, the likeliest in a real PDF. One row
        // per leaking glyph.
        case(
            "The Ti\u{2010}6Al\u{2010}4V and Inconel 718 alloys were compared.",
            "Inconel 718",
            "hardness",
            6.0,
            Expect::MustDrop,
            "designation dash class round 10: U+2010 HYPHEN spells the designation",
        ),
        case(
            "The Ti\u{2011}6Al\u{2011}4V and Inconel 718 alloys were compared.",
            "Inconel 718",
            "hardness",
            6.0,
            Expect::MustDrop,
            "designation dash class round 10: U+2011 NON-BREAKING HYPHEN, the \
             line-break-proof spelling a typesetter picks",
        ),
        case(
            "The Ti\u{2012}6Al\u{2012}4V and Inconel 718 alloys were compared.",
            "Inconel 718",
            "hardness",
            6.0,
            Expect::MustDrop,
            "designation dash class round 10: U+2012 FIGURE DASH spells the designation",
        ),
        case(
            "The Ti\u{2015}6Al\u{2015}4V and Inconel 718 alloys were compared.",
            "Inconel 718",
            "hardness",
            6.0,
            Expect::MustDrop,
            "designation dash class round 10: U+2015 HORIZONTAL BAR spells the designation",
        ),
        case(
            "The Ti\u{2212}6Al\u{2212}4V and Inconel 718 alloys were compared.",
            "Inconel 718",
            "hardness",
            6.0,
            Expect::MustDrop,
            "designation dash class round 10: U+2212 MINUS SIGN spells the designation",
        ),
        case(
            "The Ti\u{fe63}6Al\u{fe63}4V and Inconel 718 alloys were compared.",
            "Inconel 718",
            "hardness",
            6.0,
            Expect::MustDrop,
            "designation dash class round 10: U+FE63 SMALL HYPHEN-MINUS spells the designation",
        ),
        case(
            "Inconel 718 was solution treated and aged.",
            "Inconel 718",
            "UTS",
            718.0,
            Expect::MustDrop,
            "pins occurrence_inside_name itself: deleting that guard left \
             the whole corpus green at round 8 — the 718 of the spaced \
             designation has clean token boundaries and only positional \
             containment inside the name refuses it",
        ),
        // ---------------- MUST_DROP: H2 — glued e and x --------------
        case(
            "The Ti-6Al-4V strain rate was 2e5 per second.",
            "Ti-6Al-4V",
            "strain_rate",
            2.0,
            Expect::MustDrop,
            "H2 closed round 8: 2e5 is scientific notation, not 2 + a unit",
        ),
        case(
            "Ti-6Al-4V ran 1e6 cycles to failure.",
            "Ti-6Al-4V",
            "cycles_to_failure",
            1.0,
            Expect::MustDrop,
            "H2 closed round 8: 1e6 is scientific notation",
        ),
        case(
            "The Ti-6Al-4V coupon was imaged at 950x magnification.",
            "Ti-6Al-4V",
            "UTS",
            950.0,
            Expect::MustDrop,
            "H2 pin: 950x is magnification, not 950 + a unit",
        ),
        case(
            "The Ti-6Al-4V tensile tests followed ASTM E8-16e1.",
            "Ti-6Al-4V",
            "elongation",
            16.0,
            Expect::MustDrop,
            "designation suffix closed round 8: denying e drops this one of the five",
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
        // ---------------- MUST_DROP: ranges on every dash (round 10) -
        // Round 10 overturned the round-4 ASCII exception: a
        // digit/dash/digit run is a range whatever the glyph. The
        // exception stamped "950-1100" batch identifiers, "30-50um"
        // endpoints and "E1820-20b" designators as measurements —
        // fabricated property records with perfect provenance. The
        // en-dash rows above pin U+2013; one row per remaining glyph
        // pins the class at the range site, and the ASCII rows pin both
        // endpoints of the forms round 6 misread as measurements. The
        // KNOWN markers round 9 and round 10 carried are gone because
        // the range guard now refuses them.
        case(
            "Ti-6Al-4V powder layers of 30-50um were deposited.",
            "Ti-6Al-4V",
            "layer_thickness",
            50.0,
            Expect::MustDrop,
            "round 10 FIXED the round-9 KNOWN: the prose asserts 30 TO 50 um, \
             not 50; round 6 read the glued unit as redemption, round 9 \
             picked MustDrop, the range guard on the whole dash class \
             enforces it",
        ),
        case(
            "Ti-6Al-4V powder layers of 30-50um were deposited.",
            "Ti-6Al-4V",
            "layer_thickness",
            30.0,
            Expect::MustDrop,
            "round 10: the LOW endpoint of the ASCII range — both endpoints \
             are range bounds, whichever side carries the glued unit",
        ),
        case(
            "CoCrFeNi grains of 5-10mm were observed.",
            "CoCrFeNi",
            "grain_size",
            10.0,
            Expect::MustDrop,
            "round 10: the round-6 'measured harm' was a range endpoint too",
        ),
        case(
            "CoCrFeNi grains of 5-10mm were observed.",
            "CoCrFeNi",
            "grain_size",
            5.0,
            Expect::MustDrop,
            "round 10: the low endpoint of the grain-size range",
        ),
        case(
            "The Ti-6Al-4V batches 950-1100 were tested.",
            "Ti-6Al-4V",
            "UTS",
            1100.0,
            Expect::MustDrop,
            "round 10 FIXED the batch-identifier fabrication: 950-1100 is a \
             batch designator, and a batch identifier stamped as a property \
             of Ti-6Al-4V is a fabricated property record with perfect \
             provenance — the worst shape named in the module doc",
        ),
        case(
            "The Ti-6Al-4V batches 950-1100 were tested.",
            "Ti-6Al-4V",
            "UTS",
            950.0,
            Expect::MustDrop,
            "round 10: the low endpoint of the batch range",
        ),
        case(
            "The Ti-6Al-4V fracture tests followed ASTM E1820-20b.",
            "Ti-6Al-4V",
            "fracture_toughness",
            20.0,
            Expect::MustDrop,
            "round 10 FIXED the round-8 KNOWN: E1820-20b is a standard \
             designator; the digit-before-dash redemption stamped 20 through \
             the trailing unit-initial 'b' until the range guard grew the \
             whole dash class",
        ),
        case(
            "The Ti-6Al-4V fatigue tests followed ASTM E466-15a.",
            "Ti-6Al-4V",
            "fatigue_life",
            15.0,
            Expect::MustDrop,
            "round 10 FIXED the round-7 recorded residue: E466-15a is the \
             same digit/dash/digit shape; no standard-designator guard was \
             needed once the range guard covered the class",
        ),
        case(
            "The Ti-6Al-4V UTS ranged from 950\u{2010}1100 MPa.",
            "Ti-6Al-4V",
            "UTS",
            1100.0,
            Expect::MustDrop,
            "range dash class round 10: U+2010 HYPHEN joins the range",
        ),
        case(
            "The Ti-6Al-4V UTS ranged from 950\u{2011}1100 MPa.",
            "Ti-6Al-4V",
            "UTS",
            1100.0,
            Expect::MustDrop,
            "range dash class round 10: U+2011 NON-BREAKING HYPHEN joins the range",
        ),
        case(
            "The Ti-6Al-4V UTS ranged from 950\u{2012}1100 MPa.",
            "Ti-6Al-4V",
            "UTS",
            1100.0,
            Expect::MustDrop,
            "range dash class round 10: U+2012 FIGURE DASH joins the range",
        ),
        case(
            "The Ti-6Al-4V UTS ranged from 950\u{2014}1100 MPa.",
            "Ti-6Al-4V",
            "UTS",
            1100.0,
            Expect::MustDrop,
            "range dash class round 10: U+2014 EM DASH joins the range",
        ),
        case(
            "The Ti-6Al-4V UTS ranged from 950\u{2015}1100 MPa.",
            "Ti-6Al-4V",
            "UTS",
            1100.0,
            Expect::MustDrop,
            "range dash class round 10: U+2015 HORIZONTAL BAR joins the range",
        ),
        case(
            "The Ti-6Al-4V UTS ranged from 950\u{2212}1100 MPa.",
            "Ti-6Al-4V",
            "UTS",
            1100.0,
            Expect::MustDrop,
            "range dash class round 10: U+2212 MINUS SIGN joins the range",
        ),
        case(
            "The Ti-6Al-4V UTS ranged from 950\u{fe63}1100 MPa.",
            "Ti-6Al-4V",
            "UTS",
            1100.0,
            Expect::MustDrop,
            "range dash class round 10: U+FE63 SMALL HYPHEN-MINUS joins the range",
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
        // Dash-class pins (round 9): the sign flip must drop for EVERY
        // glyph of MINUS_CAPABLE_DASHES, not a hand-picked trio. The
        // true negatives under these glyphs still drop — none is a
        // needle glyph; that recall-loss class is recorded by the
        // KNOWN U+2013/U+2014 entries.
        case(
            "The residual stress in Ti-6Al-4V was \u{2014}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            350.0,
            Expect::MustDrop,
            "dash class round 9: the former KNOWN U+2014 twin now drops — \
             marker removed at the tripwire's demand",
        ),
        case(
            "The residual stress in Ti-6Al-4V was \u{2010}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            350.0,
            Expect::MustDrop,
            "dash class: U+2010 HYPHEN, ordinary PDF-extractor output",
        ),
        case(
            "The residual stress in Ti-6Al-4V was \u{2011}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            350.0,
            Expect::MustDrop,
            "dash class: U+2011 NON-BREAKING HYPHEN, ordinary PDF-extractor output",
        ),
        case(
            "The residual stress in Ti-6Al-4V was \u{2012}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            350.0,
            Expect::MustDrop,
            "dash class: U+2012, literally named FIGURE DASH",
        ),
        case(
            "The residual stress in Ti-6Al-4V was \u{2015}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            350.0,
            Expect::MustDrop,
            "dash class: U+2015 HORIZONTAL BAR",
        ),
        case(
            "The residual stress in Ti-6Al-4V was \u{fe63}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            350.0,
            Expect::MustDrop,
            "dash class: U+FE63 SMALL HYPHEN-MINUS",
        ),
        // ---------------- MUST_DROP: the separator shapes (round 11) --
        // Round 10 made U+2013/U+2014 needle glyphs for negatives on
        // the argument "a range dash has a digit before it, a minus
        // does not". Measured round 11: that separates minus from
        // RANGE but not from SEPARATOR — a label/value separator also
        // has no digit before the dash, and all three reviewer shapes
        // stamped a compressive value from a tensile source while
        // dropping the correct positive. Round 11 reverted them to
        // non-sign glyphs; these rows pin the closure so the question
        // never reopens.
        case(
            "Ti-6Al-4V UTS \u{2013}950 MPa (longitudinal)",
            "Ti-6Al-4V",
            "UTS",
            -950.0,
            Expect::MustDrop,
            "round 11: the dash SEPARATES the label UTS from its value — \
             the source value is +950, so -950 is a fabrication; U+2013 is \
             no sign glyph and the claim has no needle",
        ),
        case(
            "\u{2013}950 MPa was recorded for Ti-6Al-4V.",
            "Ti-6Al-4V",
            "UTS",
            -950.0,
            Expect::MustDrop,
            "round 11: the same separator shape at line start — locally \
             indistinguishable from a genuine minus, so it shares the \
             drop; the recall loss is recorded by the KNOWN rows below",
        ),
        case(
            "The Ti-6Al-4V result \u{2014}950 MPa\u{2014} matched the target.",
            "Ti-6Al-4V",
            "UTS",
            -950.0,
            Expect::MustDrop,
            "round 11: the em-dash separator twin — a tensile result \
             bracketed by em dashes must not stamp a compressive claim",
        ),
        // ---------------- round 11: signed needles vs dash ranges ------
        // Only '-' and U+2212 are sign glyphs — round 11 reverted
        // round 10's U+2013/U+2014 (the separator shapes fabricated
        // negatives; see the section above). These rows pin a signed
        // needle against range and reference shapes under the glyphs
        // that survive.
        case(
            "The Ti-6Al-4V UTS ranged from 300\u{2212}950 MPa.",
            "Ti-6Al-4V",
            "UTS",
            -950.0,
            Expect::MustDrop,
            "round 11 rewrite of the round-10 VACUOUS row: the old \
             \u{2013}1100 shape dropped NoSpan because the -1100 needle is \
             never constructed un-grouped (|value| >= 1000), so the row \
             proved nothing. The signed \u{2212}950 needle IS constructed \
             (U+2212 stays a sign glyph) and the boundary clause refuses \
             it — the range's left digit 300 glues before the dash",
        ),
        case(
            "The Ti-6Al-4V data are listed in Refs. 25-27.",
            "Ti-6Al-4V",
            "UTS",
            -27.0,
            Expect::MustDrop,
            "round 11: a signed needle must not match a reference range — \
             pinned under the ASCII hyphen now that U+2013 is no sign \
             glyph; 25 before the dash glues and the boundary clause \
             refuses",
        ),
        // ---------------- MUST_DROP: refused U+2212 needles ----------
        // The round-5 UTF-8 advance panic: a rejected U+2212 occurrence
        // must advance by the minus's 3 bytes, not one byte, or the next
        // slice starts inside the char and the whole ingest panics. All
        // other corpus U+2212 cases are ACCEPTED at first occurrence, so
        // the advance line is never reached there — these needles are
        // REFUSED, which is what exercises it.
        case(
            "Ti-6Al-4V at \u{2212}950x zoom had UTS.",
            "Ti-6Al-4V",
            "UTS",
            -950.0,
            Expect::MustDrop,
            "round-5 regression, missed until round 9: the refused \u{2212}950x \
             needle must advance by char; a byte advance panics the scan",
        ),
        case(
            "Ti-6Al-4V stress \u{2212}950\u{2013}1100 MPa.",
            "Ti-6Al-4V",
            "stress",
            -950.0,
            Expect::MustDrop,
            "the range twin: a U+2212 needle refused as a range endpoint \
             must also advance by char, not byte",
        ),
        case(
            "Ti-6Al-4V stress \u{2212}950\u{2013}1100 MPa.",
            "Ti-6Al-4V",
            "stress",
            1100.0,
            Expect::MustDrop,
            "the high endpoint of a signed en-dash range is a range bound too",
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
            "The AlSi10Mg UTS was 300 MPa.",
            "Ti-6Al-4V",
            "UTS",
            300.0,
            Expect::MustDrop,
            "KNOWN: subject-blind attribution — the span names AlSi10Mg and \
             never Ti-6Al-4V, yet the number stamps for the claimed subject \
             through the subject-OR-object arm. Cross-subject attribution is \
             the largest live fabrication channel: a number from a paper about \
             a different alloy becomes a claim about yours",
        ),
        known(
            "The residual stress in Ti-6Al-4V was \u{2013}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            -350.0,
            Expect::MustStamp,
            "KNOWN: recall loss, round 11 REVERTED the round-10 fix — the \
             prose asserts the stress IS -350 MPa, a materials engineer \
             calls that supported; but U+2013 also SEPARATES a label from \
             its value ('UTS \u{2013}950 MPa'), the two shapes are locally \
             indistinguishable (neither has a digit before the dash), and \
             the separator reading stamped compressive from tensile, so \
             U+2013 is no sign glyph again and the true negative has no \
             needle. What the code SHOULD do but does not yet",
        ),
        known(
            "The residual stress in Ti-6Al-4V was \u{2014}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            -350.0,
            Expect::MustStamp,
            "KNOWN: the same recall loss for the U+2014 typesetting — \
             round 11 reverted the round-10 stamp after the em-dash \
             separator shape ('result \u{2014}950 MPa\u{2014}') stamped a \
             compressive value from a tensile source",
        ),
        // H1's space requirement cost (round 9): a label number with a
        // GLUED unit drops — the exemption demands the space, the
        // boundary check cannot redeem what the Label guard refuses
        // afterwards. Thirteen forms, recorded so the cost is visible;
        // round 10 removed cross-section 10mm — a position, not a
        // property, so its drop is correct, not a cost (the twins sit
        // with the label/sample/run family above) — and added the eight
        // glued losses the round-9 word list caused, one per new label
        // word; round 11 removed batch 25kg — the mass of one powder lot
        // is extensive, not a property, so its drop is correct, not a
        // cost (the twin sits with the label/sample/run family above) —
        // leaving seven: coupon 3mm, specimen 5mm, panel 2mm, test
        // 950MPa, scan step 50um, condition 980C, trial 30min.
        known(
            "Each Ti-6Al-4V sample 3mm thick was ground and polished.",
            "Ti-6Al-4V",
            "thickness",
            3.0,
            Expect::MustStamp,
            "KNOWN: glued recall lost to H1's space requirement — sample 3mm",
        ),
        known(
            "Each Inconel 718 run 30min was quenched.",
            "Inconel 718",
            "duration",
            30.0,
            Expect::MustStamp,
            "KNOWN: glued recall lost to H1's space requirement — run 30min",
        ),
        known(
            "The Ti-6Al-4V sample 980\u{b0}C cycle was logged.",
            "Ti-6Al-4V",
            "temperature",
            980.0,
            Expect::MustStamp,
            "KNOWN: glued recall lost to H1's space requirement — sample 980\u{b0}C",
        ),
        known(
            "The AlSi10Mg samples 5mm thick were sectioned.",
            "AlSi10Mg",
            "thickness",
            5.0,
            Expect::MustStamp,
            "KNOWN: glued recall lost to H1's space requirement — samples 5mm",
        ),
        known(
            "Each AlSi10Mg sample 30um layer was imaged.",
            "AlSi10Mg",
            "layer_thickness",
            30.0,
            Expect::MustStamp,
            "KNOWN: glued recall lost to H1's space requirement — sample 30um",
        ),
        known(
            "One CoCrFeNi sample 5wt% Cr was analysed.",
            "CoCrFeNi",
            "content",
            5.0,
            Expect::MustStamp,
            "KNOWN: glued recall lost to H1's space requirement — sample 5wt%",
        ),
        known(
            "Each Ti-6Al-4V coupon 3mm thick was weighed.",
            "Ti-6Al-4V",
            "thickness",
            3.0,
            Expect::MustStamp,
            "KNOWN: glued recall lost to H1's space requirement — coupon 3mm, \
             one of the eight losses the round-9 word list caused",
        ),
        known(
            "Each Ti-6Al-4V specimen 5mm thick was sectioned.",
            "Ti-6Al-4V",
            "thickness",
            5.0,
            Expect::MustStamp,
            "KNOWN: glued recall lost to H1's space requirement — specimen 5mm",
        ),
        known(
            "The AlSi10Mg panel 2mm thick was cut.",
            "AlSi10Mg",
            "thickness",
            2.0,
            Expect::MustStamp,
            "KNOWN: glued recall lost to H1's space requirement — panel 2mm",
        ),
        known(
            "The Ti-6Al-4V test 950MPa peak UTS was logged.",
            "Ti-6Al-4V",
            "UTS",
            950.0,
            Expect::MustStamp,
            "KNOWN: glued recall lost to H1's space requirement — test 950MPa",
        ),
        known(
            "The AlSi10Mg scan step 50um was imaged.",
            "AlSi10Mg",
            "scan_step_size",
            50.0,
            Expect::MustStamp,
            "KNOWN: glued recall lost to H1's space requirement — scan step 50um",
        ),
        known(
            "The Ti-6Al-4V condition 980C soak was logged.",
            "Ti-6Al-4V",
            "temperature",
            980.0,
            Expect::MustStamp,
            "KNOWN: glued recall lost to H1's space requirement — condition 980C",
        ),
        known(
            "The Inconel 718 trial 30min ran to completion.",
            "Inconel 718",
            "duration",
            30.0,
            Expect::MustStamp,
            "KNOWN: glued recall lost to H1's space requirement — trial 30min",
        ),
        // ---------------- KNOWN failures, round-10 structural gaps ----
        // Gaps earlier rounds documented in PROSE but never pinned. A
        // doc comment cannot tell anyone when a gap closes or widens;
        // the KNOWN mechanism exists exactly for that and was used for
        // fourteen other items.
        known(
            "The Ti-6Al-4V UTS was 950 MPa.",
            "Ti-6Al-4V",
            "density",
            950.0,
            Expect::MustDrop,
            "KNOWN: object-blind attribution — the twin of the subject-blind \
             KNOWN row. The span names UTS, never density, yet the number \
             stamps for ANY claimed object because support checks the \
             presence of subject OR object, not which property the number \
             belongs to",
        ),
        known(
            "The Ti-6Al-4V UTS was 950 MPa and the yield strength 880 MPa.",
            "Ti-6Al-4V",
            "yield_strength",
            950.0,
            Expect::MustDrop,
            "KNOWN: predicate binding, the largest remaining structural gap \
             in the claims.rs module doc, recorded in prose there since \
             round 9 but never pinned: THE VALUE IS NEVER TIED TO THE \
             PREDICATE. 950 belongs to UTS in this sentence, yet the \
             yield_strength claim stamps — and the correct 880 claim stamps \
             indistinguishably beside it",
        ),
        case(
            "Ti-6Al-4V | 950 | 300",
            "Ti-6Al-4V",
            "UTS",
            950.0,
            Expect::MustStamp,
            "round 11: the subject IN the span and the value of its own \
             column — the control half of the transposed-table pair below",
        ),
        known(
            "Ti-6Al-4V | 950 | 300",
            "Ti-6Al-4V",
            "UTS",
            300.0,
            Expect::MustDrop,
            "KNOWN: transposed tables — round 11 put the SUBJECT IN THE \
             SPAN: the old prose 'UTS (MPa) | 950 | 300' never named \
             Ti-6Al-4V, so that row exercised the subject-blind OR-arm \
             (already pinned above), not column binding — the day \
             subject-blindness closes, that row would go green and the \
             tripwire would strip the marker while this gap stayed wide \
             open. A pin that dies when a DIFFERENT gap closes is not a \
             pin. The row carries two columns and the engine cannot tell \
             them apart: 300 belongs to another column yet stamps as UTS — \
             row-span support is co-occurrence, not column binding",
        ),
        // ---------------- KNOWN failures, range-guard gaps (round 11) -
        // Round 9 recorded "spaced ranges, ASCII-typed ranges and
        // negative ranges still stamp their endpoints"; round 10
        // recorded only "spaced ranges" in the round that widened the
        // negative shape from 2 glyphs to 4. Round 11 restored the
        // record and pinned it. The guard sees digit/dash/digit
        // adjacency only: a space defeats it, and the second dash of a
        // double-dash is followed by a sign, not a digit.
        known(
            "The Ti-6Al-4V UTS ranged from -950--400 MPa.",
            "Ti-6Al-4V",
            "UTS",
            -950.0,
            Expect::MustDrop,
            "KNOWN: the low endpoint of a negative dash-range stamps as a \
             point value — after -950 comes a dash and then ANOTHER dash, \
             not a digit, so the run is never seen; the high endpoint \
             (-400) drops Range because its before-arm works",
        ),
        known(
            "The Ti-6Al-4V UTS ranged from -950 to -400 MPa.",
            "Ti-6Al-4V",
            "UTS",
            -950.0,
            Expect::MustDrop,
            "KNOWN: the word-form negative range stamps its LOW endpoint — \
             'to' is not a dash, the adjacency guard never fires",
        ),
        known(
            "The Ti-6Al-4V UTS ranged from -950 to -400 MPa.",
            "Ti-6Al-4V",
            "UTS",
            -400.0,
            Expect::MustDrop,
            "KNOWN: the word-form negative range stamps its HIGH endpoint \
             too — both bounds of a range asserted as point values",
        ),
        known(
            "The Ti-6Al-4V UTS ranged from \u{2212}950 to \u{2212}400 MPa.",
            "Ti-6Al-4V",
            "UTS",
            -950.0,
            Expect::MustDrop,
            "KNOWN: the U+2212 twin of the negative word-range stamps too — \
             both surviving sign glyphs carry the gap; round 10 briefly \
             widened it to four glyphs, round 11 pinned the two",
        ),
        known(
            "The Ti-6Al-4V UTS was 950 \u{2013} 1100 MPa.",
            "Ti-6Al-4V",
            "UTS",
            950.0,
            Expect::MustDrop,
            "KNOWN: the SPACED-DASH range stamps its low endpoint — journal \
             typesetting and pdf-extract both commonly emit this shape; the \
             guard needs the digits glued to the dash",
        ),
        known(
            "The Ti-6Al-4V UTS was 950 \u{2013} 1100 MPa.",
            "Ti-6Al-4V",
            "UTS",
            1100.0,
            Expect::MustDrop,
            "KNOWN: the spaced-dash range stamps its high endpoint too",
        ),
        known(
            "The Ti-6Al-4V UTS was 950 - 1100 MPa.",
            "Ti-6Al-4V",
            "UTS",
            950.0,
            Expect::MustDrop,
            "KNOWN: the ASCII spaced-dash twin stamps — the gap is the \
             spaces, not the glyph",
        ),
        known(
            "The Ti-6Al-4V microstructures are shown in 4a and 4b.",
            "Ti-6Al-4V",
            "UTS",
            4.0,
            Expect::MustDrop,
            "KNOWN: sub-panel letters without the label word in the span — \
             'Figure' sits in an earlier sentence, the label guard never \
             sees it, and the glued 'a' redeems 4 through unit_initial",
        ),
        known(
            "Specimens 3 and 4 of Ti-6Al-4V were tested.",
            "Ti-6Al-4V",
            "UTS",
            4.0,
            Expect::MustDrop,
            "KNOWN: the plural leak at the conjunction tail — LABEL_WORDS \
             carries singulars only, 'specimens' is not a label word, so \
             the walk lands on a non-label head and 4 stamps. claims.rs \
             argues against adding the WORD unpinned; that is not an \
             argument against pinning the gap, and a KNOWN row cannot \
             itself be a cannot-fail item — the tripwire fires the day the \
             plural closes",
        ),
        known(
            "Specimens 3 and 4 of Ti-6Al-4V were tested.",
            "Ti-6Al-4V",
            "UTS",
            3.0,
            Expect::MustDrop,
            "KNOWN: the same plural leak at the list head — 3 sits directly \
             after 'specimens', no conjunction walk involved",
        ),
        known(
            "The Ti-6Al-4V powder came from U-235 stock.",
            "Ti-6Al-4V",
            "UTS",
            235.0,
            Expect::MustDrop,
            "KNOWN: joins_compound's surviving effect after the range guard \
             took digit/dash/digit — LETTER-dash-digit compounds still pass \
             the boundary and stamp. U-235 is a mass-number designator, not \
             a measurement. This row is deliberately joins_compound's only \
             pin: a green stamp-pin here would certify a fabrication",
        ),
        // ---------------- KNOWN failures, numeric decorations (r11) ---
        // RECORDED, NOT FIXED, round 11. The plus-minus sign is the
        // commonest numeric decoration in materials papers — commoner
        // than any dash glyph — and grep for it across claims.rs and
        // the corpus returned ZERO: the engine stamps the TOLERANCE as
        // the value. Digit-dash-LETTER is likewise unguarded: the
        // U-235 row covers only the mirror (letter-dash-digit) shape.
        known(
            "The Ti-6Al-4V UTS was 950 +/- 30 MPa.",
            "Ti-6Al-4V",
            "UTS",
            30.0,
            Expect::MustDrop,
            "KNOWN: 30 is the UNCERTAINTY, not the value — '950 +/- 30 MPa' \
             stamps the tolerance as Ti-6Al-4V UTS = 30 MPa, an uncertainty \
             figure promoted to a property",
        ),
        known(
            "The Ti-6Al-4V UTS was 950 \u{b1} 30 MPa.",
            "Ti-6Al-4V",
            "UTS",
            30.0,
            Expect::MustDrop,
            "KNOWN: the U+00B1 PLUS-MINUS SIGN twin stamps the tolerance too \
             — the glyph appears nowhere in claims.rs",
        ),
        case(
            "The Ti-6Al-4V UTS was 950 +/- 30 MPa.",
            "Ti-6Al-4V",
            "UTS",
            950.0,
            Expect::MustStamp,
            "round 11: the value itself still stamps beside its tolerance — \
             the control half of the two KNOWN rows above",
        ),
        known(
            "The Ti-6Al-4V 3-point bend strength was measured.",
            "Ti-6Al-4V",
            "UTS",
            3.0,
            Expect::MustDrop,
            "KNOWN: digit-dash-LETTER — '3-point' stamps 3, a method \
             descriptor read as a measurement",
        ),
        known(
            "The Ti-6Al-4V underwent 2-step ageing.",
            "Ti-6Al-4V",
            "ageing_steps",
            2.0,
            Expect::MustDrop,
            "KNOWN: '2-step' stamps 2 — a process count, not a property of \
             the alloy",
        ),
        known(
            "The Ti-6Al-4V was cleaned in 2-propanol.",
            "Ti-6Al-4V",
            "cleaning",
            2.0,
            Expect::MustDrop,
            "KNOWN: '2-propanol' stamps 2 — a chemical locant, not a \
             property",
        ),
        known(
            "The Ti-6Al-4V was dissolved in N-methyl-2-pyrrolidone.",
            "Ti-6Al-4V",
            "solvent",
            2.0,
            Expect::MustDrop,
            "KNOWN: 'N-methyl-2-pyrrolidone' stamps 2 — a locant inside a \
             solvent name",
        ),
        // ---------------- KNOWN failures, the label-word horizon ------
        // Measured round 10: 100 of 100 curated AM-vocabulary heads stamp
        // on "X 3 of Ti-6Al-4V was examined." — including Layer, Track,
        // Build, Heat and Lot, core LPBF/metallurgy specimen vocabulary,
        // likelier in an AM paper than Inset 2. A word list CANNOT
        // converge: every paper coins labels the list does not carry,
        // and every word added costs glued-recall drops (the thirteen
        // H1 rows above are the bill for twenty-one words — round 8's
        // sample/run plus round 9's nineteen; the claims.rs ledger has
        // the split right, this comment said nineteen and fourteen,
        // drifting from both). The fix is a RULE — a label-like head is
        // a noun immediately before a bare integer with no unit after
        // it — not another word. Recorded round 11: the rule's form is
        // NOT yet shippable — it must still SURVIVE the table-row pin
        // ("Ti-6Al-4V 950" MustStamp: the word before the bare integer
        // there is the subject itself with no unit after, yet 950
        // stamps) AND close the 25kg leak (batch 25kg MustDrop) before
        // it may replace the word list. Recorded, deliberately not
        // implemented, round 10.
        known(
            "Layer 3 of Ti-6Al-4V was examined.",
            "Ti-6Al-4V",
            "UTS",
            3.0,
            Expect::MustDrop,
            "KNOWN: the label-word horizon — Layer is likelier in an AM paper \
             than Inset 2, and it stamps",
        ),
        known(
            "Track 3 of Ti-6Al-4V was examined.",
            "Ti-6Al-4V",
            "UTS",
            3.0,
            Expect::MustDrop,
            "KNOWN: the label-word horizon — Track stamps",
        ),
        known(
            "Build 3 of Ti-6Al-4V was examined.",
            "Ti-6Al-4V",
            "UTS",
            3.0,
            Expect::MustDrop,
            "KNOWN: the label-word horizon — Build stamps",
        ),
        known(
            "Heat 3 of Ti-6Al-4V was examined.",
            "Ti-6Al-4V",
            "UTS",
            3.0,
            Expect::MustDrop,
            "KNOWN: the label-word horizon — Heat stamps",
        ),
        known(
            "Lot 3 of Ti-6Al-4V was examined.",
            "Ti-6Al-4V",
            "UTS",
            3.0,
            Expect::MustDrop,
            "KNOWN: the label-word horizon — Lot stamps",
        ),
    ]
}

/// Round-3 tautological containment lives in `validate_and_stamp` ->
/// `quote_in_block`, which the main tuple cannot express: it needs an
/// `ExtractedClaim` carrying an LLM-supplied quote.
struct ValidationCase {
    subject: &'static str,
    object: &'static str,
    value: Option<f64>,
    unit: Option<&'static str>,
    quote: Option<&'static str>,
    block: &'static str,
    expect: Expect,
    reason: &'static str,
    /// KNOWN failure of the current code; see the module docs.
    known: bool,
}

fn validation_claim(case: &ValidationCase) -> ExtractedClaim {
    ExtractedClaim {
        subject: case.subject.to_string(),
        predicate: "has_value".to_string(),
        object: case.object.to_string(),
        value: case.value,
        unit: case.unit.map(str::to_string),
        conditions: Vec::new(),
        confidence: None,
        kind: None,
        evidence_class: EVIDENCE_RESEARCH.to_string(),
        provenance: ClaimProvenance {
            document_id: "10.0000/corpus".to_string(),
            document_url: String::new(),
            source: "corpus".to_string(),
            locator: Locator {
                kind: BlockKind::Body,
                section_path: Vec::new(),
                label: None,
                char_offset: 0,
            },
            quote: case.quote.map(str::to_string),
        },
    }
}

fn validation_corpus() -> Vec<ValidationCase> {
    const BLOCK: &str = "We measured CoCrFeNi. Its thermal conductivity is 11.5 W/(m K) \
                         at room temperature.";
    let fact = |quote: Option<&'static str>, expect: Expect, reason: &'static str| ValidationCase {
        subject: "CoCrFeNi",
        object: "thermal_conductivity",
        value: Some(11.5),
        unit: Some("W/(m K)"),
        quote,
        block: BLOCK,
        expect,
        reason,
        known: false,
    };
    vec![
        fact(
            Some("thermal conductivity is 11.5 W/(m K)"),
            Expect::MustStamp,
            "a verbatim contained quote stamps",
        ),
        fact(
            Some("  Thermal   CONDUCTIVITY is 11.5 W/(m K) "),
            Expect::MustStamp,
            "containment normalizes case and whitespace, nothing else",
        ),
        fact(
            Some("thermal conductivity is 12.5 W/(m K)"),
            Expect::MustDrop,
            "a quote asserting a number the block never contains is dropped",
        ),
        fact(
            None,
            Expect::MustDrop,
            "a claim with no quote has nothing tying it to its block",
        ),
        fact(
            Some(""),
            Expect::MustDrop,
            "an EMPTY quote must not pass containment — the tautology guard: \
             a containment check that accepts the empty needle accepts \
             every claim",
        ),
        fact(
            Some(
                "We measured CoCrFeNi. Its thermal conductivity is 11.5 W/(m K) \
                 at room temperature. The yield strength was 999 MPa.",
            ),
            Expect::MustDrop,
            "containment is block-contains-quote, never the reverse: a quote \
             that parrots the block and appends a fabricated sentence drops",
        ),
        ValidationCase {
            subject: "CoCrFeNi",
            object: "thermal_conductivity",
            value: Some(11.5),
            unit: Some("QUDT:GigaPA"),
            quote: Some("thermal conductivity is 11.5 W/(m K)"),
            block: BLOCK,
            expect: Expect::MustDrop,
            reason: "KNOWN round 10: the unit is never checked — the claim says \
                     GPa, the block says W/(m K), and containment reads only \
                     text, so a wrong-unit claim stamps with a verbatim quote. \
                     Ground truth: a unit the block contradicts is not support",
            known: true,
        },
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

    let mut record = |stamped: bool, expect: Expect, known: bool, line: String| {
        let ok = match expect {
            Expect::MustStamp => stamped,
            Expect::MustDrop => !stamped,
        };
        if ok {
            if known {
                known_fixed.push(line);
            }
        } else if known {
            known_held.push(line);
        } else {
            match expect {
                Expect::MustStamp => dropped_when_must_stamp.push(line),
                Expect::MustDrop => stamped_when_must_drop.push(line),
            }
        }
    };

    for case in corpus() {
        let stamped =
            supporting_quote(case.subject, case.object, Some(case.value), case.prose).is_some();
        let line = format!(
            "  {} / {} = {} in {:?}\n    reason: {}",
            case.subject, case.object, case.value, case.prose, case.reason
        );
        record(stamped, case.expect, case.known, line);
    }

    for case in validation_corpus() {
        let stamped = validate_and_stamp(validation_claim(&case), case.block).is_ok();
        let line = format!(
            "  validation: {} / {} = {:?} quote {:?}\n    in {:?}\n    reason: {}",
            case.subject, case.object, case.value, case.quote, case.block, case.reason
        );
        record(stamped, case.expect, case.known, line);
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
