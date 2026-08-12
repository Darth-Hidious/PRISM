//! Workspace structures state — the materials plane of the sidebar.
//!
//! The Structures tab shows the SCIENCE of the session: the crystal
//! structures it imported, looked up, or produced, and their CIF
//! documentation — not agent plumbing. Rows are metadata from the
//! content-addressed structure cache (`app/tools/structure_io.py`,
//! `CacheStore`): `{tool, name, formula, n_atoms, composition, source}`,
//! referenced as `cache://<sha256>/structure.cif`.
//!
//! Like the artifacts plane, this module owns only typed, sanitized state;
//! all store I/O is performed by the backend behind
//! `workspace.structures.list` / `workspace.structure.fetch` requests, so
//! `render::draw` never touches disk or network.
//!
//! Honesty contract (mirrors `ObjectKind::Other` / `ObjectStatus::Unknown`):
//! nothing here is ever guessed. A missing or malformed meta field renders
//! as `unknown`; a `source` this build has no special handling for is
//! carried VERBATIM — a user import, a database lookup and a relaxation are
//! different epistemic objects and must read as themselves.

use std::time::Duration;

use serde_json::Value;

use crate::sanitize::sanitize_for_render;

/// Explicit policy for listing structures and displaying CIF text.
///
/// Keeping the limits here makes truncation behavior visible and testable
/// instead of hiding magic numbers in event handlers (same discipline as
/// [`crate::artifact::ArtifactPolicy`]).
#[derive(Debug, Clone)]
pub struct StructurePolicy {
    /// Maximum structure rows requested from the cache in one refresh.
    ///
    /// The renderer warns when this ceiling is reached, so a bounded query
    /// is never presented as the complete structure cache.
    pub list_limit: u64,
    /// Delay used to coalesce startup/tab-entry refresh requests.
    pub refresh_debounce: Duration,
    /// Delay before retrying while the active agent turn owns the tool
    /// worker (the backend answers `ui.structures.pending` meanwhile).
    pub busy_retry_delay: Duration,
    /// Fixed number of sidebar lines allocated to each structure row.
    pub item_lines: usize,
    /// Additional inline-detail lines reserved for the expanded selection.
    pub expanded_lines: usize,
    /// Maximum CIF bytes retained in the interactive detail panel.
    ///
    /// CIF text can be large (supercells). The full text remains in the
    /// structure cache; this bound keeps a large fetch from becoming a
    /// multi-megabyte allocation held by the TUI. Default: 256 KiB.
    pub cif_bytes: usize,
}

impl Default for StructurePolicy {
    fn default() -> Self {
        Self {
            list_limit: 256,
            refresh_debounce: Duration::from_millis(150),
            busy_retry_delay: Duration::from_secs(1),
            item_lines: 3,
            expanded_lines: 3,
            cif_bytes: 256 * 1024,
        }
    }
}

/// One structure row in the Workspace *Structures* tab.
///
/// Identity is the content-addressed `cache_key` (`sha256(cif_text)`).
/// Every other field is backend-supplied metadata and OPTIONAL: the meta
/// writers differ (`structure_import` records `{tool, name, formula,
/// n_atoms, composition, source}`; the MACE job runner records
/// `{tool_name, head, phase, composition, n_atoms}`), so a missing field
/// is a fact, and it renders as `unknown` — never as a plausible default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceStructure {
    /// Content-addressed cache key; the fetch key for the CIF text.
    pub cache_key: String,
    /// `cache://<key>/structure.cif` reference, when the backend sent one.
    pub cache_ref: Option<String>,
    /// Chemical formula as computed by the backend (e.g. by ASE). This is
    /// the identity a materials person scans for; `None` renders "unknown"
    /// — a formula PRISM did not compute must not be invented.
    pub formula: Option<String>,
    /// Atom count reported by the backend.
    pub n_atoms: Option<u64>,
    /// Composition rendered from the backend's element→count map (sorted,
    /// e.g. "Al1 Ti1"). `None` when absent or not a valid map.
    pub composition: Option<String>,
    /// Provenance of the structure — `user_import`, a database lookup, a
    /// relaxation, … Carried VERBATIM like `ObjectKind::Other`; `None`
    /// renders "unknown". Never guessed from other fields.
    pub source: Option<String>,
    /// Optional label stored at import time (e.g. "mp-1823 TiAl").
    pub name: Option<String>,
    /// Tool that wrote the cache entry (`tool` or legacy `tool_name`).
    pub tool: Option<String>,
    /// Cache-entry timestamp reported by the backend.
    pub created_at: Option<String>,
}

/// Placeholder rendered for every missing meta field. Deliberately literal:
/// an empty string could read as a value, and a guess would be a lie.
pub const UNKNOWN: &str = "unknown";

impl WorkspaceStructure {
    /// Parse one backend row without inventing missing metadata.
    ///
    /// `cache_key` is the only required field (it is the fetch identity);
    /// a row without it is an error, mirroring the artifact discipline.
    /// Optional fields that are absent or of the wrong type become `None`
    /// and render as `unknown`.
    pub fn from_value(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "structure row is not an object".to_string())?;

        let cache_key = object
            .get("cache_key")
            .and_then(Value::as_str)
            .ok_or_else(|| "structure row has no valid `cache_key`".to_string())?;
        let clean_key = sanitize_for_render(cache_key);
        if clean_key != cache_key {
            return Err("structure row `cache_key` contains terminal controls".to_string());
        }
        if clean_key.trim().is_empty() {
            return Err("structure row `cache_key` is empty".to_string());
        }

        /// Optional text field: sanitized, non-empty, else `None`.
        fn text_opt(object: &serde_json::Map<String, Value>, field: &str) -> Option<String> {
            let raw = object.get(field).and_then(Value::as_str)?;
            let clean = sanitize_for_render(raw);
            (clean.trim() != "").then_some(clean)
        }

        let n_atoms = object.get("n_atoms").and_then(Value::as_u64);

        // Composition arrives as an element→count map (Counter in the meta
        // writers). Any invalid entry makes the WHOLE field unknown — a
        // half-rendered composition would be a partial invention.
        let composition = object
            .get("composition")
            .and_then(Value::as_object)
            .and_then(|map| {
                let mut parts: Vec<(String, u64)> = Vec::new();
                for (element, count) in map {
                    if element.trim().is_empty() {
                        return None;
                    }
                    parts.push((sanitize_for_render(element), count.as_u64()?));
                }
                if parts.is_empty() {
                    return None;
                }
                parts.sort();
                Some(
                    parts
                        .iter()
                        .map(|(element, count)| format!("{element}{count}"))
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            });

        // The meta writers disagree on the field name; both spellings are
        // accepted verbatim. This is format tolerance, not guessing.
        let tool = text_opt(object, "tool").or_else(|| text_opt(object, "tool_name"));

        Ok(Self {
            cache_key: clean_key,
            cache_ref: text_opt(object, "cache_ref"),
            formula: text_opt(object, "formula"),
            n_atoms,
            composition,
            source: text_opt(object, "source"),
            name: text_opt(object, "name"),
            tool,
            created_at: text_opt(object, "created_at"),
        })
    }

    /// Formula for display — `unknown` when the backend never sent one.
    pub fn formula_display(&self) -> &str {
        self.formula.as_deref().unwrap_or(UNKNOWN)
    }

    /// The `cache://` reference for display — `unknown` when absent.
    pub fn cache_ref_display(&self) -> &str {
        self.cache_ref.as_deref().unwrap_or(UNKNOWN)
    }

    /// Source for display — verbatim when reported, `unknown` otherwise.
    pub fn source_display(&self) -> &str {
        self.source.as_deref().unwrap_or(UNKNOWN)
    }
}

/// Health/loading state for the session's structure cache view.
///
/// `Ready([])` ("no structures yet") and `Unavailable` ("structure cache
/// unavailable") are different facts and must never be collapsed into one.
#[derive(Debug, Clone, Default)]
pub enum StructuresStoreState {
    /// A request has not completed yet (including a backend-busy retry).
    #[default]
    Loading,
    /// The cache was queried and returned a valid list. May be empty.
    Ready(Vec<WorkspaceStructure>),
    /// The cache could not be queried or returned a malformed response.
    Unavailable(String),
}

/// Format CIF text for the detail panel, bounded by `max_bytes`.
///
/// The bound applies to what the TUI holds for display; the full text
/// remains in the structure cache. If the backend already flagged the text
/// as truncated, or the display policy truncates it here, the banner says
/// so — a truncated CIF is never presented as the whole file.
pub fn format_cif_body(cif: &str, max_bytes: usize, backend_truncated: bool) -> String {
    let mut truncated = backend_truncated;
    let mut text: String = if cif.len() > max_bytes {
        // Floor to a UTF-8 char boundary so the bound never splits a codepoint.
        let mut cut = max_bytes;
        while cut > 0 && !cif.is_char_boundary(cut) {
            cut -= 1;
        }
        truncated = true;
        cif[..cut].to_string()
    } else {
        cif.to_string()
    };
    if truncated {
        text.push_str(&format!(
            "\n\n---\nCIF display truncated at {} by policy; the full text remains in the structure cache.",
            format_cif_bytes(u64::try_from(max_bytes).unwrap_or(u64::MAX))
        ));
    }
    text
}

/// Compact exact byte size for truncation banners.
fn format_cif_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    let bytes_f = bytes as f64;
    if bytes_f >= MIB {
        format!("{:.1} MiB", bytes_f / MIB)
    } else if bytes_f >= KIB {
        format!("{:.1} KiB", bytes_f / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_complete_structure_row() {
        let row = WorkspaceStructure::from_value(&json!({
            "cache_key": "ab12",
            "cache_ref": "cache://ab12/structure.cif",
            "tool": "structure_import",
            "name": "TiAl gamma",
            "formula": "TiAl",
            "n_atoms": 2,
            "composition": {"Ti": 1, "Al": 1},
            "source": "user_import",
            "created_at": "2026-08-11T10:00:00+00:00"
        }))
        .expect("valid row");

        assert_eq!(row.formula_display(), "TiAl");
        assert_eq!(row.n_atoms, Some(2));
        assert_eq!(row.composition.as_deref(), Some("Al1 Ti1"));
        assert_eq!(row.source_display(), "user_import");
        assert_eq!(row.cache_ref_display(), "cache://ab12/structure.cif");
    }

    #[test]
    fn legacy_tool_name_spelling_is_accepted_verbatim() {
        let row = WorkspaceStructure::from_value(&json!({
            "cache_key": "cd34",
            "tool_name": "mace_relax",
        }))
        .expect("valid row");
        assert_eq!(row.tool.as_deref(), Some("mace_relax"));
    }

    #[test]
    fn missing_meta_fields_are_unknown_not_invented() {
        let row = WorkspaceStructure::from_value(&json!({"cache_key": "ef56"}))
            .expect("cache_key alone is a valid row");

        assert_eq!(row.formula, None);
        assert_eq!(row.formula_display(), "unknown");
        assert_eq!(row.n_atoms, None);
        assert_eq!(row.composition, None);
        assert_eq!(row.source_display(), "unknown");
        assert_eq!(row.cache_ref_display(), "unknown");
    }

    #[test]
    fn wrong_typed_fields_are_unknown_not_invented() {
        let row = WorkspaceStructure::from_value(&json!({
            "cache_key": "99aa",
            "formula": 42,
            "n_atoms": "many",
            "composition": "TiAl",
            "source": "   "
        }))
        .expect("wrong types degrade to unknown");

        assert_eq!(row.formula, None);
        assert_eq!(row.n_atoms, None);
        assert_eq!(row.composition, None);
        assert_eq!(row.source, None);
    }

    #[test]
    fn partial_composition_is_not_half_invented() {
        let row = WorkspaceStructure::from_value(&json!({
            "cache_key": "bb77",
            "composition": {"Ti": 1, "Al": "??"}
        }))
        .expect("row itself stays valid");
        assert_eq!(row.composition, None);
    }

    #[test]
    fn missing_cache_key_is_an_error_not_a_row() {
        let error = WorkspaceStructure::from_value(&json!({"formula": "TiAl"}))
            .expect_err("identity is required");
        assert!(error.contains("cache_key"), "{error}");

        let error = WorkspaceStructure::from_value(&json!({"cache_key": "   "}))
            .expect_err("blank identity is rejected");
        assert_eq!(error, "structure row `cache_key` is empty");
    }

    #[test]
    fn control_chars_in_cache_key_are_rejected() {
        let error = WorkspaceStructure::from_value(&json!({"cache_key": "ab\x1b[31m12"}))
            .expect_err("terminal controls must not become a fetch key");
        assert!(error.contains("terminal controls"), "{error}");
    }

    #[test]
    fn unknown_source_is_carried_verbatim() {
        let row = WorkspaceStructure::from_value(&json!({
            "cache_key": "cc88",
            "source": "some_future_importer"
        }))
        .expect("valid row");
        assert_eq!(row.source_display(), "some_future_importer");
    }

    #[test]
    fn cif_body_within_budget_is_verbatim() {
        let cif = "data_TiAl\n_cell_length_a 4.0\n";
        assert_eq!(format_cif_body(cif, 4096, false), cif);
    }

    #[test]
    fn cif_body_is_bounded_with_an_explicit_banner() {
        let rendered = format_cif_body("abcdefghij", 4, false);
        assert!(rendered.starts_with("abcd"));
        assert!(rendered.contains("CIF display truncated at 4 B by policy"));
        assert!(!rendered.contains("efghij"));
    }

    #[test]
    fn backend_truncation_flag_is_surfaced() {
        let rendered = format_cif_body("short", 4096, true);
        assert!(rendered.contains("CIF display truncated"));
    }

    #[test]
    fn cif_bound_never_splits_a_codepoint() {
        let rendered = format_cif_body("αβγδ", 5, false);
        assert!(rendered.starts_with('α'));
        assert!(rendered.contains("truncated"));
    }
}
