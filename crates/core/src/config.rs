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
    pub indexer: ModelServiceSection,
    #[serde(default)]
    pub searcher: ModelServiceSection,
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
    /// External Neo4j URI (bolt://host:port) when mode=external
    #[serde(default)]
    pub neo4j_uri: Option<String>,
    /// External Qdrant URI (http://host:port) when mode=external
    #[serde(default)]
    pub qdrant_uri: Option<String>,
    /// External Kafka URI when mode=external
    #[serde(default)]
    pub kafka_uri: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformSection {
    #[serde(default = "default_platform_url")]
    pub url: String,
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
    #[serde(default = "default_llm_provider")]
    pub llm_provider: String,
    /// Custom ontology mapping rules YAML file path.
    #[serde(default)]
    pub mapping_file: Option<String>,
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

// ── Defaults ────────────────────────────────────────────────────────

fn default_node_name() -> String { hostname().unwrap_or_else(|| "prism-node".into()) }
fn default_port() -> u16 { 7327 }
fn default_data_dir() -> String { "/var/prism/data".into() }
fn default_managed() -> String { "managed".into() }
fn default_platform_url() -> String { "https://platform.marc27.com".into() }
fn default_discovery() -> Vec<String> { vec!["mdns".into(), "platform".into()] }
fn default_mesh_port() -> u16 { 7328 }
fn default_engine() -> String { "llm".into() }
fn default_llm_provider() -> String { "platform".into() }
fn default_session_timeout() -> String { "24h".into() }
fn default_true() -> bool { true }
fn default_platform_mode() -> String { "platform".into() }
fn default_context_length() -> usize { 4096 }
fn default_gpu_layers() -> u32 { 99 }

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
        Self { name: default_node_name(), port: default_port(), data_dir: default_data_dir() }
    }
}

impl Default for ServicesSection {
    fn default() -> Self {
        Self { mode: default_managed(), neo4j_uri: None, qdrant_uri: None, kafka_uri: None }
    }
}

impl Default for PlatformSection {
    fn default() -> Self { Self { url: default_platform_url() } }
}

impl Default for MeshSection {
    fn default() -> Self { Self { discovery: default_discovery(), publish_port: default_mesh_port() } }
}

impl Default for OntologySection {
    fn default() -> Self { Self { engine: default_engine(), llm_provider: default_llm_provider(), mapping_file: None } }
}

impl Default for AuthSection {
    fn default() -> Self {
        Self { session_timeout: default_session_timeout(), require_platform_auth: true, allow_local_users: true }
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
    pub fn load(project_root: Option<&Path>) -> Self {
        let mut config = Self::default();

        // Global: ~/.prism/prism.toml
        if let Some(home) = std::env::var_os("HOME") {
            let global = PathBuf::from(home).join(".prism").join("prism.toml");
            if global.exists() {
                if let Ok(gc) = Self::from_file(&global) {
                    config = gc;
                    tracing::debug!(path = %global.display(), "loaded global config");
                }
            }
        }

        // Project: .prism/prism.toml or <project_root>/.prism/prism.toml
        let root = project_root
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let project = root.join(".prism").join("prism.toml");
        if project.exists() {
            if let Ok(pc) = Self::from_file(&project) {
                // Merge: project overrides global (simple: just replace)
                config = pc;
                tracing::debug!(path = %project.display(), "loaded project config");
            }
        }

        config
    }

    /// Resolve the API key for a model service section, checking env vars.
    pub fn resolve_api_key(section: &ModelServiceSection) -> Option<String> {
        // Direct key takes precedence
        if let Some(ref key) = section.api_key {
            if !key.is_empty() {
                return Some(key.clone());
            }
        }
        // Fall back to env var
        if let Some(ref env_name) = section.api_key_env {
            if let Ok(key) = std::env::var(env_name) {
                if !key.is_empty() {
                    return Some(key);
                }
            }
        }
        // Fall back to LLM_API_KEY
        std::env::var("LLM_API_KEY").ok().filter(|k| !k.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = NodeConfig::default();
        assert_eq!(config.node.port, 7327);
        assert_eq!(config.services.mode, "managed");
        assert_eq!(config.platform.url, "https://platform.marc27.com");
        assert_eq!(config.ontology.engine, "llm");
        assert_eq!(config.ontology.llm_provider, "platform");
        assert_eq!(config.indexer.mode, "platform");
        assert_eq!(config.searcher.mode, "platform");
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
        assert_eq!(config.platform.url, "https://platform.marc27.com");
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
neo4j_uri = "bolt://db.internal:7687"
qdrant_uri = "http://vectors.internal:6333"

[platform]
url = "https://platform.marc27.com"

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
        assert_eq!(config.services.neo4j_uri.as_deref(), Some("bolt://db.internal:7687"));
        assert_eq!(config.indexer.model.as_deref(), Some("marc27/prism-indexer-9b-Q4_K_XL.gguf"));
        assert_eq!(config.indexer.port, Some(8100));
        assert_eq!(config.searcher.mode, "platform");
        assert_eq!(config.searcher.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(config.ontology.mapping_file.as_deref(), Some("mappings/materials.yaml"));
    }

    #[test]
    fn resolve_api_key_from_section() {
        let section = ModelServiceSection {
            api_key: Some("direct-key".into()),
            ..Default::default()
        };
        assert_eq!(NodeConfig::resolve_api_key(&section), Some("direct-key".into()));
    }

    #[test]
    fn resolve_api_key_empty_string_falls_through() {
        let section = ModelServiceSection {
            api_key: Some("".into()),
            ..Default::default()
        };
        // Empty key falls through to env var check
        assert_eq!(NodeConfig::resolve_api_key(&section), std::env::var("LLM_API_KEY").ok().filter(|k| !k.is_empty()));
    }
}
