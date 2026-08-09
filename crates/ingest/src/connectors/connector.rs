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
//!
//! Swapping a built-in for your own implementation is [`replace_connector`].
//! The two-call contract shared by every adapter plane: `register` refuses a
//! taken id AND a taken extension claim (an accidental collision fails
//! loudly; nothing silently wins a route), `replace` refuses a free id (a
//! typo cannot silently ADD while the connector you meant to displace keeps
//! running).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, RwLock, RwLockReadGuard};

use anyhow::{Result, bail};
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

/// A connector's declaration — id and extension claims — captured by calling
/// the trait methods ONCE, outside any lock. Connector-supplied code never
/// runs while the process-wide registry lock is held, so a connector whose
/// `id()`/`extensions()` re-enters the registry (reads it, or registers
/// another connector) cannot deadlock registration.
#[derive(Clone, Copy)]
struct Declaration {
    id: &'static str,
    extensions: &'static [&'static str],
}

impl Declaration {
    /// Capture and validate a declaration: a non-empty id and at least one
    /// extension claim, each claim non-empty, lowercase, without the dot
    /// (the [`Connector::extensions`] contract), and not repeated. Refused
    /// loudly at registration so a malformed connector never becomes an
    /// entry that path matching cannot route to.
    fn of(connector: &dyn Connector) -> Result<Self> {
        let decl = Self {
            id: connector.id(),
            extensions: connector.extensions(),
        };
        if decl.id.trim().is_empty() {
            bail!("connector id must be non-empty");
        }
        if decl.extensions.is_empty() {
            bail!("connector '{}' claims no extensions", decl.id);
        }
        for (i, ext) in decl.extensions.iter().enumerate() {
            if ext.is_empty() || ext.contains('.') || ext.chars().any(|c| c.is_ascii_uppercase()) {
                bail!(
                    "connector '{}' extension claim '{ext}' must be non-empty, \
                     lowercase, and without the dot",
                    decl.id
                );
            }
            if decl.extensions[..i].contains(ext) {
                bail!("connector '{}' claims extension '{ext}' twice", decl.id);
            }
        }
        Ok(decl)
    }
}

/// Ordered registry of connectors — the ONE place the extension→connector
/// decision is made. Iteration order is registration order, which keeps
/// derived lists (pickers, error messages) deterministic.
///
/// Invariant: every registered connector owns EVERY extension it declares.
/// `register` refuses id and extension collisions, and `replace` refuses to
/// steal another connector's claims, so `get(id)` can never return a
/// connector whose advertised extensions route elsewhere.
pub struct ConnectorRegistry {
    connectors: Vec<Arc<dyn Connector>>,
    /// Captured declarations, parallel to `connectors`. All bookkeeping
    /// reads these — never the trait methods — so no connector code runs
    /// under the process-wide registry lock.
    decls: Vec<Declaration>,
    by_id: HashMap<&'static str, usize>,
    by_extension: HashMap<&'static str, usize>,
}

impl ConnectorRegistry {
    pub fn new() -> Self {
        Self {
            connectors: Vec::new(),
            decls: Vec::new(),
            by_id: HashMap::new(),
            by_extension: HashMap::new(),
        }
    }

    /// The built-in file connectors, in canonical order.
    pub fn builtin() -> Self {
        let mut reg = Self::new();
        reg.register(Arc::new(CsvConnector))
            .expect("built-in connector declarations are valid and unique");
        reg.register(Arc::new(ParquetConnector))
            .expect("built-in connector declarations are valid and unique");
        reg
    }

    /// Add a connector under a FREE id with FREE extension claims.
    ///
    /// The two-call contract shared by every adapter plane: `register`
    /// refuses a taken id AND a taken extension claim — an accidental
    /// collision must fail loudly, and silently shadowing another
    /// connector's route would be a partial, invisible replacement.
    /// Taking over a registered connector is a deliberate act with its
    /// own call: [`ConnectorRegistry::replace`]. Malformed declarations
    /// (see [`Declaration::of`]) are refused the same way. On refusal
    /// nothing changes.
    pub fn register(&mut self, connector: Arc<dyn Connector>) -> Result<()> {
        let decl = Declaration::of(connector.as_ref())?;
        self.insert_new(decl, connector)
    }

    /// Deliberately swap the connector registered under the SAME id.
    ///
    /// Strict both ways: the id must be taken (a typo'd id cannot silently
    /// ADD a connector while the one you meant to displace keeps running),
    /// and the replacement may claim only extensions that are free or
    /// currently owned by the id being replaced (replacing "csv" must not
    /// silently steal another connector's route — replace that connector
    /// separately). ALL of the displaced version's claims are dropped, so
    /// a replacement that narrows its extensions leaves no stale routes.
    ///
    /// Returns the displaced connector — hand it back to this function to
    /// restore the original — and logs what was displaced.
    pub fn replace(&mut self, connector: Arc<dyn Connector>) -> Result<Arc<dyn Connector>> {
        let decl = Declaration::of(connector.as_ref())?;
        let displaced = self.swap(decl, connector)?;
        tracing::info!(id = decl.id, "connector deliberately replaced");
        Ok(displaced)
    }

    /// Registration body. Uses only the captured `decl` — no connector
    /// trait calls — so it is safe to run under the process-wide write
    /// lock (see [`register_connector`]).
    fn insert_new(&mut self, decl: Declaration, connector: Arc<dyn Connector>) -> Result<()> {
        if self.by_id.contains_key(decl.id) {
            bail!(
                "connector id '{}' is already registered; swap it deliberately \
                 with ConnectorRegistry::replace (replace_connector for the \
                 process-wide registry)",
                decl.id
            );
        }
        for ext in decl.extensions {
            if let Some(&owner) = self.by_extension.get(ext) {
                bail!(
                    "connector '{}' claims extension '{ext}', which is already \
                     claimed by connector '{}'; replace that connector to take \
                     over its routes",
                    decl.id,
                    self.decls[owner].id
                );
            }
        }
        let idx = self.connectors.len();
        self.by_id.insert(decl.id, idx);
        for ext in decl.extensions {
            self.by_extension.insert(ext, idx);
        }
        self.connectors.push(connector);
        self.decls.push(decl);
        Ok(())
    }

    /// Replacement body. Uses only the captured `decl` — no connector
    /// trait calls — so it is safe to run under the process-wide write
    /// lock (see [`replace_connector`]).
    fn swap(
        &mut self,
        decl: Declaration,
        connector: Arc<dyn Connector>,
    ) -> Result<Arc<dyn Connector>> {
        let Some(&idx) = self.by_id.get(decl.id) else {
            bail!(
                "no connector '{}' registered to replace; add it with \
                 ConnectorRegistry::register (register_connector for the \
                 process-wide registry)",
                decl.id
            );
        };
        for ext in decl.extensions {
            if let Some(&owner) = self.by_extension.get(ext)
                && owner != idx
            {
                bail!(
                    "replacement connector '{}' claims extension '{ext}', \
                     which is owned by connector '{}'; replace that \
                     connector separately",
                    decl.id,
                    self.decls[owner].id
                );
            }
        }
        // Drop the displaced version's claims entirely — a replacement
        // that narrows its extensions must not leave stale routes behind.
        self.by_extension.retain(|_, i| *i != idx);
        for ext in decl.extensions {
            self.by_extension.insert(ext, idx);
        }
        self.decls[idx] = decl;
        Ok(std::mem::replace(&mut self.connectors[idx], connector))
    }

    /// The connector claiming `path`'s extension (matched case-insensitively),
    /// or `None` when no registered connector parses this format.
    pub fn connector_for(&self, path: &Path) -> Option<Arc<dyn Connector>> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        self.by_extension
            .get(ext.as_str())
            .map(|&idx| self.connectors[idx].clone())
    }

    /// Whether any registered connector claims `path`.
    pub fn claims(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                self.by_extension
                    .contains_key(ext.to_ascii_lowercase().as_str())
            })
    }

    /// Look up a connector by its stable id.
    pub fn get(&self, id: &str) -> Option<Arc<dyn Connector>> {
        self.by_id.get(id).map(|&idx| self.connectors[idx].clone())
    }

    /// Every claimed extension, in registration order. Collisions are
    /// refused at registration and replacement never steals claims, so
    /// every listed extension RESOLVES to its claiming connector — this
    /// list feeds the picker text and the unsupported-format error, which
    /// must never advertise a route [`ConnectorRegistry::connector_for`]
    /// will not take.
    pub fn extensions(&self) -> Vec<&'static str> {
        self.decls
            .iter()
            .flat_map(|decl| decl.extensions.iter().copied())
            .collect()
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

/// The process-wide registry: starts as [`ConnectorRegistry::builtin`] and
/// is extendable at runtime through [`register_connector`]. Every site that
/// asks "is this file ours?" (pipeline dispatch, CLI backend routing, the
/// TUI picker, node dataset discovery) reads it through [`registry`].
static REGISTRY: LazyLock<RwLock<ConnectorRegistry>> =
    LazyLock::new(|| RwLock::new(ConnectorRegistry::builtin()));

/// Serialises every test IN THIS BINARY that mutates a process-wide
/// registry (this connector registry AND `crate::ontologies`' registry) or
/// reads one around another test's mutation window (`cargo test` runs a
/// binary's tests on concurrent threads). ONE home, on purpose: per-test
/// (or per-registry) copies of a lock serialise nothing, and one lock for
/// both registries removes any lock-ordering question. Scope is exactly
/// this binary — other crates' test binaries are separate processes with
/// their own `REGISTRY`, so they cannot race this one. Async-aware because
/// the guard is deliberately held across the `ingest_file` await.
#[cfg(test)]
pub(crate) static GLOBAL_REGISTRY_TEST_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

/// Read access to the process-wide registry. Hold the guard only for the
/// query — never across an `.await` (the guard is not `Send`) and never
/// while calling [`register_connector`] on the same thread (write-lock
/// deadlock).
pub fn registry() -> RwLockReadGuard<'static, ConnectorRegistry> {
    REGISTRY.read().expect("connector registry lock poisoned")
}

/// Register a connector in the process-wide registry — the explicit
/// registration call that makes a NEW format live on every surface that
/// consults [`registry`], with no match arm, enum variant, `builtin()`
/// edit, or consumer-list edit. Refusal semantics are
/// [`ConnectorRegistry::register`]'s: malformed declarations, a taken id,
/// and a taken extension claim are all `Err` — swapping a registered
/// connector is [`replace_connector`].
///
/// The declaration is captured (the connector's trait methods are called)
/// BEFORE the write lock is taken: connector-supplied code that re-enters
/// the registry cannot deadlock registration, and a refused registration
/// never poisons the shared registry.
pub fn register_connector(connector: Arc<dyn Connector>) -> Result<()> {
    let decl = Declaration::of(connector.as_ref())?;
    REGISTRY
        .write()
        .expect("connector registry lock poisoned")
        .insert_new(decl, connector)
}

/// Deliberately swap a connector registered in the process-wide registry —
/// how a caller takes over a built-in format ("csv", "parquet") with their
/// own implementation. Semantics are [`ConnectorRegistry::replace`]'s:
/// the id must already be registered, and the replacement may not claim
/// another connector's extensions. Returns the displaced connector — hand
/// it back to this function to restore the original — and logs what was
/// displaced.
///
/// Like [`register_connector`], the declaration is captured before the
/// write lock is taken, so connector-supplied code cannot deadlock the
/// registry.
pub fn replace_connector(connector: Arc<dyn Connector>) -> Result<Arc<dyn Connector>> {
    let decl = Declaration::of(connector.as_ref())?;
    let displaced = REGISTRY
        .write()
        .expect("connector registry lock poisoned")
        .swap(decl, connector)?;
    tracing::info!(
        id = decl.id,
        "connector deliberately replaced in the process-wide registry"
    );
    Ok(displaced)
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

    /// The PROPERTY the routing surfaces depend on, not a frozen list of
    /// today's built-ins (which reddened the suite whenever a connector was
    /// added): every claim of every registered connector routes back to its
    /// claimant, case-insensitively; `extensions()` advertises exactly the
    /// claims in registration order; and unclaimed names are refused under
    /// `Path::extension` semantics.
    #[test]
    fn every_builtin_claim_routes_to_its_claiming_connector() {
        let mut reg = ConnectorRegistry::builtin();
        // Novel runtime registration BEFORE the property loops: its claims
        // are data no frozen list or hardcoded routing table can contain,
        // so the assertions below cannot be satisfied by an implementation
        // that ignores registrations — the falsifiability the old
        // hardcoded-list test lacked.
        reg.register(Arc::new(Fake {
            id: "zzz-novel",
            exts: &["zzz1", "zzz2"],
        }))
        .expect("a novel connector must register");

        let claimed = reg.extensions();
        assert!(
            claimed.ends_with(&["zzz1", "zzz2"]),
            "runtime-registered claims must be advertised, in order: {claimed:?}",
        );

        // extensions() is exactly the connectors' claims, in registration
        // order (collisions are refused, so no dedupe can hide anything).
        // With the novel connector in the mix this cross-checks the STORED
        // declarations against the live trait declarations — a capture bug
        // (reorder, drop, duplicate) fails here.
        let mut expected: Vec<&str> = Vec::new();
        for connector in reg.all() {
            expected.extend(connector.extensions());
        }
        assert_eq!(claimed, expected);

        for connector in reg.all() {
            for ext in connector.extensions() {
                // Lowercase and UPPERCASE forms both route to the claimant.
                for name in [
                    format!("a.{ext}"),
                    format!("A.{}", ext.to_ascii_uppercase()),
                ] {
                    let path_owned = Path::new(&name);
                    assert!(reg.claims(path_owned), "{name}");
                    assert_eq!(
                        reg.connector_for(path_owned).map(|c| c.id()),
                        Some(connector.id()),
                        "{name} must route to its claiming connector",
                    );
                }
                // Path::extension semantics, exactly what the pipeline
                // rejects on: a file NAMED like a format has no extension,
                // and a dotfile's leading dot is not a separator.
                assert!(!reg.claims(Path::new(ext)), "bare '{ext}'");
                let dotfile = format!(".{ext}");
                assert!(!reg.claims(Path::new(&dotfile)), "{dotfile}");
            }
        }

        // An extension nothing claims is refused — derived, not frozen.
        assert!(!claimed.contains(&"zzz_unclaimed"));
        assert!(reg.connector_for(Path::new("a.zzz_unclaimed")).is_none());
        assert!(!reg.claims(Path::new("a.zzz_unclaimed")));
        assert!(reg.connector_for(Path::new("no_extension")).is_none());
    }

    /// Same property for id lookup: every registered connector resolves by
    /// its own id, and an id nobody registered resolves to nothing.
    #[test]
    fn lookup_by_id_returns_the_right_adapter() {
        let reg = ConnectorRegistry::builtin();
        for connector in reg.all() {
            assert_eq!(
                reg.get(connector.id()).map(|c| c.id()),
                Some(connector.id()),
            );
        }
        assert!(reg.get("zzz_unregistered").is_none());
    }

    /// Deliberate replacement must leave ONE coherent truth: id lookup
    /// returns the replacement, claims the replacement dropped stop
    /// routing, `extensions()` carries no stale or duplicate entries, and
    /// the displaced connector comes back for the restore path.
    #[test]
    fn replace_swaps_claims_coherently_and_returns_the_displaced() {
        let mut reg = ConnectorRegistry::builtin();
        let count_before = reg.all().len();

        // V2 of the csv connector no longer claims tsv.
        let displaced = reg
            .replace(Arc::new(Fake {
                id: "csv",
                exts: &["csv"],
            }))
            .expect("a registered id must be replaceable");
        assert_eq!(displaced.id(), "csv");
        assert!(
            displaced.extensions().contains(&"tsv"),
            "we displaced the genuine built-in (the one that claimed tsv)",
        );

        // Id lookup and extension lookup agree: both return V2
        // (distinguished by its narrowed declaration).
        assert_eq!(
            reg.get("csv").expect("csv registered").extensions(),
            ["csv"],
            "get() must return the replacement, not the displaced version",
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
        assert!(!reg.extensions().contains(&"tsv"));
        assert_eq!(
            reg.all().len(),
            count_before,
            "replaced in place, not appended"
        );
    }

    /// The loud half of the two-call contract: `register` refuses a taken
    /// id AND a taken extension claim — the old silent later-wins
    /// shadowing is gone — and a refusal changes nothing.
    #[test]
    fn register_refuses_taken_id_and_taken_extension() {
        let mut reg = ConnectorRegistry::builtin();
        let extensions_before = reg.extensions();

        // Same id: accidental collision, refused loudly, deliberate path named.
        let err = reg
            .register(Arc::new(Fake {
                id: "csv",
                exts: &["mycsv"],
            }))
            .expect_err("a taken id must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("already registered"), "{msg}");
        assert!(msg.contains("replace_connector"), "{msg}");

        // Different id, taken extension: refused — a silent shadow would be
        // a partial, invisible replacement of the claim's owner.
        let err = reg
            .register(Arc::new(Fake {
                id: "better-csv",
                exts: &["csv"],
            }))
            .expect_err("a taken extension claim must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("'csv'"), "{msg}");

        // Nothing changed: no entry, no rerouted claim.
        assert_eq!(reg.extensions(), extensions_before);
        assert!(reg.get("better-csv").is_none());
        assert_eq!(
            reg.connector_for(Path::new("x.csv")).map(|c| c.id()),
            Some("csv"),
        );
    }

    /// The strict half: `replace` refuses a FREE id (a typo'd id cannot
    /// silently ADD while the connector the caller meant to displace keeps
    /// running) and refuses to steal another connector's extension claims.
    #[test]
    fn replace_refuses_a_free_id_and_anothers_claims() {
        let mut reg = ConnectorRegistry::builtin();

        let err = match reg.replace(Arc::new(Fake {
            id: "csvv",
            exts: &["csv"],
        })) {
            Err(e) => e,
            Ok(_) => panic!("replacing an unregistered id must be refused"),
        };
        let msg = format!("{err:#}");
        assert!(msg.contains("no connector 'csvv'"), "{msg}");

        // The replacement for "csv" tries to also grab parquet's route.
        let parquet_owner = reg
            .connector_for(Path::new("x.parquet"))
            .expect("parquet claimed")
            .id();
        let err = match reg.replace(Arc::new(Fake {
            id: "csv",
            exts: &["csv", "parquet"],
        })) {
            Err(e) => e,
            Ok(_) => panic!("stealing another connector's claim must be refused"),
        };
        let msg = format!("{err:#}");
        assert!(msg.contains("parquet"), "{msg}");

        // The failed replace changed nothing.
        assert_eq!(
            reg.connector_for(Path::new("x.parquet")).map(|c| c.id()),
            Some(parquet_owner),
        );
        assert_eq!(
            reg.connector_for(Path::new("x.csv")).map(|c| c.id()),
            Some("csv"),
        );
        assert!(reg.claims(Path::new("x.tsv")), "csv keeps its full claims");
    }

    /// The documented refusal behaviour: malformed declarations are
    /// refused loudly at registration time, and leave nothing behind.
    #[test]
    fn malformed_declarations_are_refused() {
        let mut reg = ConnectorRegistry::new();
        let malformed: &[(&'static str, &'static [&'static str])] = &[
            ("bad-dotted", &[".csv"]),
            ("bad-uppercase", &["CSV"]),
            ("bad-empty-claim", &[""]),
            ("bad-no-claims", &[]),
            ("", &["ok"]),
            ("bad-duplicate-claim", &["a", "a"]),
        ];
        for &(id, exts) in malformed {
            assert!(
                reg.register(Arc::new(Fake { id, exts })).is_err(),
                "declaration id={id:?} exts={exts:?} must be refused",
            );
        }
        assert!(
            reg.all().is_empty(),
            "refused registrations must leave nothing behind",
        );
    }
}
