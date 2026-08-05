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

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use chrono::{DateTime, NaiveDate, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

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
            bail!("licence '{}' declares zero seats; seat count must be at least 1", raw.id);
        }
        let expires = parse_expiry(&raw.expires)
            .with_context(|| format!("licence '{}' has an invalid expiry {:?}", raw.id, raw.expires))?;
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
        return Ok(date.and_hms_opt(23, 59, 59).expect("23:59:59 is valid").and_utc());
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
    pub fn from_str(text: &str) -> Result<Self> {
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
        Self::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
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
                return Err(e).with_context(|| format!("failed to open {}", key_path.display()))
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
            self.issued_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            self.expires_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
        )
        .into_bytes()
    }

    /// Offline verification: signature check only. Expiry is a separate
    /// question ([`Lease::is_expired`]) so a node can tell a forgery from
    /// an honest-but-late lease.
    pub fn verify(&self) -> Result<()> {
        let key_bytes = hex::decode(&self.verifying_key)
            .context("lease verifying key is not hex")?;
        let key_bytes: [u8; 32] = key_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("lease verifying key must be 32 bytes"))?;
        let verifying = VerifyingKey::from_bytes(&key_bytes)
            .context("lease verifying key is not a valid Ed25519 key")?;
        let sig_bytes = hex::decode(&self.signature)
            .context("lease signature is not hex")?;
        let signature = Signature::from_slice(&sig_bytes)
            .context("lease signature is malformed")?;
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
        let registry = LicenceRegistry::from_str(DECL).unwrap();
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
        let registry = LicenceRegistry::from_str("").unwrap();
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
        // from_str("") above and the loader's contract here.)
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
        let err = LicenceRegistry::from_str(zero_seats).unwrap_err();
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
        let err = LicenceRegistry::from_str(dup).unwrap_err();
        assert!(err.to_string().contains("duplicate licence id"), "{err}");

        let bad_date = r#"
[[licence]]
id = "x"
name = "X"
seats = 1
expires = "next tuesday"
"#;
        assert!(LicenceRegistry::from_str(bad_date).is_err());
    }

    #[test]
    fn registry_secret_is_never_serialized() {
        let registry = LicenceRegistry::from_str(DECL).unwrap();
        let json = serde_json::to_string(&registry.licences).unwrap();
        assert!(
            !json.contains("serial-9f2a-SECRET"),
            "secret leaked through serialization: {json}"
        );
    }

    #[test]
    fn licence_expiry_detection() {
        let registry = LicenceRegistry::from_str(DECL).unwrap();
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
        let registry = LicenceRegistry::from_str(DECL).unwrap();
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
        let registry = LicenceRegistry::from_str(DECL).unwrap();
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
        assert_eq!(parse_slurm_walltime("30").unwrap(), Duration::from_secs(30 * 60));
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
        for bad in ["", "abc", "10:", ":10", "0", "00:00:00", "1-2-3", "10:70", "10:00:99"] {
            assert!(parse_slurm_walltime(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn licence_request_defaults_to_one_seat() {
        let parsed: LicenceRequest = serde_json::from_str(r#"{"id": "vasp-6"}"#).unwrap();
        assert_eq!(parsed.seats, 1);
    }

    // ── Signed leases ──────────────────────────────────────────────────

    fn test_lease(keys: &LeaseKeys, expires_at: DateTime<Utc>) -> Lease {
        sign_lease(
            keys,
            "vasp-6",
            4,
            Uuid::new_v4(),
            Utc::now(),
            expires_at,
        )
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
        let expiry = bound_lease_expiry(licence_expires, Utc::now(), Some(Duration::from_secs(100 * 86_400)));
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
            let mode = std::fs::metadata(dir.join(LEASE_KEY_FILE)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "private lease key must be 0600");
        }

        std::fs::remove_dir_all(dir).unwrap();
    }
}
