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
//! claim + block -> stamped/dropped, same two scoreboard axes.

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
        known(
            "Ti-6Al-4V powder layers of 30-50um were deposited.",
            "Ti-6Al-4V",
            "layer_thickness",
            50.0,
            Expect::MustDrop,
            "KNOWN: a range endpoint is not a point value — the prose asserts \
             30 TO 50 um, not 50. The en-dash twin of this construct drops \
             correctly; the compound-friendly ASCII decision stamps the high \
             endpoint anyway, so the same construct has opposite ground truth \
             decided only by which dash the typesetter used. Ground truth \
             picked round 9: MustDrop; the ASCII behaviour is the recorded \
             deviation, visible here instead of certified",
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
        // ---------------- MUST_DROP: citation dash ranges (round 10) --
        // The citation walk-back trimmed only '-', U+2013 and U+2014;
        // the other six glyphs of the dash class stranded the trim on
        // the dash, the bracket was never seen, and the second citation
        // number stamped. One row per leaking glyph.
        case(
            "Ti-6Al-4V has been studied extensively [11\u{2010}13].",
            "Ti-6Al-4V",
            "UTS",
            13.0,
            Expect::MustDrop,
            "citation dash class round 10: U+2010 HYPHEN range separator",
        ),
        case(
            "Ti-6Al-4V has been studied extensively [11\u{2011}13].",
            "Ti-6Al-4V",
            "UTS",
            13.0,
            Expect::MustDrop,
            "citation dash class round 10: U+2011 NON-BREAKING HYPHEN range separator",
        ),
        case(
            "Ti-6Al-4V has been studied extensively [11\u{2012}13].",
            "Ti-6Al-4V",
            "UTS",
            13.0,
            Expect::MustDrop,
            "citation dash class round 10: U+2012 FIGURE DASH range separator",
        ),
        case(
            "Ti-6Al-4V has been studied extensively [11\u{2015}13].",
            "Ti-6Al-4V",
            "UTS",
            13.0,
            Expect::MustDrop,
            "citation dash class round 10: U+2015 HORIZONTAL BAR range separator",
        ),
        case(
            "Ti-6Al-4V has been studied extensively [11\u{2212}13].",
            "Ti-6Al-4V",
            "UTS",
            13.0,
            Expect::MustDrop,
            "citation dash class round 10: U+2212 MINUS SIGN range separator",
        ),
        case(
            "Ti-6Al-4V has been studied extensively [11\u{fe63}13].",
            "Ti-6Al-4V",
            "UTS",
            13.0,
            Expect::MustDrop,
            "citation dash class round 10: U+FE63 SMALL HYPHEN-MINUS range separator",
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
            "The Ti-6Al-4V fracture tests followed ASTM E1820-20b.",
            "Ti-6Al-4V",
            "fracture_toughness",
            20.0,
            Expect::MustDrop,
            "KNOWN: a digit-joined standard designator with a unit-initial suffix; \
             closing it needs a standard-designator guard, not dash surgery",
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
            "KNOWN: recall loss, recorded round 9 — the prose asserts the stress \
             IS -350 MPa, a materials engineer calls that claim supported; \
             U+2013 is not a needle glyph yet, so the true negative has no \
             needle and drops. What the code SHOULD do but does not yet",
        ),
        known(
            "The residual stress in Ti-6Al-4V was \u{2014}350 MPa.",
            "Ti-6Al-4V",
            "residual_stress",
            -350.0,
            Expect::MustStamp,
            "KNOWN: the same recall loss for the U+2014 typesetting of the \
             minus sign — the true negative drops for lack of a needle",
        ),
        // H1's space requirement cost (round 9): a label number with a
        // GLUED unit drops — the exemption demands the space, the
        // boundary check cannot redeem what the Label guard refuses
        // afterwards. Seven forms, recorded so the cost is visible.
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
            "A cross-section 10mm above the build plate was examined for AlSi10Mg.",
            "AlSi10Mg",
            "height",
            10.0,
            Expect::MustStamp,
            "KNOWN: glued recall lost to H1's space requirement — cross-section 10mm",
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
        record(stamped, case.expect, false, line);
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
