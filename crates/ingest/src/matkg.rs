//! Streaming bulk loader for MatKG 1.4 (Venugopal & Olivetti, Scientific
//! Data 11:217, 2024; doi:10.5281/zenodo.10144972, CC BY 4.0).
//!
//! MatKG's `SUBRELOBJ.nt` distribution is REIFIED: each `sro_rowN` node
//! carries one `hasSubject`, `hasRelationship`, `hasObject`, and `hasCount`
//! statement, and the four statements of one row are scattered across the
//! whole ~21.6M-line file. The loader recovers rows in two streaming passes
//! over the (optionally tar.gz/gz-compressed) file:
//!
//! 1. index `rowid → hasCount` (a dense `Vec<u32>` — ~22 MB for the pinned
//!    5.4M-row artifact, never the parsed triples themselves) and hash the
//!    exact input bytes;
//! 2. select rows (`min_count` filter, then top-`limit` by count, ties by
//!    row id) and collect ONLY the selected rows' fields.
//!
//! What a row means, and what it does not:
//! - The entity class is the URI path segment of `hasSubject`/`hasObject`
//!   (`CHM`, `SMT`, `PRO`, `APL`, `SPL`, `CMT`, `DSC`), resolved through the
//!   registered [`crate::ontologies::MatKgOntology`] declaration.
//! - `hasRelationship` values are type-pair-shaped strings that DISAGREE
//!   with the subject/object URI types in the majority of rows (measured:
//!   27,218 of 32,331 fully-joined rows in a 6M-line window), because the
//!   same name pair is tallied under several per-mention NER type contexts.
//!   The value is therefore never trusted as a class or relation source —
//!   every fact is stored under the one declared relationship
//!   `COOCCURS_WITH` — but the statement's PRESENCE is still required for a
//!   complete row.
//! - `hasCount` is corroboration STRENGTH, not truth and not a probability.
//!   It is (a) a load filter (`min_count`), (b) stored verbatim as the
//!   fact's `value`, and (c) log-compressed into a capped ranking
//!   confidence ([`confidence_for_count`]).
//! - The same unordered name pair appears repeatedly (mirrored directions
//!   and repeated type contexts, with genuinely differing counts). Rows are
//!   merged onto the unordered canonical name pair keeping the MAXIMUM
//!   count — never the sum, which would double-count the (measured) 78% of
//!   mirrors that are symmetric duplicates of one tally.
//!
//! Every write goes through the store's production dispatch
//! ([`ProvenanceStore::write_classified_fact_with_evidence`]) under the
//! composed tenant `local@matkg`, as evidence class Research (ORANGE:
//! literature co-occurrence is not measurement), stamped with the MatKG
//! ontology artifact, and attributed to the dataset DOI — whose per-source
//! evidence dedup is what makes re-loading idempotent.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result, bail};
use prism_provenance::{
    ClassifiedFactNodes, ClassifiedNode, EvidenceClass, LocalFact, LocalProvenance,
    OntologyClassification, ProvenanceStore, canonical_key,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::ontologies::{MATKG_ONTOLOGY_ID, Ontology, storage_tenant};

/// The stable source identity every loaded fact is attributed to. It is the
/// version-pinned dataset DOI, so the store's per-source evidence key
/// (`doi:…`) is identical across runs — that existing dedup, not a second
/// mechanism, is what makes re-loading idempotent.
pub const MATKG_SOURCE_ID: &str = "doi:10.5281/zenodo.10144972";

/// PROV-O software agent recorded for every load activity.
pub const MATKG_AGENT_ID: &str = "prism-matkg-loader";

/// The one relationship the SUBRELOBJ distribution actually carries.
pub const MATKG_PREDICATE: &str = "COOCCURS_WITH";

/// Default row budget. Loading all 5.4M rows into a laptop Turso store is a
/// deliberate choice (`limit: None`), not the default.
pub const DEFAULT_LIMIT: usize = 10_000;

/// Rows whose count is below this are skipped by default. 25 is the minimum
/// count present in the pinned MatKG 1.4 artifact, so the default filters
/// nothing silently — it exists to be raised.
pub const DEFAULT_MIN_COUNT: u32 = 25;

/// Row-id sanity ceiling. The pinned artifact's ids are dense in
/// 0..5,396,084; a file whose ids exceed this would make the dense count
/// index unreasonably large, and the loader refuses loudly instead of
/// swallowing gigabytes.
const MAX_ROW_ID: u64 = 64_000_000;

/// Load bounds. `limit: None` means "everything passing `min_count`" and is
/// the deliberate full-load switch.
#[derive(Debug, Clone, Copy)]
pub struct MatkgLoadOptions {
    pub min_count: u32,
    pub limit: Option<usize>,
}

impl Default for MatkgLoadOptions {
    fn default() -> Self {
        Self {
            min_count: DEFAULT_MIN_COUNT,
            limit: Some(DEFAULT_LIMIT),
        }
    }
}

/// Complete accounting of one load run. Every row of the input is either
/// loaded or reported in exactly one skip bucket — silent truncation is the
/// defect class this struct exists to prevent:
/// `rows_total = rows_below_min_count + rows_beyond_limit + rows_selected`
/// and `rows_selected = rows_incomplete + rows_malformed + rows_ambiguous +
/// rows_self_pair + rows_merged_duplicates + facts_written`.
#[derive(Debug, Clone, Serialize)]
pub struct MatkgLoadReport {
    /// Reified rows discovered (distinct `sro_row` ids with a `hasCount`).
    pub rows_total: u64,
    /// Skipped: count below `min_count`.
    pub rows_below_min_count: u64,
    /// Skipped: outside the top-`limit` by (count desc, row id asc).
    pub rows_beyond_limit: u64,
    /// Rows the bounds admitted.
    pub rows_selected: u64,
    /// Selected rows missing one of the four reified statements.
    pub rows_incomplete: u64,
    /// Selected rows whose fields failed to decode (unknown type code,
    /// empty or non-UTF-8 name, malformed URI).
    pub rows_malformed: u64,
    /// Rows carrying a duplicate reified statement (never guessed at).
    pub rows_ambiguous: u64,
    /// Selected rows whose subject and object canonicalise to one name.
    pub rows_self_pair: u64,
    /// Selected rows merged into an already-kept unordered name pair
    /// (mirrored directions and repeated NER type contexts; max count wins).
    pub rows_merged_duplicates: u64,
    /// Facts written (one per surviving unordered pair).
    pub facts_written: u64,
    /// Distinct label-qualified entities among the written facts.
    pub entities_written: u64,
    /// Input lines that parsed as none of the four reified statements.
    pub lines_unparsed: u64,
    /// Total input lines.
    pub lines_total: u64,
    /// Storage tenant the facts landed under (`local@matkg`).
    pub tenant: String,
    /// PROV-O activity id of this run.
    pub activity_id: String,
    /// SHA-256 of the exact input file bytes as read.
    pub data_sha256: String,
    /// SHA-256 of the MatKG ontology artifact that classified the facts.
    pub ontology_artifact_sha256: String,
    /// Ontology version IRI stamped on every assertion.
    pub ontology_version_iri: String,
    /// The CC-BY attribution the licence requires; callers must surface it.
    pub attribution: String,
}

/// The REQUIRED CC-BY 4.0 attribution, kept in one place so every surface
/// (CLI, report, dataset node) states the same thing.
pub const MATKG_ATTRIBUTION: &str = "Contains data from MatKG 1.4 by Vineeth Venugopal and Elsa \
     Olivetti, used under CC BY 4.0. Dataset: https://doi.org/10.5281/zenodo.10144972. Citation: \
     Venugopal, V. & Olivetti, E. MatKG: An autonomously generated knowledge graph in Material \
     Science. Scientific Data 11, 217 (2024).";

/// Map `hasCount` to a bounded ranking confidence.
///
/// Deliberately NOT a probability: co-occurrence count is corroboration
/// strength. The map is a log compression — order-of-magnitude scaling is
/// what a five-orders-of-magnitude count range (25 ..= 206,811 in the pinned
/// artifact) supports — hard-capped at 0.80 so no amount of literature
/// agreement can approach execution-grade certainty (the store treats
/// confidence 1.0 as "asserted with full confidence", which co-occurrence
/// never earns). Deterministic per count, independent of the rest of the
/// file, so identical facts get identical confidence whatever the bounds of
/// the run that loaded them.
#[must_use]
pub fn confidence_for_count(count: u32) -> f64 {
    let count = f64::from(count.max(1));
    (count.log10() / 6.0).min(0.80)
}

/// MatKG URI type segment → declared extraction label.
fn code_to_label(code: &str) -> Option<&'static str> {
    Some(match code {
        "APL" => "Application",
        "CMT" => "CharacterisationMethod",
        "CHM" => "Chemical",
        "DSC" => "Descriptor",
        "PRO" => "Property",
        "SPL" => "SymmetryPhaseLabel",
        "SMT" => "SynthesisMethod",
        _ => return None,
    })
}

/// Strict percent-decoding: `%XX` escapes only, decoded exactly once, and
/// the result must be UTF-8. Malformed escapes and non-UTF-8 results are
/// `None` — the row is then reported as malformed, never guessed at.
fn percent_decode_strict(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = char::from(*bytes.get(i + 1)?).to_digit(16)?;
            let lo = char::from(*bytes.get(i + 2)?).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// One parsed reified statement.
enum Statement<'a> {
    Subject(u64, &'a str),
    Object(u64, &'a str),
    Relationship(u64),
    Count(u64, u32),
}

enum ParsedLine<'a> {
    Blank,
    Unparsed,
    Stmt(Statement<'a>),
}

const ROW_PREFIX: &str = "<http://example.com/sro_row";
const PRED_PREFIX: &str = "> <http://example.com/";
const URI_PREFIX: &str = "<http://example.com/";

/// Parse one N-Triples line of the SUBRELOBJ shape. Anything else —
/// including other well-formed N-Triples — is `Unparsed` and counted, not
/// skipped silently.
fn parse_line(line: &str) -> ParsedLine<'_> {
    let line = line.trim_end();
    if line.is_empty() {
        return ParsedLine::Blank;
    }
    let Some(rest) = line.strip_prefix(ROW_PREFIX) else {
        return ParsedLine::Unparsed;
    };
    let digits_end = rest.find('>').unwrap_or(0);
    let Ok(rowid) = rest[..digits_end].parse::<u64>() else {
        return ParsedLine::Unparsed;
    };
    let rest = &rest[digits_end..];
    let Some(rest) = rest.strip_prefix(PRED_PREFIX) else {
        return ParsedLine::Unparsed;
    };
    let Some(pred_end) = rest.find('>') else {
        return ParsedLine::Unparsed;
    };
    let (pred, rest) = rest.split_at(pred_end);
    let Some(object) = rest
        .strip_prefix("> ")
        .and_then(|r| r.strip_suffix(" ."))
        .map(str::trim)
    else {
        return ParsedLine::Unparsed;
    };
    match pred {
        "hasSubject" | "hasObject" => {
            let Some(path) = object
                .strip_prefix(URI_PREFIX)
                .and_then(|o| o.strip_suffix('>'))
            else {
                return ParsedLine::Unparsed;
            };
            ParsedLine::Stmt(if pred == "hasSubject" {
                Statement::Subject(rowid, path)
            } else {
                Statement::Object(rowid, path)
            })
        }
        "hasRelationship" => {
            if !object.starts_with(URI_PREFIX) {
                return ParsedLine::Unparsed;
            }
            ParsedLine::Stmt(Statement::Relationship(rowid))
        }
        "hasCount" => {
            // "117"^^<http://www.w3.org/2001/XMLSchema#integer>
            let Some(rest) = object.strip_prefix('"') else {
                return ParsedLine::Unparsed;
            };
            let Some(quote) = rest.find('"') else {
                return ParsedLine::Unparsed;
            };
            let Ok(count) = rest[..quote].parse::<u32>() else {
                return ParsedLine::Unparsed;
            };
            ParsedLine::Stmt(Statement::Count(rowid, count))
        }
        _ => ParsedLine::Unparsed,
    }
}

/// Open the decompressed N-Triples stream for `path`. Supported inputs are
/// the shapes MatKG actually ships: `.nt`, `.nt.gz`, and `.tar.gz`/`.tgz`
/// containing one `.nt` member. Anything else is refused by name — the
/// loader never guesses at container formats.
///
/// The reader is deliberately NOT `Send` (`tar::Entry` is not): it is
/// created and fully consumed inside one blocking task, never moved across
/// threads.
fn open_nt_stream(file: std::fs::File, path: &Path) -> Result<Box<dyn BufRead>> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let member = TarMember::first_nt(flate2::read::GzDecoder::new(file))
            .with_context(|| format!("reading tar archive {}", path.display()))?;
        Ok(Box::new(BufReader::with_capacity(1 << 20, member)))
    } else if name.ends_with(".nt.gz") {
        Ok(Box::new(BufReader::with_capacity(
            1 << 20,
            flate2::read::GzDecoder::new(file),
        )))
    } else if name.ends_with(".nt") {
        Ok(Box::new(BufReader::with_capacity(1 << 20, file)))
    } else {
        bail!(
            "unsupported MatKG input {} — expected .nt, .nt.gz, .tar.gz, or .tgz",
            path.display()
        );
    }
}

/// A tar member being streamed: reads exactly `remaining` bytes of the
/// member's content from the underlying stream.
///
/// The `tar` crate's `Entry` borrows its `Archive` and so cannot be
/// returned as an owned reader; the fixed 512-byte-block layout needed to
/// stream ONE regular member is small enough to read directly, and
/// anything that does not look like the documented single-`.nt`-member
/// MatKG layout is refused loudly.
struct TarMember<R> {
    inner: R,
    remaining: u64,
}

impl<R: std::io::Read> TarMember<R> {
    /// Scan headers until the first regular member whose name ends in
    /// `.nt`; skip anything else (pax/global extended headers, directories)
    /// by its recorded size.
    fn first_nt(mut inner: R) -> Result<Self> {
        let mut header = [0u8; 512];
        loop {
            inner
                .read_exact(&mut header)
                .context("reading tar header block")?;
            if header.iter().all(|&b| b == 0) {
                bail!("no .nt member found — expected the MatKG SUBRELOBJ.nt.tar.gz layout");
            }
            let name_end = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
            let name = std::str::from_utf8(&header[..name_end])
                .context("tar member name is not UTF-8")?
                .to_string();
            let size_field = std::str::from_utf8(&header[124..136])
                .context("tar size field is not UTF-8")?
                .trim_matches(['\0', ' '])
                .to_string();
            let size = u64::from_str_radix(&size_field, 8)
                .with_context(|| format!("tar size field {size_field:?} is not octal"))?;
            let typeflag = header[156];
            if (typeflag == b'0' || typeflag == 0) && name.ends_with(".nt") {
                return Ok(Self {
                    inner,
                    remaining: size,
                });
            }
            // Skip this member's content, padded to 512-byte blocks.
            let mut to_skip = size.div_ceil(512) * 512;
            let mut scratch = [0u8; 4096];
            while to_skip > 0 {
                let chunk = to_skip.min(scratch.len() as u64) as usize;
                inner
                    .read_exact(&mut scratch[..chunk])
                    .context("skipping tar member")?;
                to_skip -= chunk as u64;
            }
        }
    }
}

impl<R: std::io::Read> std::io::Read for TarMember<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let cap = usize::try_from(self.remaining.min(buf.len() as u64)).expect("bounded by len");
        let n = self.inner.read(&mut buf[..cap])?;
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// Pass-1 result: the dense count index plus line accounting and the exact
/// input-bytes digest.
struct CountIndex {
    counts: Vec<u32>,
    ambiguous: std::collections::HashSet<u64>,
    lines_total: u64,
    lines_unparsed: u64,
    data_sha256: String,
}

fn scan_counts(path: &Path) -> Result<CountIndex> {
    // The digest identifies the exact artifact bytes on disk (comparable
    // with the recorded upstream distribution hash), computed in its own
    // cheap pass over the raw bytes before the parse pass re-opens the
    // same path.
    let mut raw = std::fs::File::open(path)
        .with_context(|| format!("opening MatKG input {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = std::io::Read::read(&mut raw, &mut buffer).context("hashing MatKG input")?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let data_sha256 = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();

    let file = std::fs::File::open(path)
        .with_context(|| format!("opening MatKG input {}", path.display()))?;
    let mut reader = open_nt_stream(file, path)?;
    let mut counts: Vec<u32> = Vec::new();
    let mut ambiguous = std::collections::HashSet::new();
    let mut lines_total = 0u64;
    let mut lines_unparsed = 0u64;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).context("reading MatKG input")?;
        if n == 0 {
            break;
        }
        lines_total += 1;
        match parse_line(&line) {
            ParsedLine::Blank => {}
            ParsedLine::Unparsed => lines_unparsed += 1,
            ParsedLine::Stmt(Statement::Count(rowid, count)) => {
                if rowid >= MAX_ROW_ID {
                    bail!(
                        "row id {rowid} exceeds the sanity ceiling {MAX_ROW_ID} — \
                         this does not look like the MatKG SUBRELOBJ distribution"
                    );
                }
                let index = usize::try_from(rowid).expect("row id under ceiling");
                if counts.len() <= index {
                    counts.resize(index + 1, 0);
                }
                if counts[index] != 0 {
                    // A second hasCount for one row: never guess which one
                    // is real.
                    ambiguous.insert(rowid);
                } else {
                    // Counts of literal 0 cannot be distinguished from
                    // "absent" in the dense index; the pinned artifact's
                    // minimum is 25. A 0-count row is reported as unparsed
                    // rather than silently invented.
                    if count == 0 {
                        lines_unparsed += 1;
                    } else {
                        counts[index] = count;
                    }
                }
            }
            ParsedLine::Stmt(_) => {}
        }
    }
    Ok(CountIndex {
        counts,
        ambiguous,
        lines_total,
        lines_unparsed,
        data_sha256,
    })
}

#[derive(Default)]
struct RowFields {
    subject: Option<String>,
    object: Option<String>,
    has_relationship: bool,
    duplicate: bool,
}

/// Pass 2: collect the four fields of exactly the selected rows.
fn collect_selected_rows(path: &Path, selected: &Selection) -> Result<HashMap<u64, RowFields>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening MatKG input {}", path.display()))?;
    let mut reader = open_nt_stream(file, path)?;
    let mut rows: HashMap<u64, RowFields> = HashMap::with_capacity(selected.len());
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).context("reading MatKG input")?;
        if n == 0 {
            break;
        }
        let ParsedLine::Stmt(stmt) = parse_line(&line) else {
            continue;
        };
        let rowid = match stmt {
            Statement::Subject(id, _)
            | Statement::Object(id, _)
            | Statement::Relationship(id)
            | Statement::Count(id, _) => id,
        };
        if !selected.contains(rowid) {
            continue;
        }
        let entry = rows.entry(rowid).or_default();
        match stmt {
            Statement::Subject(_, path) => {
                if entry.subject.is_some() {
                    entry.duplicate = true;
                } else {
                    entry.subject = Some(path.to_string());
                }
            }
            Statement::Object(_, path) => {
                if entry.object.is_some() {
                    entry.duplicate = true;
                } else {
                    entry.object = Some(path.to_string());
                }
            }
            Statement::Relationship(_) => entry.has_relationship = true,
            Statement::Count(..) => {}
        }
    }
    Ok(rows)
}

/// The selected row-id set, as a bitset over the dense id space.
struct Selection {
    bits: Vec<u64>,
    count: usize,
}

impl Selection {
    fn new(capacity: usize) -> Self {
        Self {
            bits: vec![0; capacity.div_ceil(64)],
            count: 0,
        }
    }

    fn insert(&mut self, rowid: u64) {
        let (word, bit) = (rowid as usize / 64, rowid as usize % 64);
        if self.bits[word] & (1 << bit) == 0 {
            self.bits[word] |= 1 << bit;
            self.count += 1;
        }
    }

    fn contains(&self, rowid: u64) -> bool {
        let (word, bit) = (rowid as usize / 64, rowid as usize % 64);
        self.bits.get(word).is_some_and(|w| w & (1 << bit) != 0)
    }

    fn len(&self) -> usize {
        self.count
    }
}

/// One decoded endpoint of a co-occurrence pair.
#[derive(Debug, Clone)]
struct Endpoint {
    name: String,
    canonical: String,
    label: &'static str,
    class_iri: String,
    matkg_uri: String,
}

fn decode_endpoint(raw_path: &str, ontology: &dyn Ontology) -> Option<Endpoint> {
    let (code, encoded_name) = raw_path.split_once('/')?;
    let label = code_to_label(code)?;
    let name = percent_decode_strict(encoded_name)?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return None;
    }
    // Resolve through the REGISTERED declaration — the loader never invents
    // a class identity the ontology does not declare.
    let class = ontology.class_for_label(label)?;
    let storage_label = ontology.storage_label(label)?;
    debug_assert_eq!(storage_label, label, "MatKG storage mapping is identity");
    Some(Endpoint {
        canonical: canonical_key(&name),
        name,
        label,
        class_iri: class.iri.as_str().to_string(),
        matkg_uri: format!("http://example.com/{raw_path}"),
    })
}

/// Load MatKG facts from `path` into `store`, honestly bounded and fully
/// accounted. See the module docs for the semantics of every decision.
pub async fn load(
    path: &Path,
    store: &ProvenanceStore,
    options: MatkgLoadOptions,
) -> Result<MatkgLoadReport> {
    let ontology = crate::ontologies::active(Some(MATKG_ONTOLOGY_ID))
        .context("the MatKG ontology must be registered before loading")?;
    let tenant = storage_tenant(prism_provenance::LOCAL_TENANT, MATKG_ONTOLOGY_ID);

    // Pass 1 (blocking IO on a blocking thread): count index + input hash.
    let scan_path = path.to_path_buf();
    let index = tokio::task::spawn_blocking(move || scan_counts(&scan_path))
        .await
        .context("MatKG scan task panicked")??;

    // Selection: min_count filter, then top-`limit` by (count desc, id asc).
    let mut rows_total = 0u64;
    let mut rows_below_min_count = 0u64;
    let mut candidates: Vec<(u32, u64)> = Vec::new();
    for (rowid, &count) in index.counts.iter().enumerate() {
        if count == 0 {
            continue;
        }
        rows_total += 1;
        if count < options.min_count {
            rows_below_min_count += 1;
        } else {
            candidates.push((count, rowid as u64));
        }
    }
    let mut rows_beyond_limit = 0u64;
    if let Some(limit) = options.limit
        && candidates.len() > limit
    {
        // Deterministic: highest count first, ties to the smaller row id.
        candidates.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        rows_beyond_limit = (candidates.len() - limit) as u64;
        candidates.truncate(limit);
    }
    let mut selection = Selection::new(index.counts.len());
    for &(_, rowid) in &candidates {
        selection.insert(rowid);
    }
    let rows_selected = selection.len() as u64;

    // Pass 2 (blocking IO on a blocking thread): fields of selected rows.
    let collect_path = path.to_path_buf();
    let bits = std::sync::Arc::new(selection);
    let bits_for_task = std::sync::Arc::clone(&bits);
    let fields =
        tokio::task::spawn_blocking(move || collect_selected_rows(&collect_path, &bits_for_task))
            .await
            .context("MatKG collect task panicked")??;

    // Assemble pairs: deterministic ascending row-id order, merge onto the
    // unordered canonical name pair with MAX count.
    let mut rowids: Vec<u64> = candidates.iter().map(|&(_, id)| id).collect();
    rowids.sort_unstable();
    let mut rows_incomplete = 0u64;
    let mut rows_malformed = 0u64;
    let mut rows_ambiguous = 0u64;
    let mut rows_self_pair = 0u64;
    let mut rows_merged_duplicates = 0u64;
    struct Pair {
        subject: Endpoint,
        object: Endpoint,
        count: u32,
    }
    let mut pairs: HashMap<(String, String), Pair> = HashMap::new();
    for rowid in rowids {
        let count = index.counts[usize::try_from(rowid).expect("dense id")];
        if index.ambiguous.contains(&rowid) {
            rows_ambiguous += 1;
            continue;
        }
        let Some(row) = fields.get(&rowid) else {
            rows_incomplete += 1;
            continue;
        };
        if row.duplicate {
            rows_ambiguous += 1;
            continue;
        }
        let (Some(subject_raw), Some(object_raw), true) = (
            row.subject.as_deref(),
            row.object.as_deref(),
            row.has_relationship,
        ) else {
            rows_incomplete += 1;
            continue;
        };
        let (Some(subject), Some(object)) = (
            decode_endpoint(subject_raw, ontology.as_ref()),
            decode_endpoint(object_raw, ontology.as_ref()),
        ) else {
            rows_malformed += 1;
            continue;
        };
        if subject.canonical == object.canonical {
            rows_self_pair += 1;
            continue;
        }
        // Canonical direction: lexicographically smaller canonical name is
        // the subject, so mirrored rows land on one identity.
        let (subject, object) = if subject.canonical <= object.canonical {
            (subject, object)
        } else {
            (object, subject)
        };
        let key = (subject.canonical.clone(), object.canonical.clone());
        match pairs.entry(key) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(Pair {
                    subject,
                    object,
                    count,
                });
            }
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                rows_merged_duplicates += 1;
                if count > slot.get().count {
                    slot.insert(Pair {
                        subject,
                        object,
                        count,
                    });
                }
            }
        }
    }

    // Deterministic write order.
    let mut ordered: Vec<(&(String, String), &Pair)> = pairs.iter().collect();
    ordered.sort_unstable_by(|a, b| a.0.cmp(b.0));

    let started_at = chrono::Utc::now().to_rfc3339();
    let activity_id = uuid::Uuid::new_v4().to_string();
    let mut prov = LocalProvenance {
        activity_id: activity_id.clone(),
        agent_id: MATKG_AGENT_ID.into(),
        agent_kind: "SoftwareAgent".into(),
        source_entity_id: MATKG_SOURCE_ID.into(),
        source_kind: "Dataset".into(),
        tenant: tenant.clone(),
        started_at: started_at.clone(),
        ended_at: started_at,
        locality: "local".into(),
        // The loader reads the artifact itself; the DOI is the origin.
        origin_source_id: None,
    };
    store.record_activity(&prov).await?;

    let classification = OntologyClassification {
        version_iri: ontology.version_iri().as_str(),
        artifact_sha256: ontology.artifact_sha256(),
    };

    let mut entity_seen: std::collections::BTreeSet<(String, &'static str)> =
        std::collections::BTreeSet::new();
    for (_, pair) in &ordered {
        let fact = LocalFact {
            subject: pair.subject.name.clone(),
            predicate: MATKG_PREDICATE.into(),
            object: pair.object.name.clone(),
            // The raw MatKG tally, verbatim and auditable.
            value: Some(f64::from(pair.count)),
            unit: None,
            confidence: Some(confidence_for_count(pair.count)),
            kind: None,
        };
        let nodes = ClassifiedFactNodes {
            subject: ClassifiedNode {
                entity_type: pair.subject.label,
                storage_label: pair.subject.label,
                class_iri: &pair.subject.class_iri,
            },
            object: ClassifiedNode {
                entity_type: pair.object.label,
                storage_label: pair.object.label,
                class_iri: &pair.object.class_iri,
            },
        };
        store
            .write_classified_fact_with_evidence(
                &fact,
                &prov,
                EvidenceClass::Research,
                nodes,
                classification,
                // MatKG facts carry no kind — COOCCURS_WITH stays a generic
                // edge under the MatKG declaration, never an EMMO shape.
                None,
            )
            .await?;
        for endpoint in [&pair.subject, &pair.object] {
            if entity_seen.insert((endpoint.canonical.clone(), endpoint.label)) {
                // Additive props write: preserves the original placeholder
                // URI for traceability without adopting it as identity.
                let props = serde_json::json!({ "matkg_uri": endpoint.matkg_uri }).to_string();
                store
                    .write_classified_entity(
                        &endpoint.name,
                        ClassifiedNode {
                            entity_type: endpoint.label,
                            storage_label: endpoint.label,
                            class_iri: &endpoint.class_iri,
                        },
                        Some(props),
                        &tenant,
                    )
                    .await?;
            }
        }
    }

    // A queryable record of the load itself: dataset identity, exact input
    // bytes, artifact identity, licence, and the REQUIRED attribution.
    let facts_written = ordered.len() as u64;
    let dataset_props = serde_json::json!({
        "doi": MATKG_SOURCE_ID,
        "release": "1.4",
        "license": "CC-BY-4.0",
        "attribution": MATKG_ATTRIBUTION,
        "data_sha256": index.data_sha256,
        "ontology_version_iri": classification.version_iri,
        "ontology_artifact_sha256": classification.artifact_sha256,
        "activity_id": activity_id,
    })
    .to_string();
    store
        .write_extracted_entity(
            "MatKG 1.4 (SUBRELOBJ)",
            "Dataset",
            Some(dataset_props),
            &tenant,
        )
        .await?;

    // Close the activity honestly: ended_at is the real end of the run.
    prov.ended_at = chrono::Utc::now().to_rfc3339();
    store.record_activity(&prov).await?;

    Ok(MatkgLoadReport {
        rows_total,
        rows_below_min_count,
        rows_beyond_limit,
        rows_selected,
        rows_incomplete,
        rows_malformed,
        rows_ambiguous,
        rows_self_pair,
        rows_merged_duplicates,
        facts_written,
        entities_written: entity_seen.len() as u64,
        lines_unparsed: index.lines_unparsed,
        lines_total: index.lines_total,
        tenant,
        activity_id: prov.activity_id,
        data_sha256: index.data_sha256,
        ontology_artifact_sha256: classification.artifact_sha256.to_string(),
        ontology_version_iri: classification.version_iri.to_string(),
        attribution: MATKG_ATTRIBUTION.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_provenance::ProvenanceStore;
    use std::path::PathBuf;

    /// Tempfile-backed inputs and store, removed on drop.
    struct TempFiles {
        dir: PathBuf,
    }

    impl TempFiles {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("prism_matkg_test_{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }
    }

    impl Drop for TempFiles {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn stmt(rowid: u64, pred: &str, object: &str) -> String {
        format!("<http://example.com/sro_row{rowid}> <http://example.com/{pred}> {object} .\n")
    }

    fn uri(path: &str) -> String {
        format!("<http://example.com/{path}>")
    }

    fn count_lit(count: u32) -> String {
        format!("\"{count}\"^^<http://www.w3.org/2001/XMLSchema#integer>")
    }

    /// One complete reified row as four statements.
    fn row(rowid: u64, subject: &str, rel: &str, object: &str, count: u32) -> [String; 4] {
        [
            stmt(rowid, "hasSubject", &uri(subject)),
            stmt(rowid, "hasRelationship", &uri(rel)),
            stmt(rowid, "hasObject", &uri(object)),
            stmt(rowid, "hasCount", &count_lit(count)),
        ]
    }

    /// Interleave the statements of many rows so that no two statements of
    /// one row are adjacent — the scatter the real 21.6M-line file has.
    fn scattered(rows: &[[String; 4]]) -> String {
        let mut out = String::new();
        for field in 0..4 {
            for row in rows {
                out.push_str(&row[field]);
            }
        }
        out
    }

    async fn open_store(files: &TempFiles) -> ProvenanceStore {
        ProvenanceStore::open(&files.path("store.db"))
            .await
            .expect("open temp store")
    }

    fn all_rows() -> MatkgLoadOptions {
        MatkgLoadOptions {
            min_count: 1,
            limit: None,
        }
    }

    /// The four statements of one row are joined BY ROW ID across the whole
    /// scattered stream: a join that pairs a subject with another row's
    /// object produces pairs this test does not accept. Names are
    /// percent-decoded (dirt like the trailing semicolon is real upstream
    /// data and is kept verbatim), classes come from the URI path segment,
    /// and the co-occurrence tally rides the fact as its auditable value.
    #[tokio::test]
    async fn reified_join_recovers_scattered_rows_exactly() {
        let files = TempFiles::new();
        let nt = scattered(&[
            row(7, "CHM/Silicon%20Oxide", "CHM-PRO", "PRO/Band%20Gap", 40),
            row(3, "SPL/Olivine%3B", "SPL-CHM", "CHM/LiFePO4", 117),
            row(11, "SMT/Electrospinning", "SMT-APL", "APL/Filtration", 33),
        ]);
        std::fs::write(files.path("fixture.nt"), nt).unwrap();
        let store = open_store(&files).await;

        let report = load(&files.path("fixture.nt"), &store, all_rows())
            .await
            .unwrap();
        assert_eq!(report.rows_total, 3);
        assert_eq!(report.facts_written, 3);
        assert_eq!(report.entities_written, 6);
        assert_eq!(report.lines_unparsed, 0);
        assert_eq!(report.lines_total, 12);

        // Exact recovered pairs, through the production read API. The pair
        // direction is canonical (lexicographically smaller name first), so
        // each row's own subject/object must land together — a cross-row
        // join lands names this list refuses.
        let facts = store
            .recall_with_context_scoped("", &[&report.tenant], 50)
            .await
            .unwrap();
        let mut pairs: Vec<(String, String, Option<f64>)> = facts
            .iter()
            .map(|f| (f.subject.clone(), f.object.clone(), f.value))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            pairs,
            vec![
                ("Band Gap".into(), "Silicon Oxide".into(), Some(40.0)),
                ("Electrospinning".into(), "Filtration".into(), Some(33.0)),
                ("LiFePO4".into(), "Olivine;".into(), Some(117.0)),
            ],
            "each reified row must be joined by row id, decoded, and \
             canonically ordered: {facts:?}"
        );
        assert!(
            facts.iter().all(|f| f.predicate == MATKG_PREDICATE),
            "every MatKG fact is a co-occurrence: {facts:?}"
        );

        // Classes come from the URI path segment of the row's OWN
        // subject/object statements.
        let nodes = store
            .graph_search_scoped("LiFePO4", &[&report.tenant], 10)
            .await
            .unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].label, "Chemical");
        assert_eq!(
            nodes[0].class_iri.as_deref(),
            Some("https://mirdyne.com/ontology/matkg#Chemical")
        );
        let nodes = store
            .graph_search_scoped("Olivine;", &[&report.tenant], 10)
            .await
            .unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].label, "SymmetryPhaseLabel");
    }

    /// Bounds are honest: every skipped row lands in exactly one reported
    /// bucket, and the report arithmetic reconciles to the row total. A
    /// mutation that suppresses a skip counter (or silently truncates)
    /// dies on the exact numbers.
    #[tokio::test]
    async fn min_count_and_limit_bound_the_load_and_report_every_skip() {
        let files = TempFiles::new();
        let nt = scattered(&[
            row(1, "CHM/A1", "CHM-PRO", "PRO/B1", 30),
            row(2, "CHM/A2", "CHM-PRO", "PRO/B2", 100),
            row(3, "CHM/A3", "CHM-PRO", "PRO/B3", 500),
            row(4, "CHM/A4", "CHM-PRO", "PRO/B4", 40),
            row(5, "CHM/A5", "CHM-PRO", "PRO/B5", 70),
        ]);
        std::fs::write(files.path("fixture.nt"), nt).unwrap();
        let store = open_store(&files).await;

        let report = load(
            &files.path("fixture.nt"),
            &store,
            MatkgLoadOptions {
                min_count: 40,
                limit: Some(3),
            },
        )
        .await
        .unwrap();

        assert_eq!(report.rows_total, 5);
        assert_eq!(report.rows_below_min_count, 1, "count 30 < min_count 40");
        assert_eq!(report.rows_beyond_limit, 1, "count 40 is outside the top 3");
        assert_eq!(report.rows_selected, 3);
        assert_eq!(report.facts_written, 3);
        assert_eq!(
            report.rows_total,
            report.rows_below_min_count + report.rows_beyond_limit + report.rows_selected,
            "no row may vanish from the accounting"
        );
        assert_eq!(
            report.rows_selected,
            report.rows_incomplete
                + report.rows_malformed
                + report.rows_ambiguous
                + report.rows_self_pair
                + report.rows_merged_duplicates
                + report.facts_written,
        );

        // The strongest three landed; the filtered and truncated ones did not.
        let tenant = report.tenant.as_str();
        for present in ["A2", "A3", "A5"] {
            assert_eq!(
                store
                    .graph_search_scoped(present, &[tenant], 10)
                    .await
                    .unwrap()
                    .len(),
                1,
                "{present} passed the bounds and must be stored"
            );
        }
        for absent in ["A1", "A4"] {
            assert!(
                store
                    .graph_search_scoped(absent, &[tenant], 10)
                    .await
                    .unwrap()
                    .is_empty(),
                "{absent} was skipped and must NOT be stored"
            );
        }
    }

    /// MatKG tallies the same unordered name pair repeatedly — mirrored
    /// directions and repeated per-mention NER type contexts, with
    /// genuinely differing counts (measured on the real file). Rows merge
    /// onto the unordered canonical pair keeping the MAXIMUM count (a sum
    /// would double-count symmetric duplicates), and self-pairs are
    /// refused.
    #[tokio::test]
    async fn mirror_and_type_context_duplicates_merge_on_max_count() {
        let files = TempFiles::new();
        let nt = scattered(&[
            row(1, "CHM/Oxygen", "CHM-SMT", "SMT/Porphyrin", 100),
            row(2, "SMT/Porphyrin", "SMT-CHM", "CHM/Oxygen", 80),
            row(3, "PRO/Oxygen", "PRO-CHM", "CHM/Porphyrin", 60),
            row(4, "SPL/Water", "SPL-CHM", "CHM/Water", 55),
        ]);
        std::fs::write(files.path("fixture.nt"), nt).unwrap();
        let store = open_store(&files).await;

        let report = load(&files.path("fixture.nt"), &store, all_rows())
            .await
            .unwrap();
        assert_eq!(report.rows_total, 4);
        assert_eq!(
            report.rows_merged_duplicates, 2,
            "rows 2 and 3 merge into row 1's pair"
        );
        assert_eq!(report.rows_self_pair, 1, "Water–Water is degenerate");
        assert_eq!(report.facts_written, 1);

        let facts = store
            .recall_with_context_scoped("Oxygen", &[&report.tenant], 10)
            .await
            .unwrap();
        assert_eq!(facts.len(), 1, "one fact for the unordered pair: {facts:?}");
        assert_eq!(facts[0].value, Some(100.0), "max count wins, never the sum");
    }

    /// The non-negotiable isolation and honesty properties, asserted
    /// through the store's production read APIs: MatKG facts live under
    /// `local@matkg` and are INVISIBLE to a bare `local` read; they are
    /// evidence class Research (ORANGE) — literature co-occurrence, not
    /// measurement — and every fact is PROV-O-attributed to the dataset
    /// DOI and stamped with the exact ontology artifact.
    #[tokio::test]
    async fn matkg_facts_land_isolated_research_and_prov_attributed() {
        let files = TempFiles::new();
        let nt = scattered(&[row(1, "CHM/LiFePO4", "CHM-SPL", "SPL/Olivine", 117)]);
        std::fs::write(files.path("fixture.nt"), nt).unwrap();
        let store = open_store(&files).await;

        let report = load(&files.path("fixture.nt"), &store, all_rows())
            .await
            .unwrap();
        assert_eq!(report.tenant, "local@matkg");
        assert_eq!(report.facts_written, 1);

        // Isolation: nothing under the user's own tenant.
        assert!(
            store
                .graph_search_scoped("LiFePO4", &["local"], 10)
                .await
                .unwrap()
                .is_empty(),
            "MatKG entities must never appear under the local tenant"
        );
        assert!(
            store
                .recall_with_context_scoped("LiFePO4", &["local"], 10)
                .await
                .unwrap()
                .is_empty(),
            "MatKG assertions must never appear under the local tenant"
        );
        // ...but discovered by the DEFAULT read scope, labelled.
        assert_eq!(
            store.default_read_tenants().await.unwrap(),
            ["local", "local@matkg"],
            "the loaded tenant must join the default read scope"
        );

        let facts = store
            .recall_with_context_scoped("LiFePO4", &["local@matkg"], 10)
            .await
            .unwrap();
        assert_eq!(facts.len(), 1);
        let fact = &facts[0];
        assert_eq!(
            fact.evidence_class,
            EvidenceClass::Research,
            "literature co-occurrence is ORANGE, never anything stronger"
        );
        assert_eq!(fact.tenant, "local@matkg");
        assert_eq!(fact.source, MATKG_SOURCE_ID);
        assert_eq!(fact.agent, MATKG_AGENT_ID);
        assert!(
            (fact.confidence - confidence_for_count(117)).abs() < 1e-12,
            "confidence is the documented log-compressed map of the count"
        );
        assert!(fact.confidence <= 0.80, "capped below certainty");

        // PROV-O: the per-source evidence key is the dataset DOI, and the
        // assertion is stamped with the MatKG artifact that classified it.
        // The count rides the assertion as its value, so the id is the
        // CONDITIONED one.
        let assertion = prism_provenance::conditioned_assertion_id(
            "local@matkg",
            "LiFePO4",
            MATKG_PREDICATE,
            "Olivine",
            Some(117.0),
            None,
            &[],
        )
        .unwrap();
        let evidence = store.assertion_evidence_by_id(&assertion).await.unwrap();
        assert_eq!(evidence.len(), 1, "one source: the dataset DOI");
        assert_eq!(evidence[0].source_key, "doi:10.5281/zenodo.10144972");
        assert_eq!(evidence[0].evidence_class, EvidenceClass::Research);
        let classifications = store.assertion_classifications(&assertion).await.unwrap();
        assert_eq!(
            classifications.len(),
            1,
            "one load, one classification stamp"
        );
        assert_eq!(
            classifications[0].version_iri,
            "https://mirdyne.com/ontology/matkg/1.4"
        );
        assert_eq!(
            classifications[0].artifact_sha256,
            report.ontology_artifact_sha256
        );
        assert_eq!(classifications[0].activity_id, report.activity_id);
    }

    /// Re-running the loader over the same artifact must not inflate
    /// anything: same facts, same entities, ONE evidence contribution under
    /// the DOI source key, unchanged confidence. This leans on the store's
    /// existing per-source dedup — the loader adds no second mechanism.
    #[tokio::test]
    async fn reloading_the_same_artifact_inflates_nothing() {
        let files = TempFiles::new();
        let nt = scattered(&[
            row(1, "CHM/LiFePO4", "CHM-SPL", "SPL/Olivine", 117),
            row(2, "SMT/Sintering", "SMT-PRO", "PRO/Density", 45),
        ]);
        std::fs::write(files.path("fixture.nt"), nt).unwrap();
        let store = open_store(&files).await;

        let first = load(&files.path("fixture.nt"), &store, all_rows())
            .await
            .unwrap();
        let facts_before = store
            .recall_with_context_scoped("", &[&first.tenant], 50)
            .await
            .unwrap();

        let second = load(&files.path("fixture.nt"), &store, all_rows())
            .await
            .unwrap();
        assert_eq!(second.facts_written, first.facts_written);

        let facts_after = store
            .recall_with_context_scoped("", &[&second.tenant], 50)
            .await
            .unwrap();
        assert_eq!(
            facts_after.len(),
            facts_before.len(),
            "no duplicate assertions"
        );
        for (before, after) in facts_before.iter().zip(&facts_after) {
            assert_eq!(before.subject, after.subject);
            assert!(
                (before.confidence - after.confidence).abs() < 1e-12,
                "a re-load from the SAME source must not move confidence: \
                 {} vs {}",
                before.confidence,
                after.confidence
            );
        }
        let nodes = store
            .graph_search_scoped("", &[&second.tenant], 50)
            .await
            .unwrap();
        // 4 fact endpoints + the dataset record node.
        assert_eq!(nodes.len(), 5, "no duplicate entities: {nodes:?}");
    }

    /// Data dirt is counted, never guessed at: unknown type codes, empty
    /// and undecodable names are malformed; a row missing one of its four
    /// statements is incomplete; both are excluded from the store.
    #[tokio::test]
    async fn dirt_rows_are_reported_and_excluded() {
        let files = TempFiles::new();
        let mut rows_nt = scattered(&[
            row(1, "CHM/Good", "CHM-PRO", "PRO/Fact", 50),
            row(2, "XXX/UnknownType", "XXX-PRO", "PRO/Y", 60),
            row(3, "CHM/", "CHM-PRO", "PRO/EmptyName", 70),
            row(4, "CHM/Bad%ZZEscape", "CHM-PRO", "PRO/Z", 80),
        ]);
        // Row 5 is incomplete: no hasObject statement.
        rows_nt.push_str(&stmt(5, "hasSubject", &uri("CHM/Lonely")));
        rows_nt.push_str(&stmt(5, "hasRelationship", &uri("CHM-PRO")));
        rows_nt.push_str(&stmt(5, "hasCount", &count_lit(90)));
        std::fs::write(files.path("fixture.nt"), rows_nt).unwrap();
        let store = open_store(&files).await;

        let report = load(&files.path("fixture.nt"), &store, all_rows())
            .await
            .unwrap();
        assert_eq!(report.rows_total, 5);
        assert_eq!(report.rows_malformed, 3);
        assert_eq!(report.rows_incomplete, 1);
        assert_eq!(report.facts_written, 1);
        assert!(
            store
                .graph_search_scoped("UnknownType", &[&report.tenant], 10)
                .await
                .unwrap()
                .is_empty(),
            "a malformed row must not half-land"
        );
    }

    /// The gz and tar.gz containers stream to the same result as the plain
    /// file — the shapes MatKG actually ships.
    #[tokio::test]
    async fn compressed_containers_load_identically() {
        use std::io::Write as _;

        let files = TempFiles::new();
        let nt = scattered(&[
            row(1, "CHM/LiFePO4", "CHM-SPL", "SPL/Olivine", 117),
            row(2, "SMT/Sintering", "SMT-PRO", "PRO/Density", 45),
        ]);
        std::fs::write(files.path("fixture.nt"), &nt).unwrap();

        // .nt.gz
        let gz_file = std::fs::File::create(files.path("fixture.nt.gz")).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(gz_file, flate2::Compression::fast());
        encoder.write_all(nt.as_bytes()).unwrap();
        encoder.finish().unwrap();

        // .tar.gz with a correct ustar header (name, octal size, checksum).
        let mut header = [0u8; 512];
        let name = b"SUBRELOBJ.nt";
        header[..name.len()].copy_from_slice(name);
        header[100..107].copy_from_slice(b"0000644"); // mode
        header[108..115].copy_from_slice(b"0000000"); // uid
        header[116..123].copy_from_slice(b"0000000"); // gid
        let size_octal = format!("{:011o}", nt.len());
        header[124..124 + 11].copy_from_slice(size_octal.as_bytes());
        header[136..147].copy_from_slice(b"00000000000"); // mtime
        header[148..156].copy_from_slice(b"        "); // checksum spaces for computing
        header[156] = b'0'; // regular file
        header[257..262].copy_from_slice(b"ustar");
        let checksum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        let checksum_octal = format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(checksum_octal.as_bytes());
        let mut tar_bytes = header.to_vec();
        tar_bytes.extend_from_slice(nt.as_bytes());
        let padding = (512 - nt.len() % 512) % 512;
        tar_bytes.extend(std::iter::repeat_n(0u8, padding));
        tar_bytes.extend(std::iter::repeat_n(0u8, 1024)); // end-of-archive
        let tgz_file = std::fs::File::create(files.path("fixture.tar.gz")).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(tgz_file, flate2::Compression::fast());
        encoder.write_all(&tar_bytes).unwrap();
        encoder.finish().unwrap();

        let mut written = Vec::new();
        for name in ["fixture.nt", "fixture.nt.gz", "fixture.tar.gz"] {
            let store = ProvenanceStore::open(&files.path(&format!("store_{name}.db")))
                .await
                .unwrap();
            let report = load(&files.path(name), &store, all_rows()).await.unwrap();
            written.push((report.facts_written, report.rows_total, report.lines_total));
        }
        assert_eq!(written[0], (2, 2, 8));
        assert_eq!(written[0], written[1], "gz must decode to the same rows");
        assert_eq!(
            written[0], written[2],
            "tar.gz must decode to the same rows"
        );
    }

    /// The confidence map is the documented, capped log compression.
    #[test]
    fn confidence_is_log_compressed_and_capped() {
        assert!((confidence_for_count(25) - (25f64.log10() / 6.0)).abs() < 1e-12);
        assert!(confidence_for_count(100) < confidence_for_count(1000));
        assert!(
            (confidence_for_count(206_811) - 0.80).abs() < 1e-12,
            "capped"
        );
        assert!(confidence_for_count(1) >= 0.0);
    }
}
