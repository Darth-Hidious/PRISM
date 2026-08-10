//! Is this recovered text trustworthy enough to extract facts from?
//!
//! Two different lies a document reader can tell, each with its own check.
//!
//! **A damaged text layer.** A PDF's embedded text is not guaranteed to be the
//! text a human sees. When a font ships a broken `ToUnicode` CMap the extractor
//! emits raw glyph indices, and page 1 of a real NASA turbine-disk report comes
//! out as `\x0f(%\x18\x1a))\x019&\x16(\x16#\x1a*\x1a()` where the page says
//! `Process/parameters`. Nothing downstream notices: the string is non-empty,
//! so the old `text.trim().is_empty()` guard passed it straight to the
//! extraction prompt. [`text_layer_damage`] is what notices.
//!
//! **A degenerate model output.** Vision models fed a repetitive image region —
//! a grid of micrographs each captioned `10 mm`, a table's rule lines — fall
//! into emitting one token sequence forever. Measured on Gemma 4 12B: a
//! micrograph tile produced `10 mm` 600 times, and a table tile produced `| |
//! | |` to the token limit. A sampling penalty does not fix it (it changes
//! *which* text repeats, not that it repeats). Feeding a loop to the fact
//! extractor would mint hundreds of identical assertions, so
//! [`is_degenerate`] drops the offending page instead.
//!
//! Both checks are cheap, exact, and deliberately not "smart": a fuzzy
//! word-likeness score was tried first and scored the genuinely-corrupt page as
//! 90% clean, because corrupt output is not misspelled words — it is bytes that
//! never form words at all.
//!
//! # What these signals do NOT catch
//!
//! State it plainly, because a quality gate that is trusted beyond its reach is
//! worse than none. The turbine-disk poster that motivated this module is read
//! by `pdf-extract` into 3548 clean, well-formed characters — and the entire
//! process-parameter table (`Electron Beam Melting`, `Pre-heat`, `Melt Scan
//! Speed`, `Powder size`) is simply ABSENT from them. The extractor dropped the
//! glyphs it could not map instead of emitting rubbish, so no signal here fires:
//! the page is not empty, not control-byte soup, and not sparse. A vision model
//! reading the same page recovers the whole table.
//!
//! That is why [`Damage`] is an open pair and [`DamageSignal`] is a list rather
//! than an enum: the failures worth catching next are the ones nobody has
//! enumerated yet. Detecting THIS one needs a second opinion — a second reader,
//! or the font's own `ToUnicode` coverage — not another threshold on the text.
//! Until such a signal exists, a caller who knows their corpus is affected
//! should read with [`Policy::Only`]`("vision")` rather than trusting
//! escalation to notice.
//!
//! [`Policy::Only`]: crate::document::Policy::Only

/// A reason to read a page another way.
///
/// Deliberately NOT an enum of known failures. Every closed taxonomy of "ways
/// a PDF lies" is a list of the failures whoever wrote it had already seen,
/// and the case that motivated this module escapes exactly that: a page with
/// 3548 perfectly clean characters that had silently dropped an entire
/// process-parameter table. It is not empty, not control-byte soup, and not
/// sparse — it passes every named filter, and a vision model reading the same
/// page recovers the whole table.
///
/// So a damage report is an open pair — which signal fired, and what it saw —
/// and the SIGNALS are a list callers can extend ([`DamageSignal`]). Adding a
/// detector for a failure nobody has met yet costs one closure, not a new
/// variant plus every `match` that has to learn about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Damage {
    /// Which signal fired, e.g. `"no-text"`. Stable enough to group by.
    pub signal: String,
    /// What it saw, in a sentence a user can act on.
    pub detail: String,
}

impl Damage {
    pub fn new(signal: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            signal: signal.into(),
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for Damage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

/// One reason a page's text might not be trustworthy.
///
/// The escalation path runs every registered signal and escalates on the first
/// that fires, so "detect a new kind of broken document" never means editing
/// the escalation logic. The built-ins are only the ones that have been
/// MEASURED against real corpora; they are a floor, not a claim of coverage.
pub type DamageSignal = fn(text: &str, policy: &DamagePolicy) -> Option<Damage>;

/// The measured built-ins, cheapest and most certain first.
///
/// Ordering matters only for which reason gets reported — any hit escalates.
pub const BUILTIN_SIGNALS: &[DamageSignal] = &[no_text, control_characters, sparse_text];

/// Nothing extractable at all — the classic scanned page. Zero-threshold and
/// corpus-independent: there is simply nothing to read.
pub fn no_text(text: &str, _policy: &DamagePolicy) -> Option<Damage> {
    text.chars()
        .all(char::is_whitespace)
        .then(|| Damage::new("no-text", "no extractable text (scanned page?)"))
}

/// C0 control characters in the body — the signature of a broken font CMap
/// emitting glyph indices as raw bytes.
pub fn control_characters(text: &str, policy: &DamagePolicy) -> Option<Damage> {
    let non_space: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    if non_space.is_empty() {
        return None;
    }
    let controls = non_space.iter().filter(|c| (**c as u32) < 0x20).count();
    let share = controls as f64 / non_space.len() as f64;
    (share > policy.control_char_limit).then(|| {
        Damage::new(
            "control-characters",
            format!(
                "{:.1}% of characters are control bytes (broken font encoding)",
                share * 100.0
            ),
        )
    })
}

/// Text extracted, but so little that the page is effectively figures —
/// either a genuine full-page figure or an extractor that silently dropped
/// the glyphs it could not map. Both want a second read.
pub fn sparse_text(text: &str, policy: &DamagePolicy) -> Option<Damage> {
    let chars = text.chars().filter(|c| !c.is_whitespace()).count();
    (chars > 0 && chars < policy.sparse_char_floor).then(|| {
        Damage::new(
            "sparse",
            format!("only {chars} characters recovered (figure page?)"),
        )
    })
}

/// Where the lines are drawn.
///
/// These are thresholds, and a threshold is a claim about a corpus, not a
/// fact about documents. The defaults below were measured on aerospace
/// conference posters and NASA technical memoranda; a corpus of datasheets,
/// theses, or patent filings has different text density and belongs to a
/// different set of numbers. So they are DECLARED and overridable rather than
/// compiled in — the same reason the ontology, the connectors, and the
/// sources are adapters: PRISM must not have one corpus's habits baked into
/// its logic.
///
/// A caller that does not want to reason about thresholds at all can bypass
/// them entirely with [`Policy::Only`], which reads with a chosen adapter and
/// never consults these numbers.
///
/// [`Policy::Only`]: crate::document::Policy::Only
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DamagePolicy {
    /// Share of non-whitespace characters that must be C0 controls before the
    /// page is called broken. Real prose contains none, so anything above a
    /// rounding error is decisive — this one is nearly corpus-independent.
    pub control_char_limit: f64,
    /// Below this many characters a page is treated as figures rather than
    /// text. The most corpus-dependent number here: a poster page and a
    /// datasheet page disagree about what "sparse" means.
    pub sparse_char_floor: usize,
    /// Share of non-blank lines that may be the SAME line before model output
    /// is judged a repetition loop rather than a transcription.
    pub repeat_share_limit: f64,
    /// Minimum lines before the repeat check applies, so a terse honest
    /// answer is never mistaken for a loop.
    pub repeat_min_lines: usize,
}

impl Default for DamagePolicy {
    fn default() -> Self {
        Self {
            control_char_limit: 0.02,
            sparse_char_floor: 120,
            repeat_share_limit: 0.5,
            repeat_min_lines: 8,
        }
    }
}

/// Assess one page's recovered text against the [`BUILTIN_SIGNALS`] plus any
/// the policy adds. `None` means nothing fired — which is NOT a guarantee the
/// page is sound, only that no signal we have recognised it.
pub fn text_layer_damage(text: &str, policy: &DamagePolicy) -> Option<Damage> {
    text_layer_damage_with(text, policy, BUILTIN_SIGNALS)
}

/// Assess against an explicit signal list. The extension point: a corpus that
/// knows its own failure modes supplies them here rather than waiting for
/// PRISM to grow a variant for each one.
pub fn text_layer_damage_with(
    text: &str,
    policy: &DamagePolicy,
    signals: &[DamageSignal],
) -> Option<Damage> {
    signals.iter().find_map(|signal| signal(text, policy))
}

/// Whether model output is a repetition loop rather than a transcription.
///
/// Deliberately measured on DISTINCT lines rather than a compression ratio:
/// the observed failures repeat one short line (`10 mm`) or one short token
/// run (`| |`) to the output limit, which a line-frequency check catches
/// exactly and cheaply.
pub fn is_degenerate(text: &str, policy: &DamagePolicy) -> bool {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() < policy.repeat_min_lines {
        return false;
    }
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for line in &lines {
        *counts.entry(*line).or_default() += 1;
    }
    let most = counts.values().copied().max().unwrap_or(0);
    most as f64 / lines.len() as f64 > policy.repeat_share_limit
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measured defaults, used by every test that is not ABOUT thresholds.
    fn policy() -> DamagePolicy {
        DamagePolicy::default()
    }

    /// The exact bytes `pdftotext` produced for page 1 of the NASA
    /// turbine-disk report, where the page reads `Process/parameters`. This
    /// is the case the old `is_empty()` guard waved through.
    const REAL_BROKEN_CMAP: &str = "\u{0f}(%\u{18}\u{1a}))\u{01}9&\u{16}(\u{16}#\u{1a}*\u{1a}()\u{01} \u{06}\"\u{1a}\
         \u{18}*(%$\u{01}\u{03}\u{1a}\u{16}#\u{01} \u{1a}\"+$\u{1e}\u{01}<\u{06}\u{03} =";

    #[test]
    fn the_real_broken_cmap_page_is_caught() {
        let damage = text_layer_damage(REAL_BROKEN_CMAP, &policy())
            .expect("broken CMap text must be caught");
        assert_eq!(damage.signal, "control-characters");
        assert!(damage.detail.contains("broken font encoding"), "{damage}");
    }

    /// The regression that motivated the whole check: a word-likeness score
    /// rated this page ~90% clean because corrupt output contains no words to
    /// misspell. Real prose from the SAME document must pass.
    #[test]
    fn genuine_prose_from_the_same_document_passes() {
        let real = "Motivation: Powder-bed additive manufacturing may offer geometric \
                    flexibility, microstructural control and eliminate legacy tooling. \
                    Polycrystalline Ni-based superalloy disks: powder metallurgy (PM) \
                    processing developed cost and property advantages to cast-wrought.";
        assert_eq!(text_layer_damage(real, &policy()), None);
    }

    #[test]
    fn an_empty_page_is_no_text_not_sparse() {
        for blank in ["", "   \n\t \n "] {
            let damage = text_layer_damage(blank, &policy()).expect("a blank page is damage");
            assert_eq!(damage.signal, "no-text", "{blank:?}");
        }
    }

    #[test]
    fn a_figure_only_page_is_sparse() {
        let damage =
            text_layer_damage("Figure 3. Cross-section.", &policy()).expect("must be caught");
        assert_eq!(damage.signal, "sparse");
    }

    /// A page just over the floor with no controls is sound — the floor is a
    /// boundary, and both sides of it must behave.
    #[test]
    fn the_sparse_floor_is_a_real_boundary() {
        let just_under = "a".repeat(policy().sparse_char_floor - 1);
        let just_over = "a".repeat(policy().sparse_char_floor);
        assert_eq!(
            text_layer_damage(&just_under, &policy()).map(|d| d.signal),
            Some("sparse".to_string()),
        );
        assert_eq!(text_layer_damage(&just_over, &policy()), None);
    }

    /// The point of the open design: a caller can add a signal for a failure
    /// PRISM has never seen, without a new variant and without touching the
    /// escalation logic. This is the honest answer to the case the built-ins
    /// MISS — a page with plenty of clean text that silently dropped a table.
    #[test]
    fn a_caller_can_add_a_signal_the_builtins_do_not_have() {
        // A page the built-ins all pass: long, clean, no control bytes.
        let page = "Real body text about superalloy disks. ".repeat(20);
        assert_eq!(text_layer_damage(&page, &policy()), None);

        // A corpus that knows its own tell supplies it.
        fn missing_expected_table(text: &str, _p: &DamagePolicy) -> Option<Damage> {
            (!text.contains("Table"))
                .then(|| Damage::new("no-table", "this corpus's pages always carry a table"))
        }
        let signals: &[DamageSignal] = &[missing_expected_table];
        let damage = text_layer_damage_with(&page, &policy(), signals)
            .expect("the caller's own signal must fire");
        assert_eq!(damage.signal, "no-table");
    }

    /// A single stray control byte — an artefact, not a broken font — must
    /// not condemn an otherwise readable page.
    #[test]
    fn one_stray_control_byte_does_not_condemn_a_page() {
        let mostly_fine = format!(
            "\u{01}{}",
            "Readable body text about superalloys. ".repeat(6)
        );
        assert_eq!(text_layer_damage(&mostly_fine, &policy()), None);
    }

    /// The two observed Gemma 4 failures, verbatim in shape.
    #[test]
    fn the_observed_model_loops_are_caught() {
        let micrograph_tile = "10 mm\n".repeat(60);
        assert!(is_degenerate(&micrograph_tile, &policy()));
        let table_tile = "| | | | | | | |\n".repeat(40);
        assert!(is_degenerate(&table_tile, &policy()));
    }

    /// A real transcription must survive, including one that legitimately
    /// repeats a unit across table rows.
    #[test]
    fn a_real_transcription_is_not_degenerate() {
        let real = "Fabrication of Turbine Disk Materials by Additive Manufacturing\n\
                    Chantal Sudbrack, Quincy Bean, Ken Cooper\n\
                    NASA Glenn Research Center, Cleveland, Ohio\n\
                    Motivation: Powder-bed additive manufacturing\n\
                    Polycrystalline Ni-based superalloy disks\n\
                    Powder metallurgy (PM) processing developed\n\
                    Powder cleanliness is critical to disk life\n\
                    High refractory content (i.e. Mo, Nb, Ta, W)\n\
                    Electron Beam Melting (EBM)\n\
                    Pre-heat Beam Passes ~10\n";
        assert!(!is_degenerate(real, &policy()));

        // A table repeating one unit in a minority of rows is fine.
        let table = "Alloy Yield Unit\nLSHR 1200 MPa\nIN718 1030 MPa\nRR1000 1050 MPa\n\
                     Alloy Elong Unit\nLSHR 12 %\nIN718 14 %\nRR1000 13 %\n";
        assert!(!is_degenerate(table, &policy()));
    }

    /// Short answers are never loops, however repetitive — the floor exists
    /// so a terse honest answer is not discarded.
    #[test]
    fn short_output_is_never_degenerate() {
        assert!(!is_degenerate("no text\nno text\nno text", &policy()));
        assert!(!is_degenerate("", &policy()));
    }
}
