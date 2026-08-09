//! The file-connector surface.
//!
//! A file format is a plugin implementing [`Connector`]: a stable id, the
//! extensions it claims, and two loaders producing what every connector
//! produces — the whole-file [`DataFrame`] the ingest pipeline runs on and a
//! [`DataSource`] describing where it came from. The pipeline and every
//! ingest-adjacent surface (CLI backend routing, node dataset discovery, the
//! TUI file picker) answer "is this file ours?" **only** through
//! [`ConnectorRegistry`] dispatch — there is no extension `match` anywhere on
//! the ingest path. Adding a format means writing one module that implements
//! [`Connector`], then ONE registration call: [`register_connector`] into the
//! process-wide registry at runtime (built-ins use a `register(...)` line in
//! [`ConnectorRegistry::builtin`] instead). No match arm, enum variant, or
//! consumer list is edited. (The new module still needs its `mod` declaration
//! to be compiled in — a compilation-unit fact of Rust, not dispatch.)

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, RwLock, RwLockReadGuard};

use anyhow::Result;
use polars::prelude::DataFrame;

use crate::DataSource;

use super::{CsvConnector, ParquetConnector};

/// One file-format connector. The contract the pipeline depends on;
/// everything else is an implementation detail of the adapter.
///
/// What a connector produces is part of the contract: [`Connector::load`]
/// yields the tabular [`DataFrame`] payload `IngestPipeline` consumes, and
/// [`Connector::to_data_source`] yields the provenance descriptor recorded
/// with the ingest.
pub trait Connector: Send + Sync {
    /// Stable machine id, e.g. `"csv"`. Registry lookup key — not the file's
    /// recorded format (that is `DataSource::format`, which reports the
    /// extension actually seen, so a `.tsv` ingest is never recorded "csv").
    fn id(&self) -> &'static str;

    /// The extensions this connector claims, lowercase and without the dot,
    /// e.g. `["csv", "tsv"]`. Matching against paths is the registry's job
    /// and is case-insensitive.
    fn extensions(&self) -> &'static [&'static str];

    /// Load the whole file into the tabular payload the pipeline consumes.
    fn load(&self, path: &Path) -> Result<DataFrame>;

    /// Describe the file as a [`DataSource`] (canonical path + actual format).
    fn to_data_source(&self, path: &Path) -> Result<DataSource>;
}

/// Ordered registry of connectors — the ONE place the extension→connector
/// decision is made. Iteration order is registration order, which keeps
/// derived lists (pickers, error messages) deterministic.
pub struct ConnectorRegistry {
    connectors: Vec<Arc<dyn Connector>>,
    by_extension: HashMap<String, usize>,
}

impl ConnectorRegistry {
    pub fn new() -> Self {
        Self {
            connectors: Vec::new(),
            by_extension: HashMap::new(),
        }
    }

    /// The built-in file connectors, in canonical order.
    pub fn builtin() -> Self {
        let mut reg = Self::new();
        reg.register(Arc::new(CsvConnector));
        reg.register(Arc::new(ParquetConnector));
        reg
    }

    /// Add a connector, refusing malformed declarations (see
    /// [`validate_declaration`]).
    ///
    /// Replacement is by id and coherent — the same resolution the
    /// retrieval `SourceRegistry` uses: re-registering an id swaps the
    /// connector in place and drops ALL of the old version's extension
    /// claims, so id lookup, extension lookup, and
    /// [`ConnectorRegistry::extensions`] can never disagree about which
    /// connector owns a format. Across DIFFERENT ids, later registrations
    /// win on extension collision (shadowing the earlier claim) so a caller
    /// can override a built-in without rebuilding the whole registry.
    ///
    /// # Panics
    /// If the declaration is malformed: empty id, no extension claims, or a
    /// claim that is empty, dotted, or not lowercase.
    pub fn register(&mut self, connector: Arc<dyn Connector>) {
        if let Err(why) = validate_declaration(connector.as_ref()) {
            panic!("refusing connector registration: {why}");
        }
        let idx = match self
            .connectors
            .iter()
            .position(|c| c.id() == connector.id())
        {
            Some(idx) => {
                // Same id: drop the old claims entirely — a replacement that
                // narrows its extensions must not leave stale routes behind.
                self.by_extension.retain(|_, i| *i != idx);
                self.connectors[idx] = connector;
                idx
            }
            None => {
                self.connectors.push(connector);
                self.connectors.len() - 1
            }
        };
        for ext in self.connectors[idx].extensions() {
            self.by_extension.insert((*ext).to_string(), idx);
        }
    }

    /// The connector claiming `path`'s extension (matched case-insensitively),
    /// or `None` when no registered connector parses this format.
    pub fn connector_for(&self, path: &Path) -> Option<Arc<dyn Connector>> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        self.by_extension
            .get(&ext)
            .map(|&idx| self.connectors[idx].clone())
    }

    /// Whether any registered connector claims `path`.
    pub fn claims(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| self.by_extension.contains_key(&ext.to_ascii_lowercase()))
    }

    /// Look up a connector by its stable id.
    pub fn get(&self, id: &str) -> Option<Arc<dyn Connector>> {
        self.connectors.iter().find(|c| c.id() == id).cloned()
    }

    /// Every claimed extension that currently RESOLVES to its claiming
    /// connector, in registration order and without duplicates. A claim
    /// shadowed by a later registration is omitted: this list feeds the
    /// picker text and the unsupported-format error, which must never
    /// advertise a route [`ConnectorRegistry::connector_for`] will not take.
    pub fn extensions(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for (idx, connector) in self.connectors.iter().enumerate() {
            for ext in connector.extensions() {
                if self.by_extension.get(*ext).copied() == Some(idx) && !out.contains(ext) {
                    out.push(ext);
                }
            }
        }
        out
    }

    /// All registered connectors, in registration order.
    pub fn all(&self) -> &[Arc<dyn Connector>] {
        &self.connectors
    }
}

impl Default for ConnectorRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

/// Why a connector declaration is acceptable: a non-empty id and at least
/// one extension claim, each claim non-empty, lowercase, and without the
/// dot (the [`Connector::extensions`] contract). Enforced at registration
/// so a malformed connector is refused loudly instead of becoming an entry
/// that path matching can never route to.
fn validate_declaration(connector: &dyn Connector) -> Result<(), String> {
    let id = connector.id();
    if id.trim().is_empty() {
        return Err("connector id must be non-empty".into());
    }
    let extensions = connector.extensions();
    if extensions.is_empty() {
        return Err(format!("connector '{id}' claims no extensions"));
    }
    for ext in extensions {
        if ext.is_empty() || ext.contains('.') || ext.chars().any(|c| c.is_ascii_uppercase()) {
            return Err(format!(
                "connector '{id}' extension claim '{ext}' must be non-empty, \
                 lowercase, and without the dot"
            ));
        }
    }
    Ok(())
}

/// The process-wide registry: starts as [`ConnectorRegistry::builtin`] and
/// is extendable at runtime through [`register_connector`]. Every site that
/// asks "is this file ours?" (pipeline dispatch, CLI backend routing, the
/// TUI picker, node dataset discovery) reads it through [`registry`].
static REGISTRY: LazyLock<RwLock<ConnectorRegistry>> =
    LazyLock::new(|| RwLock::new(ConnectorRegistry::builtin()));

/// Read access to the process-wide registry. Hold the guard only for the
/// query — never across an `.await` (the guard is not `Send`) and never
/// while calling [`register_connector`] on the same thread (write-lock
/// deadlock).
pub fn registry() -> RwLockReadGuard<'static, ConnectorRegistry> {
    REGISTRY.read().expect("connector registry lock poisoned")
}

/// Register a connector in the process-wide registry — the explicit
/// registration call that makes a new format live on every surface that
/// consults [`registry`], with no match arm, enum variant, `builtin()`
/// edit, or consumer-list edit. Replacement and shadowing semantics are
/// [`ConnectorRegistry::register`]'s.
///
/// # Panics
/// Refuses malformed declarations (see [`ConnectorRegistry::register`]).
/// Validation runs BEFORE the write lock is taken, so a refused
/// registration never poisons the shared registry.
pub fn register_connector(connector: Arc<dyn Connector>) {
    if let Err(why) = validate_declaration(connector.as_ref()) {
        panic!("refusing connector registration: {why}");
    }
    REGISTRY
        .write()
        .expect("connector registry lock poisoned")
        .register(connector);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test double with a configurable declaration.
    struct Fake {
        id: &'static str,
        exts: &'static [&'static str],
    }

    impl Connector for Fake {
        fn id(&self) -> &'static str {
            self.id
        }
        fn extensions(&self) -> &'static [&'static str] {
            self.exts
        }
        fn load(&self, _: &Path) -> Result<DataFrame> {
            Ok(DataFrame::empty())
        }
        fn to_data_source(&self, path: &Path) -> Result<DataSource> {
            Ok(DataSource {
                path: path.display().to_string(),
                format: self.id.into(),
            })
        }
    }

    #[test]
    fn builtin_registry_claims_exactly_the_connector_extensions() {
        let reg = ConnectorRegistry::builtin();
        assert_eq!(reg.extensions(), ["csv", "tsv", "parquet", "pq"]);
        for (file, id) in [
            ("a.csv", "csv"),
            ("a.tsv", "csv"),
            ("A.TSV", "csv"),
            ("a.parquet", "parquet"),
            ("a.pq", "parquet"),
        ] {
            assert_eq!(
                reg.connector_for(Path::new(file)).map(|c| c.id()),
                Some(id),
                "{file}",
            );
            assert!(reg.claims(Path::new(file)), "{file}");
        }
        assert!(reg.connector_for(Path::new("a.xlsx")).is_none());
        assert!(!reg.claims(Path::new("a.xlsx")));
        assert!(reg.connector_for(Path::new("no_extension")).is_none());
        // Path::extension semantics, exactly what the pipeline rejects on:
        // an extensionless name that LOOKS like a format is not claimed,
        // and a dotfile has no extension at all.
        assert!(!reg.claims(Path::new("parquet")));
        assert!(!reg.claims(Path::new(".tsv")));
    }

    #[test]
    fn lookup_by_id_returns_the_right_adapter() {
        let reg = ConnectorRegistry::builtin();
        assert_eq!(reg.get("parquet").map(|c| c.id()), Some("parquet"));
        assert!(reg.get("xlsx").is_none());
    }

    /// Re-registering an id must leave ONE coherent truth: id lookup
    /// returns the replacement, claims the replacement dropped stop
    /// routing, and `extensions()` carries no stale or duplicate entries.
    /// The first cut kept appending to `connectors`, so `get("csv")`
    /// returned V1 while `connector_for("x.csv")` returned V2, `x.tsv`
    /// still routed to a connector that no longer claimed it, and
    /// `extensions()` advertised both generations.
    #[test]
    fn reregistering_an_id_replaces_claims_coherently() {
        let mut reg = ConnectorRegistry::builtin();
        // V2 of the csv connector no longer claims tsv.
        reg.register(Arc::new(Fake {
            id: "csv",
            exts: &["csv"],
        }));

        // Id lookup and extension lookup agree: both return V2
        // (distinguished by its narrowed declaration).
        assert_eq!(
            reg.get("csv").expect("csv registered").extensions(),
            ["csv"],
            "get() must return the replacement, not the first registration",
        );
        assert_eq!(
            reg.connector_for(Path::new("x.csv"))
                .expect("csv routed")
                .extensions(),
            ["csv"],
        );
        // The dropped claim stops routing — no stale route to V1.
        assert!(reg.connector_for(Path::new("x.tsv")).is_none());
        assert!(!reg.claims(Path::new("x.tsv")));
        // Derived lists carry no historical or duplicate claims.
        assert_eq!(reg.extensions(), ["csv", "parquet", "pq"]);
        assert_eq!(reg.all().len(), 2, "replaced in place, not appended");
    }

    /// Pins the collision rule across DIFFERENT ids: the later registration
    /// takes the contested extension, the earlier keeps its other claims,
    /// and `extensions()` lists the shadowed claim exactly once — under the
    /// connector that now owns it.
    #[test]
    fn later_registration_shadows_an_extension_claim() {
        let mut reg = ConnectorRegistry::builtin();
        reg.register(Arc::new(Fake {
            id: "better-csv",
            exts: &["csv"],
        }));

        let id_for = |p: &str| reg.connector_for(Path::new(p)).map(|c| c.id());
        assert_eq!(id_for("x.csv"), Some("better-csv"));
        assert_eq!(id_for("x.tsv"), Some("csv"), "uncontested claim survives");
        assert_eq!(
            reg.extensions(),
            ["tsv", "parquet", "pq", "csv"],
            "the shadowed csv claim must appear once, under its new owner",
        );
    }

    /// The documented refusal behaviour: a malformed declaration (here, a
    /// dotted extension claim) is refused loudly at registration time.
    #[test]
    #[should_panic(expected = "refusing connector registration")]
    fn malformed_extension_claim_is_refused() {
        let mut reg = ConnectorRegistry::new();
        reg.register(Arc::new(Fake {
            id: "bad",
            exts: &[".csv"],
        }));
    }
}
