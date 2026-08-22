// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Table reachability against a REAL published JATS document.
//!
//! Fixture: PMC13302085 — "Theoretical Analysis of the Process Window for
//! Laser Powder-Bed Fusion for Infrared and Green Lasers Using Rosenthal
//! Approximation", *Materials* 19(12), 2026 (MDPI), CC BY 4.0, fetched
//! verbatim from Europe PMC (`/PMC13302085/fullTextXML`) on 2026-08-22. The
//! license statement travels inside the file itself.
//!
//! Why THIS document: it is the measured worst case that motivated the fix.
//! Every one of its five `<table-wrap>`s and all eight `<fig>`s live in
//! `<floats-group>` (after `</back>`) or in `<back>/<app-group>` — the
//! `<body>` holds only `<xref>` pointers. A body-only parser produced a full
//! text in which NO table and NO figure caption existed at all, so the
//! reading agent could search "Table 1" and find nothing but prose mentions.
//! MDPI journals (*Materials*, *Metals*, *Crystals*, *JMMP*) are a large
//! share of the open-access materials corpus, so this shape is common, not
//! exotic.
//!
//! Every expectation below was transcribed from the XML source by hand —
//! none was derived from parser output, so a parser that invents or drops
//! content fails here.

use prism_retrieval::fulltext::{BlockKind, parse_jats};

const REAL_JATS: &str = include_str!("fixtures/PMC13302085.nxml");

/// All five tables — the four floats-group tables and the appendix table —
/// must reach the block stream as Table blocks carrying their labels.
#[test]
fn all_five_tables_reach_the_block_stream() {
    let ft = parse_jats(REAL_JATS.as_bytes()).unwrap();
    for label in ["Table 1", "Table 2", "Table 3", "Table 4", "Table A1"] {
        assert!(
            ft.blocks.iter().any(|b| {
                b.locator.kind == BlockKind::Table && b.locator.label.as_deref() == Some(label)
            }),
            "{label} never reached the block stream"
        );
    }
}

/// The appendix table's data survives with its cell boundaries: the first
/// and last data rows of Table A1 exactly as the XML states them. The
/// Reference cell is multi-word ("Malý et al., 2022 [[53] ]") — under the
/// old space-join it fused with the Power and Speed columns beside it into
/// an unparseable run.
#[test]
fn appendix_table_rows_keep_their_cell_boundaries() {
    let ft = parse_jats(REAL_JATS.as_bytes()).unwrap();
    let a1 = ft
        .blocks
        .iter()
        .find(|b| {
            b.locator.kind == BlockKind::Table && b.locator.label.as_deref() == Some("Table A1")
        })
        .expect("Table A1 (in <back>/<app-group>) must be captured");

    let rows: Vec<&str> = a1.text.lines().collect();
    // 1 header row + 46 data rows, straight from the XML's 47 <tr> elements.
    assert_eq!(rows.len(), 47, "Table A1 must keep all 47 rows");
    assert_eq!(
        rows[0], "# | Density (%) | Classification | Reference | Power | Speed | HD | LT",
        "the header row must arrive with its column boundaries"
    );
    assert_eq!(
        rows[1], "1 | 99.9 | No LOF | Malý et al., 2022 [[53] ] | 400 | 500 | 60 | 30",
        "data row 1 must arrive with its cell boundaries"
    );
    assert_eq!(
        rows[46], "46 | 83 | LOF | Trevisan et al., 2017 [[57] ] | 195 | 400 | 80 | 30",
        "the last data row must arrive with its cell boundaries"
    );
    // The publisher-internal <object-id> must not pollute the evidence text.
    assert!(
        !a1.text.contains("materials-19-02487"),
        "object-id junk leaked into the table text"
    );
}

/// Tables are findable BY NAME in the visible text: each caption arrives as
/// "Table N. <caption>", the string a reader actually searches for. These
/// captions carry the units and conditions of the table's values.
#[test]
fn table_captions_carry_their_labels_in_the_text() {
    let ft = parse_jats(REAL_JATS.as_bytes()).unwrap();
    for heading in [
        "Table 1. Magnitude of the non-negligible term in Equation (1) for highly conductive metals.",
        "Table 2. Material constants for copper.",
        "Table 3. Absorptivity sensitivity.",
        "Table 4. Density-threshold sensitivity.",
        "Table A1. Literature Data Used for LOF Process-Window Validation.",
    ] {
        assert!(
            ft.plain_text.contains(heading),
            "missing table heading in the visible text: {heading}"
        );
    }
}

/// Figure captions live in `<floats-group>` too and were dropped with the
/// tables. They must arrive labelled, findable by "Figure N".
#[test]
fn figure_captions_reach_the_text_with_their_labels() {
    let ft = parse_jats(REAL_JATS.as_bytes()).unwrap();
    assert!(
        ft.plain_text.contains(
            "Figure 1. Illustration of the melt pool geometry and the global and local coordinates."
        ),
        "the floats-group figure caption never reached the text"
    );
    let figure_captions = ft
        .blocks
        .iter()
        .filter(|b| {
            b.locator.kind == BlockKind::Caption
                && b.locator
                    .label
                    .as_deref()
                    .is_some_and(|l| l.starts_with("Figure"))
        })
        .count();
    assert_eq!(figure_captions, 8, "the document declares eight figures");
}

/// Widening capture to `<floats-group>` and `<app-group>` must NOT widen it
/// to the bibliography: reference-list text stays out of the evidence
/// stream. "Int. J. Adv. Manuf. Technol." occurs only inside `<ref-list>`
/// in this document (verified against the source).
#[test]
fn bibliography_text_stays_excluded() {
    let ft = parse_jats(REAL_JATS.as_bytes()).unwrap();
    assert!(
        !ft.plain_text.contains("Int. J. Adv. Manuf. Technol."),
        "reference-list text leaked into the evidence stream"
    );
    // Body prose is still there — exclusion did not overreach.
    assert!(
        ft.plain_text
            .contains("Analytical models such as the Rosenthal equation"),
        "body prose went missing"
    );
}
