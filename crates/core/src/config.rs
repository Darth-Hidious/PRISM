//! Node configuration — loaded from `prism.toml`, merged with CLI flags.
//!
//! Search order (later overrides earlier):
//! 1. Built-in defaults
//! 2. `~/.prism/prism.toml` (global)
//! 3. `.prism/prism.toml` (project)
//! 4. CLI flags / environment variables

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    #[serde(default)]
    pub node: NodeSection,
    #[serde(default)]
    pub services: ServicesSection,
    #[serde(default)]
    pub platform: PlatformSection,
    #[serde(default)]
    pub mesh: MeshSection,
    #[serde(default)]
    pub ontology: OntologySection,
    #[serde(default)]
    pub auth: AuthSection,
    #[serde(default)]
    pub audit: AuditSection,
    #[serde(default)]
    pub llm: LlmSection,
    #[serde(default)]
    pub ingest: IngestSection,
    #[serde(default)]
    pub indexer: ModelServiceSection,
    #[serde(default)]
    pub searcher: ModelServiceSection,
    #[serde(default)]
    pub calphad: CalphadSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSection {
    #[serde(default = "default_node_name")]
    pub name: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServicesSection {
    /// "managed" (Docker) or "external" (user-provided URIs)
    #[serde(default = "default_managed")]
    pub mode: String,
    /// External Kafka URI when mode=external
    #[serde(default)]
    pub kafka_uri: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlatformSection {
    /// Hosted-provider root or API base. There is no implicit provider:
    /// absence means this PRISM install is local-only.
    #[serde(default)]
    pub url: Option<String>,
    /// Optional external adapter identity (for example `marc27`). PRISM owns
    /// authorization; this only selects how a provider's data is translated.
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshSection {
    #[serde(default = "default_discovery")]
    pub discovery: Vec<String>,
    #[serde(default = "default_mesh_port")]
    pub publish_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OntologySection {
    #[serde(default = "default_engine")]
    pub engine: String,
    /// Id of the ontology vocabulary local tabular ingest extracts and
    /// validates with, resolved through prism-ingest's process-wide
    /// ontology registry. Default: "emmo" (the built-in EMMO materials
    /// vocabulary). CLI ingest can lazily register an accepted artifact from
    /// the project's `.prism/ontologies/<id>.ttl`; an id available from
    /// neither source fails ingest loudly.
    #[serde(default = "default_ontology_id")]
    pub id: String,
    #[serde(default = "default_llm_provider")]
    pub llm_provider: String,
    /// Custom ontology mapping rules YAML file path.
    #[serde(default)]
    pub mapping_file: Option<String>,
    /// Where text-document ingest runs: "auto" | "local" | "cloud".
    /// auto → local when the resolved LLM endpoint is loopback (an
    /// on-device model), else cloud.
    #[serde(default = "default_locality")]
    pub locality: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSection {
    #[serde(default = "default_session_timeout")]
    pub session_timeout: String,
    #[serde(default = "default_true")]
    pub require_platform_auth: bool,
    #[serde(default = "default_true")]
    pub allow_local_users: bool,
}

/// Cross-org audit envelopes (F5). When enabled (the default), the node
/// emits signed, append-only audit records for cross-org events it
/// receives — relayed tool invocations and verified federation requests.
/// Set `enabled = false` to opt out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditSection {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for AuditSection {
    fn default() -> Self {
        Self {
            enabled: default_true(),
        }
    }
}

/// Unified LLM configuration — used by ingest, query, agent, and all tools.
///
/// One config, propagated everywhere. CLI flags override config values.
/// Set once with `prism configure --llm-*`, then every command uses it.
///
/// NOTE: `Debug` is implemented by hand (not derived) so `api_key` is REDACTED.
/// This struct is embedded in `NodeConfig`, which is `tracing::debug!(?..)`-dumped
/// into `node.log` on `node up`; a derived `Debug` would write the plaintext key
/// into a shareable log at `RUST_LOG=debug` (CWE-532). Keep it hand-written.
#[derive(Clone, Serialize, Deserialize)]
pub struct LlmSection {
    /// Provider hint: "llamacpp" (default), "ollama", "openai", "marc27", "anthropic".
    /// All providers use OpenAI-compatible API — this just sets sensible defaults.
    #[serde(default = "default_llm_kind")]
    pub provider: String,
    /// LLM base URL (e.g. "http://localhost:8080" for llama.cpp).
    #[serde(default = "default_llm_url")]
    pub url: String,
    /// Generation model name (e.g. "gemma-4-E4B-it", "claude-sonnet-4-6").
    #[serde(default)]
    pub model: Option<String>,
    /// Embedding model (separate from generation). If None, uses `model`.
    #[serde(default)]
    pub embedding_model: Option<String>,
    /// API key for authenticated providers (OpenAI, Anthropic, MARC27).
    #[serde(default)]
    pub api_key: Option<String>,
    /// Environment variable name for API key (alternative to inline).
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
    /// Request timeout in seconds.
    #[serde(default = "default_llm_timeout")]
    pub timeout_secs: u64,
    /// Max output tokens per request. `None` keeps the client's conservative
    /// default (4096). The extraction-failure diagnostic has ALWAYS told
    /// users to "raise max_output_tokens in the LLM config" when a
    /// thinking-mode model burns its whole budget on reasoning_content —
    /// but no config field existed on this path, so the advice was
    /// un-actionable (live 2026-08-10: Gemma-4-12B spent 11,100 chars of
    /// reasoning against the 4096 default and produced zero JSON).
    /// Max output tokens per response. `None` keeps the client's
    /// conservative default (4096). Reasoning/"thinking" models spend
    /// output budget on reasoning_content BEFORE the answer — gemma-4-12B
    /// burned the whole 4096 on thinking and produced zero JSON, and the
    /// client's error message told the user to raise a knob that did not
    /// exist on this path until this field.
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    /// Replay each assistant turn's `reasoning_content` in later requests.
    /// Thinking providers that want their reasoning back (z.ai's preserved
    /// thinking) do better multi-turn tool use with it; others reject the
    /// field in input. Off unless the operator says so for this endpoint.
    #[serde(default = "default_replay_reasoning_content")]
    pub replay_reasoning_content: bool,
}

impl std::fmt::Debug for LlmSection {
    /// Hand-written so a secret never reaches a log. Every field is shown
    /// EXCEPT `api_key`, which is reduced to whether it is set — never its value.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmSection")
            .field("provider", &self.provider)
            .field("url", &self.url)
            .field("model", &self.model)
            .field("embedding_model", &self.embedding_model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("api_key_env", &self.api_key_env)
            .field("timeout_secs", &self.timeout_secs)
            .field("max_output_tokens", &self.max_output_tokens)
            .finish()
    }
}

impl Default for LlmSection {
    fn default() -> Self {
        Self {
            provider: default_llm_kind(),
            url: default_llm_url(),
            model: None,
            embedding_model: None,
            api_key: None,
            api_key_env: default_api_key_env(),
            timeout_secs: default_llm_timeout(),
            max_output_tokens: None,
            replay_reasoning_content: default_replay_reasoning_content(),
        }
    }
}

fn default_replay_reasoning_content() -> bool {
    true
}

fn default_llm_kind() -> String {
    "llamacpp".into()
}
fn default_llm_url() -> String {
    "http://localhost:8080".into()
}
fn default_api_key_env() -> String {
    "LLM_API_KEY".into()
}
fn default_llm_timeout() -> u64 {
    // 0 = no read deadline; see prism_llm::LlmClient::new. This used to be 120
    // while crates/llm defaulted to 300 — two disagreeing deadlines, and the
    // shorter one silently won on the ingest path.
    0
}

/// Ingest batching — how much of a dataset or document goes into ONE
/// extraction call. The default for both knobs is DERIVED from the model's
/// context window (the binding constraint), not decreed: PRISM used to send
/// exactly 10 rows of any dataset and the first 60,000 bytes of any document
/// and silently discard the rest. These overrides exist for operators, not
/// as protective caps — the whole input is processed either way, in batches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestSection {
    /// Rows of tabular data per extraction batch. Unset ⇒ batches are packed
    /// to a byte budget derived from the model's context window.
    #[serde(default)]
    pub batch_rows: Option<usize>,
    /// Bytes of document text per extraction window. Unset ⇒ derived from
    /// the model's context window. Windows overlap slightly so a fact
    /// spanning a boundary is still seen whole by one of them.
    #[serde(default)]
    pub chunk_bytes: Option<usize>,
    /// The paper loop's READING STANDARD: the fraction of a document's lines
    /// the reader must have seen before a FIRST `finish` is accepted. Below
    /// it, the finish is refused ONCE with the largest unread ranges (the
    /// second finish always succeeds). Lines, not content — it measures
    /// reading, never what the lines say. `0` disables the gate.
    #[serde(default = "default_finish_coverage_floor")]
    pub finish_coverage_floor: f64,
    /// The paper loop's EXTRACTION STANDARD: the fraction of the
    /// quantity-bearing lines the reader has SEEN that may remain uncited by
    /// any proposal before a FIRST `finish` is refused once, with those line
    /// numbers handed back. It shows, it never demands — no target count is
    /// stated anywhere, because a model told to produce more facts produces
    /// false ones. `1` disables the gate.
    #[serde(default = "default_finish_quantity_floor")]
    pub finish_quantity_floor: f64,
    /// Capability verdict (§D.5): when EVERY sample's proposal acceptance
    /// rate stays below this floor — together with a degenerate-rate or
    /// coverage failure — the extraction is reported `model_insufficient`
    /// instead of masquerading as a quiet paper.
    #[serde(default = "default_model_acceptance_floor")]
    pub model_acceptance_floor: f64,
    /// Capability verdict (§D.5): structural-degeneracy rate of a sample's
    /// facts above this ceiling counts against the model.
    #[serde(default = "default_model_degenerate_ceiling")]
    pub model_degenerate_ceiling: f64,
}

impl Default for IngestSection {
    fn default() -> Self {
        Self {
            batch_rows: None,
            chunk_bytes: None,
            finish_coverage_floor: default_finish_coverage_floor(),
            finish_quantity_floor: default_finish_quantity_floor(),
            model_acceptance_floor: default_model_acceptance_floor(),
            model_degenerate_ceiling: default_model_degenerate_ceiling(),
        }
    }
}

/// Challenging a first finish below a quarter of the document is the
/// measured-safe default; `0` turns the gate off entirely.
fn default_finish_coverage_floor() -> f64 {
    0.25
}

/// OFF by default (`1.0`), and that is a measured decision.
///
/// The diagnosis holds: across 20 LitXAlloy papers the reader reached 100%
/// coverage on every one, stopped on `finish` (never `budget`) on every one
/// with turns to spare, and recorded about a third of the quantities present.
/// Reading was never the constraint; stopping was, and nothing measured it.
///
/// But the gate does not fix it. Four papers, same binary, on vs off: mean F1
/// 0.4329 vs 0.4381 — 0.005 apart, indistinguishable — while per paper it
/// swung BOTH ways by more than the benchmark's noise floor (+0.108 on one,
/// -0.145 on the one where it fired and claims went 24 to 42). No measured
/// accuracy, added variance, against a design note warning that a model told
/// it must produce more facts will produce false ones. An operator may set a
/// fraction to turn it on.
fn default_finish_quantity_floor() -> f64 {
    1.0
}

/// A healthy frontier model lands far above one third of its proposals; a
/// model below this line in every sample is measurably not reading.
fn default_model_acceptance_floor() -> f64 {
    1.0 / 3.0
}

/// More than half of a sample's facts structurally degenerate (two of
/// subject/predicate/object identical or blank) is beyond noise.
fn default_model_degenerate_ceiling() -> f64 {
    0.5
}

fn is_platform_llm_provider(provider: &str) -> bool {
    matches!(provider, "marc27" | "platform" | "google" | "vertexai")
}

const PLATFORM_INGEST_MODEL: &str = "gemini-3.1-flash-preview";
const PLATFORM_EMBEDDING_MODEL: &str = "gemini-embedding-2";

impl LlmSection {
    /// Resolve the model name — returns a helpful error if not configured.
    pub fn resolve_model(&self) -> Result<String> {
        self.model.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "No LLM model configured. Set one with:\n  \
                 prism configure --model <name>\n\
                 Or pass --model explicitly for this command.\n\
                 Example: prism configure --model gemma-4-E4B-it --url http://localhost:8080"
            )
        })
    }

    /// Resolve the ingest/search model, allowing platform-backed providers to
    /// fall back to the hosted Gemini default when no explicit model is set.
    pub fn resolve_model_or_platform_default(&self) -> Result<String> {
        self.model
            .clone()
            .or_else(|| {
                if is_platform_llm_provider(&self.provider) {
                    Some(PLATFORM_INGEST_MODEL.to_string())
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No LLM model configured. Set one with:\n  \
                 prism configure --model <name>\n\
                 Or pass --model explicitly for this command.\n\
                 Example: prism configure --model gemma-4-E4B-it --url http://localhost:8080"
                )
            })
    }

    /// Resolve the embedding model, defaulting platform-backed flows to the
    /// hosted Gemini embedding model when no explicit override is present.
    pub fn resolve_embedding_model_or_platform_default(&self) -> Option<String> {
        self.embedding_model.clone().or_else(|| {
            if is_platform_llm_provider(&self.provider) {
                Some(PLATFORM_EMBEDDING_MODEL.to_string())
            } else {
                None
            }
        })
    }

    /// Resolve the API key: inline value wins, then env var, then None.
    pub fn resolve_api_key(&self) -> Option<String> {
        self.api_key
            .as_ref()
            .filter(|k| !k.is_empty())
            .cloned()
            .or_else(|| std::env::var(&self.api_key_env).ok())
    }
}

/// Configuration for a managed LLM service (Indexer or Searcher).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelServiceSection {
    /// "managed" (PRISM manages llama-server), "external" (user-provided URI), or "platform" (MARC27 cloud)
    #[serde(default = "default_platform_mode")]
    pub mode: String,
    /// Model identifier (e.g. "marc27/prism-indexer-9b-Q4_K_XL.gguf" or "claude-sonnet-4-6")
    #[serde(default)]
    pub model: Option<String>,
    /// Embedding model (separate from generation model)
    #[serde(default)]
    pub embedding_model: Option<String>,
    /// External URI for mode=external (e.g. "http://gpu-cluster:8000/v1")
    #[serde(default)]
    pub uri: Option<String>,
    /// API key for authenticated providers
    #[serde(default)]
    pub api_key: Option<String>,
    /// API key environment variable name (alternative to embedding key in config)
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Context length for managed llama-server
    #[serde(default = "default_context_length")]
    pub context_length: usize,
    /// GPU layers to offload for managed llama-server
    #[serde(default = "default_gpu_layers")]
    pub gpu_layers: u32,
    /// Local port for managed llama-server
    #[serde(default)]
    pub port: Option<u16>,
}

/// Configuration for CALPHAD thermodynamic calculations.
///
/// Supports local TDB/THCEA databases and MARC27 cloud CALPHAD service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalphadSection {
    /// "local" (pycalphad with local TDB files), "platform" (MARC27 cloud), or "disabled"
    #[serde(default = "default_calphad_mode")]
    pub mode: String,
    /// Paths to local TDB/THCEA database files.
    #[serde(default)]
    pub databases: Vec<String>,
    /// Default database to use for calculations.
    #[serde(default)]
    pub default_database: Option<String>,
}

impl Default for CalphadSection {
    fn default() -> Self {
        Self {
            mode: "local".into(),
            databases: Vec::new(),
            default_database: None,
        }
    }
}

fn default_calphad_mode() -> String {
    "local".into()
}

// ── Defaults ────────────────────────────────────────────────────────

fn default_node_name() -> String {
    hostname().unwrap_or_else(|| "prism-node".into())
}
fn default_port() -> u16 {
    7327
}
fn default_data_dir() -> String {
    "/var/prism/data".into()
}
fn default_managed() -> String {
    "managed".into()
}
fn default_discovery() -> Vec<String> {
    vec!["mdns".into(), "platform".into()]
}
fn default_mesh_port() -> u16 {
    7328
}
fn default_engine() -> String {
    "llm".into()
}
fn default_ontology_id() -> String {
    "emmo".into()
}
fn default_llm_provider() -> String {
    "platform".into()
}
fn default_locality() -> String {
    "auto".into()
}
fn default_session_timeout() -> String {
    "24h".into()
}
fn default_true() -> bool {
    true
}
fn default_platform_mode() -> String {
    "platform".into()
}
fn default_context_length() -> usize {
    4096
}
fn default_gpu_layers() -> u32 {
    99
}

fn hostname() -> Option<String> {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
}

// ── Impl ────────────────────────────────────────────────────────────

impl Default for NodeConfig {
    fn default() -> Self {
        toml::from_str("").unwrap()
    }
}

impl Default for NodeSection {
    fn default() -> Self {
        Self {
            name: default_node_name(),
            port: default_port(),
            data_dir: default_data_dir(),
        }
    }
}

impl Default for ServicesSection {
    fn default() -> Self {
        Self {
            mode: default_managed(),
            kafka_uri: None,
        }
    }
}

impl Default for MeshSection {
    fn default() -> Self {
        Self {
            discovery: default_discovery(),
            publish_port: default_mesh_port(),
        }
    }
}

impl Default for OntologySection {
    fn default() -> Self {
        Self {
            engine: default_engine(),
            id: default_ontology_id(),
            llm_provider: default_llm_provider(),
            mapping_file: None,
            locality: default_locality(),
        }
    }
}

impl Default for AuthSection {
    fn default() -> Self {
        Self {
            session_timeout: default_session_timeout(),
            require_platform_auth: true,
            allow_local_users: true,
        }
    }
}

impl Default for ModelServiceSection {
    fn default() -> Self {
        Self {
            mode: default_platform_mode(),
            model: None,
            embedding_model: None,
            uri: None,
            api_key: None,
            api_key_env: None,
            context_length: default_context_length(),
            gpu_layers: default_gpu_layers(),
            port: None,
        }
    }
}

impl ModelServiceSection {
    /// Resolve a model id for platform-backed service sections.
    pub fn resolve_model_or_platform_default(&self) -> Option<String> {
        self.model.clone().or_else(|| {
            if matches!(self.mode.as_str(), "platform" | "marc27") {
                Some(PLATFORM_INGEST_MODEL.to_string())
            } else {
                None
            }
        })
    }

    /// Resolve an embedding model id for platform-backed service sections.
    pub fn resolve_embedding_model_or_platform_default(&self) -> Option<String> {
        self.embedding_model.clone().or_else(|| {
            if matches!(self.mode.as_str(), "platform" | "marc27") {
                Some(PLATFORM_EMBEDDING_MODEL.to_string())
            } else {
                None
            }
        })
    }
}

impl NodeConfig {
    /// Load config from a TOML file, falling back to defaults for missing fields.
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config from {}", path.display()))?;
        let config: Self = toml::from_str(&text)
            .with_context(|| format!("failed to parse TOML from {}", path.display()))?;
        Ok(config)
    }

    /// Load config with standard search order: defaults < global < project.
    ///
    /// Every diagnostic goes to stderr. Losing a hand-written override
    /// silently is worse than losing it loudly — `providers.rs` and
    /// `chat_config.rs` already follow that rule; this loader did not.
    pub fn load(project_root: Option<&Path>) -> Self {
        let global = std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".prism").join("prism.toml"));
        let root = project_root
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let project = root.join(".prism").join("prism.toml");
        let (config, diagnostics) = Self::load_from_paths(global.as_deref(), Some(&project));
        for line in diagnostics {
            eprintln!("warning: {line}");
        }
        config
    }

    /// The loader behind [`Self::load`], with its diagnostics returned rather
    /// than printed, so the two things it must get right are testable.
    ///
    /// A file that does not parse is REPORTED, not skipped. It was `if let
    /// Ok(..)` with no `Err` arm: one stray comma silently discarded the whole
    /// file, and with it `[ontology] id`, so every fact in the run was
    /// classified against the default vocabulary and stamped with its version
    /// IRI in provenance — certifying the wrong answer, with no diagnostic.
    ///
    /// A project file overrides the global file SECTION BY SECTION. It was
    /// whole-struct replacement (`config = pc`) behind a comment promising a
    /// merge: because every section is `#[serde(default)]`, a project file
    /// containing only `[ingest]` parsed cleanly and erased `[llm]`,
    /// `[ontology]`, `[auth]` and `[platform]` from the global file. Merging
    /// at the TOML table's top level gives exactly what the search order
    /// promised — a key a file states wins; a key it omits is inherited.
    pub fn load_from_paths(global: Option<&Path>, project: Option<&Path>) -> (Self, Vec<String>) {
        let mut table = toml::Table::new();
        let mut diagnostics = Vec::new();
        for (label, path) in [("global", global), ("project", project)] {
            let Some(path) = path else { continue };
            if !path.exists() {
                continue;
            }
            let parsed = std::fs::read_to_string(path)
                .map_err(anyhow::Error::from)
                .and_then(|text| toml::from_str::<toml::Table>(&text).map_err(Into::into));
            match parsed {
                Ok(layer) => {
                    table.extend(layer);
                    tracing::debug!(path = %path.display(), "loaded {label} config");
                }
                Err(error) => diagnostics.push(format!(
                    "ignoring {label} config {}: {error:#} — every setting in that file is \
                     being IGNORED, including [ontology] and [llm]",
                    path.display()
                )),
            }
        }
        let config = match table.try_into::<Self>() {
            Ok(config) => config,
            Err(error) => {
                diagnostics.push(format!(
                    "merged config does not fit NodeConfig: {error:#} — falling back to defaults"
                ));
                Self::default()
            }
        };
        (config, diagnostics)
    }

    /// Resolve the API key for a model service section, checking env vars.
    pub fn resolve_api_key(section: &ModelServiceSection) -> Option<String> {
        // Direct key takes precedence
        if let Some(ref key) = section.api_key
            && !key.is_empty()
        {
            return Some(key.clone());
        }
        // Fall back to env var
        if let Some(ref env_name) = section.api_key_env
            && let Ok(key) = std::env::var(env_name)
            && !key.is_empty()
        {
            return Some(key);
        }
        // Fall back to LLM_API_KEY
        std::env::var("LLM_API_KEY").ok().filter(|k| !k.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One stray comma must not silently revert the active ontology.
    #[test]
    fn a_malformed_config_file_is_reported_not_silently_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("prism.toml");
        std::fs::write(&bad, "[ontology]\nid = \"custom\",\n").unwrap();
        let (config, diagnostics) = NodeConfig::load_from_paths(Some(&bad), None);
        assert_eq!(config.ontology.id, NodeConfig::default().ontology.id);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].contains("prism.toml"), "{}", diagnostics[0]);
        assert!(diagnostics[0].contains("IGNORED"), "{}", diagnostics[0]);
    }

    /// A project file that states only `[node]` must not erase the global
    /// `[ontology]` — that is the whole-struct replacement that reverted
    /// every run to the default vocabulary.
    #[test]
    fn a_project_file_overrides_only_the_keys_it_states() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.toml");
        let project = dir.path().join("project.toml");
        std::fs::write(
            &global,
            "[node]\nport = 7001\n\n[ontology]\nid = \"custom-ontology\"\n",
        )
        .unwrap();
        std::fs::write(&project, "[node]\nport = 9002\n").unwrap();
        let (config, diagnostics) = NodeConfig::load_from_paths(Some(&global), Some(&project));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(
            config.ontology.id, "custom-ontology",
            "a section the project file does not mention must be inherited"
        );
        assert_eq!(
            config.node.port, 9002,
            "a section the project file states must win"
        );
    }

    #[test]
    fn default_config_is_valid() {
        let config = NodeConfig::default();
        assert_eq!(config.node.port, 7327);
        assert_eq!(config.services.mode, "managed");
        assert_eq!(config.platform.url, None);
        assert_eq!(config.platform.provider, None);
        assert_eq!(config.ontology.engine, "llm");
        assert_eq!(config.ontology.id, "emmo");
        assert_eq!(config.ontology.llm_provider, "platform");
        assert_eq!(config.indexer.mode, "platform");
        assert_eq!(config.searcher.mode, "platform");
    }

    #[test]
    fn replay_reasoning_content_is_on_unless_the_operator_says_no() {
        let config: NodeConfig = toml::from_str("[llm]\nmodel = \"m\"\n").unwrap();
        assert!(
            config.llm.replay_reasoning_content,
            "on by default: the model keeps its reasoning"
        );
        let config: NodeConfig =
            toml::from_str("[llm]\nreplay_reasoning_content = false\n").unwrap();
        assert!(
            !config.llm.replay_reasoning_content,
            "the operator turned it off"
        );
    }

    #[test]
    fn audit_enabled_by_default() {
        let config = NodeConfig::default();
        assert!(config.audit.enabled);
        // Absent [audit] section still defaults to on.
        let config: NodeConfig = toml::from_str("[node]\nname = \"x\"\n").unwrap();
        assert!(config.audit.enabled);
    }

    #[test]
    fn audit_can_be_opted_out() {
        let config: NodeConfig = toml::from_str("[audit]\nenabled = false\n").unwrap();
        assert!(!config.audit.enabled);
    }

    /// The paper-loop policy keys live in `[ingest]` and carry their
    /// defaults when absent — including an `[ingest]` section that sets
    /// OTHER knobs only. The gate must come from config, never from a
    /// hardcoded number in the loop.
    #[test]
    fn ingest_reading_policy_defaults_and_overrides() {
        let config: NodeConfig = toml::from_str("[ingest]\nchunk_bytes = 2000\n").unwrap();
        assert_eq!(config.ingest.finish_coverage_floor, 0.25);
        assert_eq!(config.ingest.model_acceptance_floor, 1.0 / 3.0);
        assert_eq!(config.ingest.model_degenerate_ceiling, 0.5);

        let config: NodeConfig = toml::from_str(
            "[ingest]\nfinish_coverage_floor = 0.0\nmodel_acceptance_floor = 0.5\n\
             model_degenerate_ceiling = 0.75\n",
        )
        .unwrap();
        assert_eq!(
            config.ingest.finish_coverage_floor, 0.0,
            "0 disables the gate"
        );
        assert_eq!(config.ingest.model_acceptance_floor, 0.5);
        assert_eq!(config.ingest.model_degenerate_ceiling, 0.75);

        let config = NodeConfig::default();
        assert_eq!(config.ingest.finish_coverage_floor, 0.25);
    }

    #[test]
    fn parse_minimal_toml() {
        let toml = r#"
[node]
name = "my-lab"
port = 8000
"#;
        let config: NodeConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.node.name, "my-lab");
        assert_eq!(config.node.port, 8000);
        // Other sections get defaults
        assert_eq!(config.services.mode, "managed");
        assert_eq!(config.platform.url, None);
        assert_eq!(config.platform.provider, None);
    }

    #[test]
    fn parse_full_toml() {
        let toml = r#"
[node]
name = "lab-alpha"
port = 7327
data_dir = "/data/prism"

[services]
mode = "external"
kafka_uri = "kafka://broker.internal:9092"

[platform]
url = "https://platform.marc27.com"
provider = "marc27"

[mesh]
discovery = ["mdns", "platform"]
publish_port = 7328

[ontology]
engine = "llm"
llm_provider = "platform"
mapping_file = "mappings/materials.yaml"

[auth]
session_timeout = "24h"
require_platform_auth = true
allow_local_users = true

[indexer]
mode = "managed"
model = "marc27/prism-indexer-9b-Q4_K_XL.gguf"
embedding_model = "nomic-embed-text"
context_length = 4096
gpu_layers = 99
port = 8100

[searcher]
mode = "platform"
model = "claude-sonnet-4-6"
api_key_env = "ANTHROPIC_API_KEY"
"#;
        let config: NodeConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.services.mode, "external");
        assert_eq!(
            config.services.kafka_uri.as_deref(),
            Some("kafka://broker.internal:9092")
        );
        assert_eq!(
            config.indexer.model.as_deref(),
            Some("marc27/prism-indexer-9b-Q4_K_XL.gguf")
        );
        assert_eq!(config.indexer.port, Some(8100));
        assert_eq!(config.searcher.mode, "platform");
        assert_eq!(
            config.searcher.api_key_env.as_deref(),
            Some("ANTHROPIC_API_KEY")
        );
        assert_eq!(
            config.ontology.mapping_file.as_deref(),
            Some("mappings/materials.yaml")
        );
        assert_eq!(
            config.platform.url.as_deref(),
            Some("https://platform.marc27.com")
        );
        assert_eq!(config.platform.provider.as_deref(), Some("marc27"));
        // No `id` in the [ontology] block above → the default ontology.
        assert_eq!(config.ontology.id, "emmo");
    }

    /// Corporate-separation guard: a fresh PRISM install does not silently
    /// select another company's service. This fails if any hosted default is
    /// reintroduced, including under a different hostname.
    #[test]
    fn default_platform_is_explicitly_unconfigured() {
        let config = NodeConfig::default();
        assert!(config.platform.url.is_none());
        assert!(config.platform.provider.is_none());
    }

    /// The `[ontology] id` knob parses, and its absence means the built-in
    /// default — existing configs select exactly what they always got.
    #[test]
    fn ontology_id_knob_parses_and_defaults_to_emmo() {
        let config: NodeConfig = toml::from_str("[ontology]\nid = \"chem\"\n").unwrap();
        assert_eq!(config.ontology.id, "chem");
        let config: NodeConfig = toml::from_str("").unwrap();
        assert_eq!(config.ontology.id, "emmo");
    }

    #[test]
    fn resolve_api_key_from_section() {
        let section = ModelServiceSection {
            api_key: Some("direct-key".into()),
            ..Default::default()
        };
        assert_eq!(
            NodeConfig::resolve_api_key(&section),
            Some("direct-key".into())
        );
    }

    #[test]
    fn resolve_api_key_empty_string_falls_through() {
        let section = ModelServiceSection {
            api_key: Some("".into()),
            ..Default::default()
        };
        // Empty key falls through to env var check
        assert_eq!(
            NodeConfig::resolve_api_key(&section),
            std::env::var("LLM_API_KEY").ok().filter(|k| !k.is_empty())
        );
    }

    #[test]
    fn llm_section_platform_defaults_kick_in() {
        let section = LlmSection {
            provider: "marc27".into(),
            ..Default::default()
        };
        assert_eq!(
            section.resolve_model_or_platform_default().unwrap(),
            "gemini-3.1-flash-preview"
        );
        assert_eq!(
            section
                .resolve_embedding_model_or_platform_default()
                .as_deref(),
            Some("gemini-embedding-2")
        );
    }

    #[test]
    fn model_service_platform_defaults_kick_in() {
        let section = ModelServiceSection {
            mode: "platform".into(),
            ..Default::default()
        };
        assert_eq!(
            section.resolve_model_or_platform_default().as_deref(),
            Some("gemini-3.1-flash-preview")
        );
        assert_eq!(
            section
                .resolve_embedding_model_or_platform_default()
                .as_deref(),
            Some("gemini-embedding-2")
        );
    }
}
