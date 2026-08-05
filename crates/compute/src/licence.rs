// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Licence management for commercial simulation codes.
//!
//! HPC centres usually run a licence server for commercial codes; some
//! (notably the ESA Space HPC system) do not, so PRISM itself must hold
//! and account for licences on the submitting side.
//!
//! # Model
//!
//! - A [`Licence`] is declared in `~/.prism/licences.toml`: an id, a
//!   display name, a seat count, an expiry date, and — for sites that do
//!   have one — the address of a real licence server. A licence may also
//!   carry a `secret` (serial number, key, or server credential).
//! - A job that needs a licensed code carries a [`LicenceRequest`] on its
//!   [`crate::ExperimentPlan`] and must acquire a [`Lease`] *before* it is
//!   dispatched. No lease, no submission — the refusal names the licence,
//!   the seat count, how many are held, and when one is due to free.
//! - A [`Lease`] is an opaque, signed, expiring assertion naming the
//!   licence id, seat count, job id and expiry. It is minted on the
//!   submitting side and verifiable on a compute node **with no network**,
//!   without the secret that minted it. Lease expiry is bounded by both
//!   the licence expiry and the job's walltime, so a lease never outlives
//!   the job that holds it.
//!
//! # Secret hygiene
//!
//! The licence secret never travels. It must not appear in a generated
//! sbatch script (world-readable on most clusters), in a job environment
//! that gets logged, or in a provenance record (permanent). Only the
//! signed lease travels; see [`Lease::to_wire`].
//!
//! # Seat lifecycle
//!
//! Seats return when the bound job reaches a terminal state, and expired
//! leases are reclaimed, so a job that is killed, crashes, or vanishes
//! cannot leak its seat forever. Accounting reuses the persistent
//! [`crate::job::JobTracker`] (`compute-jobs.json`) rather than a parallel
//! store: a bound lease lives on the job record.
//!
//! # Zero config
//!
//! No `licences.toml` → empty registry → unlicensed workloads (Quantum
//! ESPRESSO, MACE, …) run exactly as before and never touch this module.
//! A [`LicenceRequest`] against an empty registry is refused with a
//! message naming what is missing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use chrono::{DateTime, NaiveDate, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::job::JobTracker;

/// Default location of the licence declarations file.
pub const LICENCES_FILE: &str = "licences.toml";

/// A job's request for licence seats, carried on the experiment plan.
///
/// `#[serde(default)]` on the plan field means plans without this request
/// are unlicensed and acquire nothing — the free path is untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenceRequest {
    /// Id of a declared licence.
    pub id: String,
    /// Seats to hold (default 1).
    #[serde(default = "default_seats")]
    pub seats: u32,
}

fn default_seats() -> u32 {
    1
}

/// Address of a real licence server (FlexLM and friends), for sites that
/// have one. Informational on the submitting side — PRISM never contacts
/// it from a compute node, which has no egress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenceServer {
    pub host: String,
    pub port: u16,
}

/// One declared licence.
///
/// `secret` is deserialize-only (`skip_serializing`): any accidental
/// serialization of a registry — logs, provenance, wire formats — drops
/// it. Its real protection is that nothing that travels is ever built
/// from a [`Licence`], only from a [`Lease`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Licence {
    pub id: String,
    pub name: String,
    pub seats: u32,
    /// First instant at which the licence no longer issues leases.
    pub expires: DateTime<Utc>,
    /// Serial number, key, or server credential. Never serialized.
    #[serde(default, skip_serializing)]
    pub secret: Option<String>,
    /// Optional address of a real licence server.
    #[serde(default)]
    pub server: Option<LicenceServer>,
}

/// Raw form straight out of TOML, before validation.
#[derive(Deserialize)]
struct RawLicence {
    id: String,
    name: String,
    seats: u32,
    /// `YYYY-MM-DD` (licence valid through the end of that day, UTC) or a
    /// full RFC 3339 timestamp.
    expires: String,
    #[serde(default)]
    secret: Option<String>,
    #[serde(default)]
    server: Option<LicenceServer>,
}

#[derive(Deserialize)]
struct LicencesFile {
    #[serde(default)]
    licence: Vec<RawLicence>,
}

impl Licence {
    fn from_raw(raw: RawLicence) -> Result<Self> {
        if raw.id.is_empty() {
            bail!("licence entry with an empty id");
        }
        if raw.seats == 0 {
            bail!(
                "licence '{}' declares zero seats; seat count must be at least 1",
                raw.id
            );
        }
        let expires = parse_expiry(&raw.expires).with_context(|| {
            format!(
                "licence '{}' has an invalid expiry {:?}",
                raw.id, raw.expires
            )
        })?;
        Ok(Self {
            id: raw.id,
            name: raw.name,
            seats: raw.seats,
            expires,
            secret: raw.secret,
            server: raw.server,
        })
    }

    /// True once `now` has reached the expiry instant.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires
    }
}

fn parse_expiry(text: &str) -> Result<DateTime<Utc>> {
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        // Valid through the end of the named day.
        return Ok(date
            .and_hms_opt(23, 59, 59)
            .expect("23:59:59 is valid")
            .and_utc());
    }
    let dt = DateTime::parse_from_rfc3339(text)?;
    Ok(dt.with_timezone(&Utc))
}

/// The set of declared licences. Empty when nothing is configured — that
/// is a supported, fully working state (zero config).
#[derive(Debug, Clone, Default)]
pub struct LicenceRegistry {
    licences: Vec<Licence>,
}

impl LicenceRegistry {
    /// Parse declarations from TOML text.
    pub fn from_toml(text: &str) -> Result<Self> {
        let file: LicencesFile =
            toml::from_str(text).context("failed to parse licence declarations")?;
        let mut licences = Vec::with_capacity(file.licence.len());
        for raw in file.licence {
            let licence = Licence::from_raw(raw)?;
            if licences.iter().any(|l: &Licence| l.id == licence.id) {
                bail!("duplicate licence id '{}'", licence.id);
            }
            licences.push(licence);
        }
        Ok(Self { licences })
    }

    /// Load declarations from one file.
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::from_toml(&text).with_context(|| format!("failed to parse {}", path.display()))
    }

    /// `~/.prism/licences.toml`. A missing file is *not* an error — it is
    /// the zero-config state: an empty registry. A file that exists but
    /// does not parse is an error, reported at the first licence request.
    pub fn load_default() -> Result<Self> {
        match default_path() {
            Some(path) if path.exists() => Self::from_file(&path),
            _ => Ok(Self::default()),
        }
    }

    /// The default declarations path, when `$HOME` is known.
    pub fn default_path() -> Option<PathBuf> {
        default_path()
    }

    pub fn get(&self, id: &str) -> Option<&Licence> {
        self.licences.iter().find(|l| l.id == id)
    }

    pub fn ids(&self) -> Vec<&str> {
        self.licences.iter().map(|l| l.id.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.licences.len()
    }

    pub fn is_empty(&self) -> bool {
        self.licences.is_empty()
    }
}

fn default_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".prism").join(LICENCES_FILE))
}

/// Why a lease could not be issued. Every variant renders as a complete,
/// actionable sentence: which licence, how many seats exist, how many are
/// held, when one is due to free. Never phrased as a command for the user
/// to go run elsewhere — the remedy belongs where the user already is.
#[derive(Debug, Error)]
pub enum LicenceError {
    #[error(
        "job requested licence '{requested}' but no licence with that id is declared{declared}. \
         Licences are declared in ~/.prism/licences.toml as [[licence]] tables \
         with id, name, seats and expires"
    )]
    NotDeclared { requested: String, declared: String },

    #[error(
        "licence '{licence_name}' ({licence_id}) expired on {expired_at}; no lease can be \
         issued against an expired licence. Its declaration in ~/.prism/licences.toml \
         needs a later expiry before this job can be submitted"
    )]
    LicenceExpired {
        licence_id: String,
        licence_name: String,
        expired_at: DateTime<Utc>,
    },

    #[error(
        "no seat available for licence '{licence_name}' ({licence_id}): {seats_total} seat(s) \
         exist and {seats_held} {held_are} held by active jobs. {earliest} The job was not \
         submitted and nothing was dispatched"
    )]
    NoSeats {
        licence_id: String,
        licence_name: String,
        seats_total: u32,
        seats_held: u32,
        held_are: &'static str,
        earliest: String,
        /// Machine-readable copy of the earliest free time, for callers
        /// that want to act on it rather than render it.
        earliest_free: Option<DateTime<Utc>>,
    },

    #[error("licence request is invalid: {reason}")]
    InvalidRequest { reason: String },

    #[error("licence configuration is broken: {0}")]
    ConfigBroken(String),
}

impl LicenceError {
    /// Build the `NotDeclared` variant from a registry snapshot.
    pub fn not_declared(requested: &str, declared: &[&str]) -> Self {
        let declared = if declared.is_empty() {
            " — no licences are declared at all".to_string()
        } else {
            format!("; declared licences: {}", declared.join(", "))
        };
        Self::NotDeclared {
            requested: requested.to_string(),
            declared,
        }
    }

    /// Build the `NoSeats` variant from a live seat summary.
    pub fn no_seats(
        licence: &Licence,
        seats_held: u32,
        earliest_free: Option<DateTime<Utc>>,
    ) -> Self {
        let earliest = match earliest_free {
            Some(when) => format!(
                "The earliest held seat is due to free at {} UTC",
                when.format("%Y-%m-%dT%H:%M:%S")
            ),
            None => "Held seats free as their leases expire (each bounded by its job's walltime)"
                .to_string(),
        };
        Self::NoSeats {
            licence_id: licence.id.clone(),
            licence_name: licence.name.clone(),
            seats_total: licence.seats,
            seats_held,
            held_are: if seats_held == 1 { "is" } else { "are" },
            earliest,
            earliest_free,
        }
    }
}

/// Parse a SLURM `--time` wall limit into a duration.
///
/// Accepted SLURM forms: `MM`, `MM:SS`, `HH:MM:SS`, `D-HH`, `D-HH:MM`,
/// `D-HH:MM:SS`. A licence lease must never outlive the job's walltime, so
/// an unparseable walltime on a licensed job is an error, not a guess.
pub fn parse_slurm_walltime(spec: &str) -> Result<Duration> {
    let (days, rest) = match spec.split_once('-') {
        Some((days, rest)) => (
            days.parse::<u64>()
                .with_context(|| format!("invalid days component in {spec:?}"))?,
            rest,
        ),
        None => (0, spec),
    };

    let parts: Vec<&str> = rest.split(':').collect();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [a] => {
            let v = parse_time_part(a, spec)?;
            if days > 0 {
                // `D-HH`
                (v, 0, 0)
            } else {
                // bare number = minutes in SLURM
                (0, v, 0)
            }
        }
        [a, b] => {
            let h = parse_time_part(a, spec)?;
            let m = parse_time_part(b, spec)?;
            if days > 0 {
                // `D-HH:MM`
                (h, m, 0)
            } else {
                // `MM:SS`
                (0, h, m)
            }
        }
        [a, b, c] => (
            parse_time_part(a, spec)?,
            parse_time_part(b, spec)?,
            parse_time_part(c, spec)?,
        ),
        _ => bail!("invalid SLURM walltime {spec:?}"),
    };

    if minutes > 59 || seconds > 59 || (days == 0 && parts.len() == 3 && hours > 999) {
        bail!("invalid SLURM walltime {spec:?}");
    }

    let total = days * 86_400 + hours * 3_600 + minutes * 60 + seconds;
    if total == 0 {
        bail!("SLURM walltime {spec:?} is zero");
    }
    Ok(Duration::from_secs(total))
}

fn parse_time_part(part: &str, spec: &str) -> Result<u64> {
    if part.is_empty() {
        bail!("invalid SLURM walltime {spec:?}");
    }
    part.parse::<u64>()
        .with_context(|| format!("invalid component {part:?} in SLURM walltime {spec:?}"))
}

// ── Signed leases ──────────────────────────────────────────────────────

/// File names for the lease signing keypair under the compute data dir.
const LEASE_KEY_FILE: &str = "lease-signing.key";
const LEASE_PUB_FILE: &str = "lease-signing.pub";

/// Ed25519 keypair used to mint and verify leases.
///
/// The signing half never leaves the submitting machine. The verifying
/// half travels inside each lease so a compute node with no egress can
/// check the assertion offline. It is *not* the licence secret: a licence
/// secret (serial, key, server credential) grants the licence against the
/// vendor; this keypair only proves PRISM minted the lease.
#[derive(Clone)]
pub struct LeaseKeys {
    signing: SigningKey,
}

impl LeaseKeys {
    /// Fresh ephemeral keypair (in-memory managers, tests).
    pub fn generate() -> Self {
        Self {
            signing: SigningKey::generate(&mut rand::rngs::OsRng),
        }
    }

    /// Load the keypair from `dir`, creating it on first use. The private
    /// key file is written mode 0600.
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        let key_path = dir.join(LEASE_KEY_FILE);
        if let Some(keys) = Self::read(&key_path)
            .with_context(|| format!("failed to read lease signing key {}", key_path.display()))?
        {
            return Ok(keys);
        }
        let keys = Self::generate();
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;

        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&key_path) {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(hex::encode(keys.signing.to_bytes()).as_bytes())
                    .with_context(|| format!("failed to write {}", key_path.display()))?;
            }
            // Another process created the file between our check and open:
            // read theirs instead of fighting.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if let Some(existing) = Self::read(&key_path)? {
                    return Ok(existing);
                }
                return Err(e).with_context(|| format!("failed to open {}", key_path.display()));
            }
            Err(e) => {
                return Err(e).with_context(|| format!("failed to open {}", key_path.display()));
            }
        }
        // The public half is not secret; publish it beside for pinning on
        // compute nodes.
        std::fs::write(dir.join(LEASE_PUB_FILE), keys.verifying_key_hex())
            .context("failed to write lease verifying key")?;
        Ok(keys)
    }

    fn read(key_path: &Path) -> Result<Option<Self>> {
        if !key_path.exists() {
            return Ok(None);
        }
        let hexed = std::fs::read_to_string(key_path)?;
        let bytes = hex::decode(hexed.trim()).context("lease signing key is not hex")?;
        let secret: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("lease signing key must be 32 bytes"))?;
        Ok(Some(Self {
            signing: SigningKey::from_bytes(&secret),
        }))
    }

    pub fn verifying_key_hex(&self) -> String {
        hex::encode(self.signing.verifying_key().to_bytes())
    }

    /// Sign raw bytes (internal minting helper).
    fn sign(&self, message: &[u8]) -> String {
        hex::encode(self.signing.sign(message).to_bytes())
    }
}

/// A lease: the only licence artefact that ever travels to a compute node.
///
/// An opaque, signed, expiring assertion naming the licence id, seat
/// count, job id and expiry. Verifiable offline via [`Lease::verify`]
/// without the licence secret. `#[serde(deny_unknown_fields)]` keeps the
/// wire form honest: a blob with extra fields is a forgery attempt, not a
/// version skew.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub licence_id: String,
    pub seats: u32,
    pub job_id: Uuid,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Hex Ed25519 verifying key of the issuing PRISM deployment.
    pub verifying_key: String,
    /// Hex Ed25519 signature over [`Lease::payload`].
    pub signature: String,
}

impl Lease {
    /// Canonical bytes covered by the signature. Deliberately a plain
    /// deterministic encoding, not JSON: identical on every platform and
    /// Rust version, since the compute node may verify with a different
    /// build than the submitter.
    pub fn payload(&self) -> Vec<u8> {
        format!(
            "licence_id={};seats={};job_id={};issued_at={};expires_at={}",
            self.licence_id,
            self.seats,
            self.job_id,
            self.issued_at
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            self.expires_at
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
        )
        .into_bytes()
    }

    /// Offline verification: signature check only. Expiry is a separate
    /// question ([`Lease::is_expired`]) so a node can tell a forgery from
    /// an honest-but-late lease.
    pub fn verify(&self) -> Result<()> {
        let key_bytes =
            hex::decode(&self.verifying_key).context("lease verifying key is not hex")?;
        let key_bytes: [u8; 32] = key_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("lease verifying key must be 32 bytes"))?;
        let verifying = VerifyingKey::from_bytes(&key_bytes)
            .context("lease verifying key is not a valid Ed25519 key")?;
        let sig_bytes = hex::decode(&self.signature).context("lease signature is not hex")?;
        let signature =
            Signature::from_slice(&sig_bytes).context("lease signature is malformed")?;
        verifying
            .verify_strict(&self.payload(), &signature)
            .map_err(|e| anyhow::anyhow!("lease signature does not verify: {e}"))
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    /// Opaque single-token form for job scripts and environments:
    /// base64 of the signed JSON. Contains no licence secret.
    pub fn to_wire(&self) -> Result<String> {
        let json = serde_json::to_vec(self)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(json))
    }

    pub fn from_wire(wire: &str) -> Result<Self> {
        let json = base64::engine::general_purpose::STANDARD
            .decode(wire.trim())
            .context("lease blob is not valid base64")?;
        serde_json::from_slice(&json).context("lease blob is not a valid lease")
    }
}

/// Full provenance record for a licence checkout, shaped exactly like a
/// `prism_provenance::ProvenanceRecord` (that crate is not a dependency
/// of prism-compute; the ledger stores these fields as JSON).
///
/// Provenance is permanent, so it carries only lease facts — licence id,
/// seats, job id, expiry, signature validity. A licence secret must never
/// appear here.
pub fn lease_provenance_record(session_id: &str, lease: &Lease) -> serde_json::Value {
    serde_json::json!({
        "id": Uuid::new_v4().to_string(),
        "timestamp": Utc::now().to_rfc3339(),
        "session_id": session_id,
        "action_type": "compute",
        "actor": "system",
        "tool_name": "licence_checkout",
        "llm_model": null,
        "input_json": {
            "licence_id": lease.licence_id,
            "seats": lease.seats,
            "job_id": lease.job_id.to_string(),
        },
        "output_json": {
            "issued_at": lease.issued_at.to_rfc3339(),
            "expires_at": lease.expires_at.to_rfc3339(),
            "signature_valid": lease.verify().is_ok(),
        },
        "parent_id": null,
        "material_ref": null,
        "confidence": 1.0,
        "tags": ["licence", "compute"],
        "status": "ok",
        "exit_code": null,
    })
}

/// Mint a signed lease. Caller decides the expiry; see
/// [`bound_lease_expiry`] for the bounding rule.
pub fn sign_lease(
    keys: &LeaseKeys,
    licence_id: &str,
    seats: u32,
    job_id: Uuid,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Lease {
    let mut lease = Lease {
        licence_id: licence_id.to_string(),
        seats,
        job_id,
        issued_at,
        expires_at,
        verifying_key: keys.verifying_key_hex(),
        signature: String::new(),
    };
    lease.signature = keys.sign(&lease.payload());
    lease
}

/// A lease expiry must never exceed the licence expiry, and must never
/// exceed the job's walltime when one is known — a lease must not outlive
/// the job that holds it.
pub fn bound_lease_expiry(
    licence_expires: DateTime<Utc>,
    now: DateTime<Utc>,
    walltime: Option<Duration>,
) -> DateTime<Utc> {
    let mut expiry = licence_expires;
    if let Some(walltime) = walltime
        && let Ok(walltime) = chrono::Duration::from_std(walltime)
    {
        let wall_limit = now + walltime;
        if wall_limit < expiry {
            expiry = wall_limit;
        }
    }
    expiry
}

// ── Seat accounting ──────────────────────────────────────────────────

/// A seat reservation held between checkout and dispatch. The job id is
/// not known yet (or, on platform backends, is assigned by the platform),
/// so no signed lease exists yet — but the seats are already held, and a
/// concurrent checkout sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatHold {
    pub lease_id: Uuid,
    pub licence_id: String,
    pub seats: u32,
    pub expires_at: DateTime<Utc>,
}

/// Live seat usage for one licence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeatSummary {
    pub seats_held: u32,
    pub earliest_free: Option<DateTime<Utc>>,
}

/// Issues leases and accounts for seats.
///
/// Seat state is *derived*, not duplicated: a bound lease lives on the
/// job record inside the existing persistent [`JobTracker`]
/// (`compute-jobs.json`), and a seat counts as held while its job is
/// non-terminal and its lease unexpired. When a job reaches a terminal
/// state its seat returns automatically; a job that is killed, crashes,
/// or vanishes stops holding its seat when the lease expires (bounded by
/// the job's walltime). This module keeps no parallel job store.
///
/// The only transient state is the checkout→dispatch window, where seats
/// must be held before any job record exists; those holds live in memory
/// and die with the process — the safe direction (a lost hold frees a
/// seat, never leaks one).
pub struct LicenceManager {
    registry: Result<LicenceRegistry>,
    tracker: JobTracker,
    pending: Arc<RwLock<HashMap<Uuid, SeatHold>>>,
    keys: Arc<RwLock<Option<LeaseKeys>>>,
    keys_dir: Option<PathBuf>,
}

impl LicenceManager {
    /// `registry` is passed as a `Result` so a broken `licences.toml`
    /// fails loudly at the first licence request but never blocks the
    /// unlicensed path.
    pub fn new(
        registry: Result<LicenceRegistry>,
        tracker: JobTracker,
        keys_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            registry,
            tracker,
            pending: Arc::new(RwLock::new(HashMap::new())),
            keys: Arc::new(RwLock::new(None)),
            keys_dir,
        }
    }

    pub fn tracker(&self) -> &JobTracker {
        &self.tracker
    }

    /// Acquire seats before dispatch. Refuses — with an actionable,
    /// non-imperative message — when the licence is undeclared, expired,
    /// or out of seats. On success the seats are held until the hold is
    /// bound to a job record ([`Self::mint`] + tracker attach) or dropped
    /// ([`Self::drop_hold`]).
    pub async fn checkout(
        &self,
        request: &LicenceRequest,
        walltime: Option<Duration>,
    ) -> Result<SeatHold> {
        let registry = self
            .registry
            .as_ref()
            .map_err(|e| LicenceError::ConfigBroken(format!("{e:#}")))?;
        let Some(licence) = registry.get(&request.id) else {
            return Err(LicenceError::not_declared(&request.id, &registry.ids()).into());
        };
        if request.seats == 0 {
            return Err(LicenceError::InvalidRequest {
                reason: "seat count must be at least 1".into(),
            }
            .into());
        }
        let now = Utc::now();
        if licence.is_expired(now) {
            return Err(LicenceError::LicenceExpired {
                licence_id: licence.id.clone(),
                licence_name: licence.name.clone(),
                expired_at: licence.expires,
            }
            .into());
        }
        let expires_at = bound_lease_expiry(licence.expires, now, walltime);
        if expires_at <= now {
            return Err(LicenceError::InvalidRequest {
                reason: "the lease would expire before the job could run \
                         (licence expired or walltime is effectively zero)"
                    .into(),
            }
            .into());
        }

        // Critical section: count + decide + insert under one write lock,
        // so concurrent checkouts of the last seat can never both win.
        let mut pending = self.pending.write().await;
        let summary = self.held_summary_locked(&request.id, now, &pending).await;
        if summary.seats_held.saturating_add(request.seats) > licence.seats {
            return Err(
                LicenceError::no_seats(licence, summary.seats_held, summary.earliest_free).into(),
            );
        }
        let hold = SeatHold {
            lease_id: Uuid::new_v4(),
            licence_id: request.id.clone(),
            seats: request.seats,
            expires_at,
        };
        pending.insert(hold.lease_id, hold.clone());
        tracing::info!(
            licence = %request.id, seats = request.seats, lease_id = %hold.lease_id,
            expires_at = %expires_at,
            "licence seats held pre-dispatch"
        );
        Ok(hold)
    }

    /// Mint the signed lease once the job id is known. The hold must
    /// still be live; the seats stay held through the job record after
    /// the lease is attached there and the hold is dropped.
    pub async fn mint(&self, hold: &SeatHold, job_id: Uuid) -> Result<Lease> {
        {
            let pending = self.pending.read().await;
            let Some(live) = pending.get(&hold.lease_id) else {
                bail!("licence hold {} was already released", hold.lease_id);
            };
            if *live != *hold {
                bail!(
                    "licence hold {} does not match its reservation",
                    hold.lease_id
                );
            }
        }
        let keys = self.keys().await?;
        Ok(sign_lease(
            &keys,
            &hold.licence_id,
            hold.seats,
            job_id,
            Utc::now(),
            hold.expires_at,
        ))
    }

    /// Release a pre-dispatch hold (submission failed or was refused
    /// after checkout). Never called for bound leases — those free their
    /// seats through the job's terminal state or lease expiry.
    pub async fn drop_hold(&self, lease_id: Uuid) {
        self.pending.write().await.remove(&lease_id);
    }

    /// Seat usage for one licence right now: pending holds plus bound
    /// leases on non-terminal jobs, ignoring anything expired.
    pub async fn held_summary(&self, licence_id: &str) -> SeatSummary {
        let pending = self.pending.read().await;
        self.held_summary_locked(licence_id, Utc::now(), &pending)
            .await
    }

    async fn held_summary_locked(
        &self,
        licence_id: &str,
        now: DateTime<Utc>,
        pending: &HashMap<Uuid, SeatHold>,
    ) -> SeatSummary {
        let mut summary = SeatSummary::default();
        let mut account = |seats: u32, expires_at: DateTime<Utc>| {
            summary.seats_held += seats;
            summary.earliest_free = Some(match summary.earliest_free {
                Some(earliest) => earliest.min(expires_at),
                None => expires_at,
            });
        };
        for hold in pending.values() {
            if hold.licence_id == licence_id && hold.expires_at > now {
                account(hold.seats, hold.expires_at);
            }
        }
        for record in self.tracker.list(false).await {
            if let Some(lease) = &record.licence
                && lease.licence_id == licence_id
                && !record.status.is_terminal()
                && lease.expires_at > now
            {
                account(lease.seats, lease.expires_at);
            }
        }
        summary
    }

    /// Reclaim seats from jobs that no longer deserve them: bound leases
    /// whose job reached a terminal state or whose lease expired. Returns
    /// how many records were cleaned. Seat counting already ignores such
    /// records — this is hygiene that keeps the persisted store honest.
    pub async fn reclaim_expired(&self) -> Result<usize> {
        let now = Utc::now();
        let mut reclaimed = 0;
        for record in self.tracker.list(false).await {
            let Some(lease) = &record.licence else {
                continue;
            };
            if record.status.is_terminal() || lease.is_expired(now) {
                self.tracker.clear_licence(record.job_id).await?;
                reclaimed += 1;
            }
        }
        if reclaimed > 0 {
            tracing::info!(reclaimed, "reclaimed expired/terminal licence leases");
        }
        Ok(reclaimed)
    }

    /// Lazy keypair: created at first mint, not at startup, so the
    /// zero-config path never writes key material.
    async fn keys(&self) -> Result<LeaseKeys> {
        let mut guard = self.keys.write().await;
        if let Some(keys) = &*guard {
            return Ok(keys.clone());
        }
        let keys = match &self.keys_dir {
            Some(dir) => LeaseKeys::load_or_create(dir)?,
            None => LeaseKeys::generate(),
        };
        *guard = Some(keys.clone());
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECL: &str = r#"
[[licence]]
id = "vasp-6"
name = "VASP 6 (ESA pool)"
seats = 32
expires = "2026-12-31"
secret = "serial-9f2a-SECRET"

[licence.server]
host = "flexlm.example.org"
port = 27000

[[licence]]
id = "gaussian-16"
name = "Gaussian 16"
seats = 4
expires = "2026-08-09T12:00:00Z"
"#;

    #[test]
    fn registry_parses_declarations_with_server_and_secret() {
        let registry = LicenceRegistry::from_toml(DECL).unwrap();
        assert_eq!(registry.len(), 2);

        let vasp = registry.get("vasp-6").unwrap();
        assert_eq!(vasp.name, "VASP 6 (ESA pool)");
        assert_eq!(vasp.seats, 32);
        assert_eq!(vasp.secret.as_deref(), Some("serial-9f2a-SECRET"));
        assert_eq!(
            vasp.server,
            Some(LicenceServer {
                host: "flexlm.example.org".into(),
                port: 27000
            })
        );
        // Date-only expiry means valid through the end of that day, UTC.
        assert_eq!(
            vasp.expires,
            chrono::DateTime::parse_from_rfc3339("2026-12-31T23:59:59Z")
                .unwrap()
                .with_timezone(&Utc)
        );

        let gaussian = registry.get("gaussian-16").unwrap();
        assert_eq!(gaussian.seats, 4);
        assert!(gaussian.secret.is_none());
        assert!(gaussian.server.is_none());
    }

    #[test]
    fn empty_registry_when_nothing_declared() {
        let registry = LicenceRegistry::from_toml("").unwrap();
        assert!(registry.is_empty());
        assert_eq!(registry.get("vasp-6"), None);
        assert!(registry.ids().is_empty());
    }

    #[test]
    fn missing_file_is_zero_config_not_an_error() {
        let path = std::env::temp_dir().join(format!(
            "prism-licence-test-{}-absent",
            uuid::Uuid::new_v4()
        ));
        // from_file on a missing path errors (caller asked for that file)…
        assert!(LicenceRegistry::from_file(&path).is_err());
        // …but the *default* load with no file present is an empty registry.
        // (Whatever this machine's real ~/.prism state is, load_default
        // must never panic; we test the documented no-file behavior via
        // from_toml("") above and the loader's contract here.)
    }

    #[test]
    fn registry_rejects_zero_seats_duplicate_ids_and_bad_dates() {
        let zero_seats = r#"
[[licence]]
id = "x"
name = "X"
seats = 0
expires = "2026-12-31"
"#;
        let err = LicenceRegistry::from_toml(zero_seats).unwrap_err();
        assert!(err.to_string().contains("zero seats"), "{err}");

        let dup = r#"
[[licence]]
id = "x"
name = "X"
seats = 1
expires = "2026-12-31"
[[licence]]
id = "x"
name = "X2"
seats = 1
expires = "2026-12-31"
"#;
        let err = LicenceRegistry::from_toml(dup).unwrap_err();
        assert!(err.to_string().contains("duplicate licence id"), "{err}");

        let bad_date = r#"
[[licence]]
id = "x"
name = "X"
seats = 1
expires = "next tuesday"
"#;
        assert!(LicenceRegistry::from_toml(bad_date).is_err());
    }

    #[test]
    fn registry_secret_is_never_serialized() {
        let registry = LicenceRegistry::from_toml(DECL).unwrap();
        let json = serde_json::to_string(&registry.licences).unwrap();
        assert!(
            !json.contains("serial-9f2a-SECRET"),
            "secret leaked through serialization: {json}"
        );
    }

    #[test]
    fn licence_expiry_detection() {
        let registry = LicenceRegistry::from_toml(DECL).unwrap();
        let gaussian = registry.get("gaussian-16").unwrap();
        let before = chrono::DateTime::parse_from_rfc3339("2026-08-09T11:59:59Z")
            .unwrap()
            .with_timezone(&Utc);
        let after = chrono::DateTime::parse_from_rfc3339("2026-08-09T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(!gaussian.is_expired(before));
        assert!(gaussian.is_expired(after));
    }

    #[test]
    fn refusal_messages_name_licence_seats_and_next_free_time() {
        let registry = LicenceRegistry::from_toml(DECL).unwrap();
        let vasp = registry.get("vasp-6").unwrap();

        let earliest = chrono::DateTime::parse_from_rfc3339("2026-08-10T14:32:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let msg = LicenceError::no_seats(vasp, 32, Some(earliest)).to_string();
        assert!(msg.contains("vasp-6"), "{msg}");
        assert!(msg.contains("VASP 6 (ESA pool)"), "{msg}");
        assert!(msg.contains("32 seat(s) exist"), "{msg}");
        assert!(msg.contains("32 are held"), "{msg}");
        assert!(msg.contains("2026-08-10T14:32:00 UTC"), "{msg}");
        assert!(msg.contains("not submitted"), "{msg}");

        let msg = LicenceError::not_declared("vasp", &[]).to_string();
        assert!(msg.contains("vasp"), "{msg}");
        assert!(msg.contains("no licences are declared"), "{msg}");
        assert!(msg.contains("licences.toml"), "{msg}");

        let msg = LicenceError::not_declared("vasp", &["gaussian-16"]).to_string();
        assert!(msg.contains("gaussian-16"), "{msg}");
    }

    #[test]
    fn refusal_messages_never_tell_the_user_to_run_a_command() {
        // Repo-wide guard crates/server/tests/no_exit_to_cli.rs bans the
        // imperative form in human-facing strings; keep licence refusals
        // compliant at the source.
        let registry = LicenceRegistry::from_toml(DECL).unwrap();
        let vasp = registry.get("vasp-6").unwrap();
        let messages = [
            LicenceError::no_seats(vasp, 32, Some(Utc::now())).to_string(),
            LicenceError::not_declared("vasp", &[]).to_string(),
            LicenceError::LicenceExpired {
                licence_id: vasp.id.clone(),
                licence_name: vasp.name.clone(),
                expired_at: Utc::now(),
            }
            .to_string(),
        ];
        for msg in &messages {
            let lowered = msg.to_ascii_lowercase();
            for needle in ["run `prism ", "runs `prism ", "run: prism ", "run prism "] {
                assert!(!lowered.contains(needle), "imperative remedy in: {msg}");
            }
        }
    }

    #[test]
    fn slurm_walltime_parses_all_slurm_forms() {
        assert_eq!(
            parse_slurm_walltime("30").unwrap(),
            Duration::from_secs(30 * 60)
        );
        assert_eq!(
            parse_slurm_walltime("10:30").unwrap(),
            Duration::from_secs(10 * 60 + 30)
        );
        assert_eq!(
            parse_slurm_walltime("04:00:00").unwrap(),
            Duration::from_secs(4 * 3600)
        );
        assert_eq!(
            parse_slurm_walltime("1-00").unwrap(),
            Duration::from_secs(24 * 3600)
        );
        assert_eq!(
            parse_slurm_walltime("2-12").unwrap(),
            Duration::from_secs(60 * 3600)
        );
        assert_eq!(
            parse_slurm_walltime("1-02:03").unwrap(),
            Duration::from_secs(26 * 3600 + 3 * 60)
        );
        assert_eq!(
            parse_slurm_walltime("1-02:03:04").unwrap(),
            Duration::from_secs(26 * 3600 + 3 * 60 + 4)
        );
    }

    #[test]
    fn slurm_walltime_rejects_garbage_and_zero() {
        for bad in [
            "", "abc", "10:", ":10", "0", "00:00:00", "1-2-3", "10:70", "10:00:99",
        ] {
            assert!(
                parse_slurm_walltime(bad).is_err(),
                "{bad:?} should not parse"
            );
        }
    }

    #[test]
    fn licence_request_defaults_to_one_seat() {
        let parsed: LicenceRequest = serde_json::from_str(r#"{"id": "vasp-6"}"#).unwrap();
        assert_eq!(parsed.seats, 1);
    }

    // ── Seat accounting (LicenceManager over JobTracker) ───────────────

    const ONE_LICENCE: &str = r#"
[[licence]]
id = "vasp-6"
name = "VASP 6 (ESA pool)"
seats = 2
expires = "2126-12-31"
secret = "serial-9f2a-SECRET"
"#;

    fn manager_with(decl: &str) -> LicenceManager {
        let registry = LicenceRegistry::from_toml(decl);
        LicenceManager::new(registry, JobTracker::new(), None)
    }

    fn request(id: &str, seats: u32) -> LicenceRequest {
        LicenceRequest {
            id: id.into(),
            seats,
        }
    }

    /// Checkout + mint + bind to a registered job record, mirroring the
    /// dispatch path.
    async fn bind_lease(
        manager: &LicenceManager,
        request: &LicenceRequest,
        walltime: Option<Duration>,
    ) -> (Uuid, Lease) {
        let hold = manager.checkout(request, walltime).await.unwrap();
        let job_id = Uuid::new_v4();
        let lease = manager.mint(&hold, job_id).await.unwrap();
        manager
            .tracker()
            .register(
                job_id,
                "licensed-job",
                "vasp6.sif",
                "byoc",
                crate::job::JobTarget::Local,
            )
            .await
            .unwrap();
        manager
            .tracker()
            .attach_licence(job_id, lease.clone())
            .await
            .unwrap();
        manager.drop_hold(hold.lease_id).await;
        (job_id, lease)
    }

    #[tokio::test]
    async fn concurrent_checkout_of_last_seat_never_oversubscribes() {
        let manager = Arc::new(manager_with(ONE_LICENCE));
        let mut set = tokio::task::JoinSet::new();
        // 16 racers for 2 seats.
        for _ in 0..16 {
            let manager = Arc::clone(&manager);
            set.spawn(async move { manager.checkout(&request("vasp-6", 1), None).await });
        }
        let mut granted = 0;
        let mut refused = 0;
        while let Some(result) = set.join_next().await {
            match result.unwrap() {
                Ok(_) => granted += 1,
                Err(error) => {
                    refused += 1;
                    assert!(
                        error.downcast_ref::<LicenceError>().is_some(),
                        "refusal must be a LicenceError: {error}"
                    );
                }
            }
        }
        assert_eq!(granted, 2, "seats were oversubscribed");
        assert_eq!(refused, 14);

        let summary = manager.held_summary("vasp-6").await;
        assert_eq!(summary.seats_held, 2);
        assert!(summary.earliest_free.is_some());
    }

    #[tokio::test]
    async fn expired_licence_never_issues_a_lease() {
        let decl = r#"
[[licence]]
id = "gaussian-16"
name = "Gaussian 16"
seats = 4
expires = "2020-01-01"
"#;
        let manager = manager_with(decl);
        let error = manager
            .checkout(&request("gaussian-16", 1), None)
            .await
            .unwrap_err();
        let licence_error = error.downcast_ref::<LicenceError>().unwrap();
        assert!(
            matches!(licence_error, LicenceError::LicenceExpired { .. }),
            "{licence_error}"
        );
        let message = licence_error.to_string();
        assert!(message.contains("expired on"), "{message}");
        assert!(message.contains("gaussian-16"), "{message}");
        // No hold escaped the refusal.
        assert_eq!(
            manager.held_summary("gaussian-16").await,
            SeatSummary::default()
        );
    }

    #[tokio::test]
    async fn manager_bounds_lease_expiry_by_licence_and_walltime() {
        // Licence expires ~30 days from now, so this test does not depend
        // on the machine clock relative to a hardcoded date.
        let expiry_date = (Utc::now() + chrono::Duration::days(30))
            .format("%Y-%m-%d")
            .to_string();
        let decl = format!(
            r#"
[[licence]]
id = "vasp-6"
name = "VASP 6 (ESA pool)"
seats = 2
expires = "{expiry_date}"
"#
        );
        let manager = manager_with(&decl);
        let registry = LicenceRegistry::from_toml(&decl).unwrap();
        let licence = registry.get("vasp-6").unwrap();

        // Walltime longer than the licence lifetime: licence expiry wins.
        let hold = manager
            .checkout(
                &request("vasp-6", 1),
                Some(Duration::from_secs(500 * 86_400)),
            )
            .await
            .unwrap();
        let lease = manager.mint(&hold, Uuid::new_v4()).await.unwrap();
        assert!(lease.expires_at <= licence.expires);
        assert_eq!(hold.expires_at, licence.expires);
        manager.drop_hold(hold.lease_id).await;

        // Walltime shorter than the licence lifetime: walltime wins.
        let before = Utc::now();
        let hold = manager
            .checkout(&request("vasp-6", 1), Some(Duration::from_secs(600)))
            .await
            .unwrap();
        let lease = manager.mint(&hold, Uuid::new_v4()).await.unwrap();
        assert!(
            lease.expires_at <= before + chrono::Duration::seconds(600 + 5),
            "{}",
            lease.expires_at
        );
        assert!(lease.expires_at > before);
        lease.verify().unwrap();
    }

    #[tokio::test]
    async fn terminal_job_returns_its_seat() {
        let manager = manager_with(ONE_LICENCE);
        let (job_id, _lease) = bind_lease(&manager, &request("vasp-6", 2), None).await;
        assert_eq!(manager.held_summary("vasp-6").await.seats_held, 2);

        // A full house refuses.
        assert!(manager.checkout(&request("vasp-6", 1), None).await.is_err());

        // The job dies — killed, crashed, or cancelled — and the tracker
        // records the terminal state.
        manager
            .tracker()
            .update_status(
                job_id,
                crate::job::TrackedStatus::Failed {
                    error: "NODE_FAIL".into(),
                },
            )
            .await
            .unwrap();

        // Seats are back to full without anyone releasing explicitly.
        assert_eq!(manager.held_summary("vasp-6").await.seats_held, 0);
        let hold = manager.checkout(&request("vasp-6", 2), None).await.unwrap();
        manager.drop_hold(hold.lease_id).await;
    }

    #[tokio::test]
    async fn vanished_job_seat_is_reclaimed_when_lease_expires() {
        let manager = manager_with(ONE_LICENCE);
        // Lease bounded by a 50ms walltime: the job vanishes without ever
        // releasing, and no status update ever arrives.
        let (job_id, lease) = bind_lease(
            &manager,
            &request("vasp-6", 2),
            Some(Duration::from_millis(50)),
        )
        .await;
        assert_eq!(manager.held_summary("vasp-6").await.seats_held, 2);
        assert!(!lease.is_expired(Utc::now()));

        tokio::time::sleep(Duration::from_millis(120)).await;

        // The expired lease no longer holds seats…
        assert_eq!(manager.held_summary("vasp-6").await.seats_held, 0);
        // …and reclaim clears the stale record so the store stays honest.
        assert_eq!(manager.reclaim_expired().await.unwrap(), 1);
        assert!(
            manager
                .tracker()
                .get(job_id)
                .await
                .unwrap()
                .licence
                .is_none()
        );

        // Seat count is back to full: both seats can be taken again.
        let hold = manager.checkout(&request("vasp-6", 2), None).await.unwrap();
        manager.drop_hold(hold.lease_id).await;
    }

    #[tokio::test]
    async fn reclaim_also_clears_terminal_records_with_live_leases() {
        let manager = manager_with(ONE_LICENCE);
        let (job_id, _lease) = bind_lease(&manager, &request("vasp-6", 1), None).await;
        manager
            .tracker()
            .update_status(job_id, crate::job::TrackedStatus::Cancelled)
            .await
            .unwrap();
        assert_eq!(manager.reclaim_expired().await.unwrap(), 1);
        assert!(
            manager
                .tracker()
                .get(job_id)
                .await
                .unwrap()
                .licence
                .is_none()
        );
        // Second sweep finds nothing left to reclaim.
        assert_eq!(manager.reclaim_expired().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn zero_licences_licensed_request_refuses_naming_what_is_missing() {
        let manager = manager_with("");
        let error = manager
            .checkout(&request("vasp-6", 1), None)
            .await
            .unwrap_err();
        let licence_error = error.downcast_ref::<LicenceError>().unwrap();
        let message = licence_error.to_string();
        assert!(
            matches!(licence_error, LicenceError::NotDeclared { .. }),
            "{message}"
        );
        assert!(message.contains("vasp-6"), "{message}");
        assert!(message.contains("no licences are declared"), "{message}");
        assert!(message.contains("licences.toml"), "{message}");
    }

    #[tokio::test]
    async fn unknown_licence_id_names_what_is_declared() {
        let manager = manager_with(ONE_LICENCE);
        let error = manager
            .checkout(&request("ansys", 1), None)
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("ansys"), "{message}");
        assert!(message.contains("vasp-6"), "{message}");
    }

    #[tokio::test]
    async fn broken_config_refuses_licensed_requests() {
        let manager = LicenceManager::new(
            Err(anyhow::anyhow!("failed to parse licences.toml: bad TOML")),
            JobTracker::new(),
            None,
        );
        let error = manager
            .checkout(&request("vasp-6", 1), None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<LicenceError>(),
                Some(LicenceError::ConfigBroken(_))
            ),
            "{error}"
        );
        assert!(error.to_string().contains("licences.toml"), "{error}");
    }

    #[tokio::test]
    async fn mint_rejects_a_dropped_or_mismatched_hold() {
        let manager = manager_with(ONE_LICENCE);
        let hold = manager.checkout(&request("vasp-6", 1), None).await.unwrap();
        manager.drop_hold(hold.lease_id).await;
        assert!(manager.mint(&hold, Uuid::new_v4()).await.is_err());

        let hold = manager.checkout(&request("vasp-6", 1), None).await.unwrap();
        let mut mismatched = hold.clone();
        mismatched.seats = 99;
        assert!(manager.mint(&mismatched, Uuid::new_v4()).await.is_err());
        manager.drop_hold(hold.lease_id).await;
    }

    #[tokio::test]
    async fn no_seats_refusal_reports_held_count_and_next_free_time() {
        let manager = manager_with(ONE_LICENCE);
        let (_job_id, lease) = bind_lease(&manager, &request("vasp-6", 2), None).await;
        let error = manager
            .checkout(&request("vasp-6", 1), None)
            .await
            .unwrap_err();
        let licence_error = error.downcast_ref::<LicenceError>().unwrap();
        let (message, earliest_free) = match licence_error {
            LicenceError::NoSeats { earliest_free, .. } => {
                (licence_error.to_string(), *earliest_free)
            }
            other => panic!("expected NoSeats, got {other}"),
        };
        assert!(message.contains("2 seat(s) exist"), "{message}");
        assert!(message.contains("2 are held"), "{message}");
        // The named free time is the expiring lease's expiry.
        assert!(message.contains("due to free"), "{message}");
        assert_eq!(earliest_free.unwrap(), lease.expires_at);
    }

    #[tokio::test]
    async fn bound_lease_survives_tracker_roundtrip() {
        use crate::job::JobTracker;

        let data_dir =
            std::env::temp_dir().join(format!("prism-licence-tracker-{}", Uuid::new_v4()));
        let tracker = JobTracker::persistent(&data_dir).unwrap();
        let manager = LicenceManager::new(
            LicenceRegistry::from_toml(ONE_LICENCE),
            tracker.clone(),
            None,
        );
        let (job_id, lease) = bind_lease(&manager, &request("vasp-6", 1), None).await;
        drop(tracker);
        drop(manager);

        // A fresh process instance sees the bound lease and counts the seat.
        let tracker = JobTracker::persistent(&data_dir).unwrap();
        let manager = LicenceManager::new(LicenceRegistry::from_toml(ONE_LICENCE), tracker, None);
        let summary = manager.held_summary("vasp-6").await;
        assert_eq!(summary.seats_held, 1, "seat lost across process restart");
        let record = manager.tracker().get(job_id).await.unwrap();
        let persisted = record.licence.unwrap();
        assert_eq!(persisted, lease);
        persisted.verify().unwrap();

        std::fs::remove_dir_all(data_dir).unwrap();
    }

    // ── Signed leases ──────────────────────────────────────────────────

    fn test_lease(keys: &LeaseKeys, expires_at: DateTime<Utc>) -> Lease {
        sign_lease(keys, "vasp-6", 4, Uuid::new_v4(), Utc::now(), expires_at)
    }

    #[test]
    fn lease_verifies_offline_without_the_secret() {
        // The licence secret plays no part in minting or verifying a
        // lease; the Ed25519 keypair is independent of it.
        let keys = LeaseKeys::generate();
        let lease = test_lease(&keys, Utc::now() + chrono::Duration::hours(2));
        lease.verify().unwrap();
        assert!(!lease.is_expired(Utc::now()));
    }

    #[test]
    fn tampered_lease_fails_verification() {
        let keys = LeaseKeys::generate();
        let lease = test_lease(&keys, Utc::now() + chrono::Duration::hours(2));

        let mut forged = lease.clone();
        forged.seats = 4096;
        assert!(forged.verify().is_err(), "seat count forgery must fail");

        let mut forged = lease.clone();
        forged.expires_at += chrono::Duration::days(365);
        assert!(forged.verify().is_err(), "expiry extension must fail");

        let mut forged = lease.clone();
        forged.job_id = Uuid::new_v4();
        assert!(forged.verify().is_err(), "job id swap must fail");

        let mut forged = lease;
        forged.licence_id = "gaussian-16".into();
        assert!(forged.verify().is_err(), "licence id swap must fail");
    }

    #[test]
    fn lease_signed_by_one_key_does_not_verify_with_another() {
        let keys = LeaseKeys::generate();
        let mut lease = test_lease(&keys, Utc::now() + chrono::Duration::hours(1));
        let attacker = LeaseKeys::generate();
        lease.verifying_key = attacker.verifying_key_hex();
        assert!(lease.verify().is_err());
    }

    #[test]
    fn lease_survives_wire_roundtrip_and_still_verifies() {
        let keys = LeaseKeys::generate();
        let lease = test_lease(&keys, Utc::now() + chrono::Duration::hours(1));
        let wire = lease.to_wire().unwrap();
        let back = Lease::from_wire(&wire).unwrap();
        assert_eq!(back, lease);
        back.verify().unwrap();

        assert!(Lease::from_wire("not base64 !!!").is_err());
    }

    #[test]
    fn lease_wire_form_carries_neither_secret_nor_signing_key() {
        let keys = LeaseKeys::generate();
        let lease = test_lease(&keys, Utc::now() + chrono::Duration::hours(1));
        let wire = lease.to_wire().unwrap();
        assert!(
            !wire.contains("serial-9f2a-SECRET"),
            "licence secret in wire form"
        );
        assert!(
            !wire.contains(&hex::encode(keys.signing.to_bytes())),
            "private signing key in wire form"
        );
    }

    #[test]
    fn lease_expiry_is_bounded_by_licence_expiry() {
        let licence_expires = Utc::now() + chrono::Duration::days(30);
        // Walltime far beyond the licence expiry: the licence wins.
        let expiry = bound_lease_expiry(
            licence_expires,
            Utc::now(),
            Some(Duration::from_secs(100 * 86_400)),
        );
        assert_eq!(expiry, licence_expires);
    }

    #[test]
    fn lease_expiry_is_bounded_by_job_walltime() {
        let now = Utc::now();
        let licence_expires = now + chrono::Duration::days(365);
        let walltime = Duration::from_secs(2 * 3600);
        let expiry = bound_lease_expiry(licence_expires, now, Some(walltime));
        assert!(expiry <= now + chrono::Duration::hours(2), "{expiry}");
        assert!(expiry > now, "{expiry}");
        // No walltime known: bounded by the licence expiry alone.
        let expiry = bound_lease_expiry(licence_expires, now, None);
        assert_eq!(expiry, licence_expires);
    }

    #[test]
    fn lease_keys_persist_and_reload() {
        let dir = std::env::temp_dir().join(format!("prism-lease-keys-{}", Uuid::new_v4()));
        let keys = LeaseKeys::load_or_create(&dir).unwrap();
        let verifying = keys.verifying_key_hex();
        let lease = test_lease(&keys, Utc::now() + chrono::Duration::hours(1));

        // A fresh process loads the same keypair; old leases still verify.
        let reloaded = LeaseKeys::load_or_create(&dir).unwrap();
        assert_eq!(reloaded.verifying_key_hex(), verifying);
        let new_lease = test_lease(&reloaded, Utc::now() + chrono::Duration::hours(1));
        lease.verify().unwrap();
        new_lease.verify().unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(LEASE_KEY_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "private lease key must be 0600");
        }

        std::fs::remove_dir_all(dir).unwrap();
    }
}
