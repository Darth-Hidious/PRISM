//! `~/.prism/config.toml` — the user-visible configuration file PRISM owns.
//!
//! Right now it stores only one thing: the **chat target** (where chat
//! turns get sent). Three first-class options:
//!
//!   - **MARC27 cloud** (default) — chat goes through MARC27's platform
//!     proxy (`api.marc27.com`), which fronts ~590 hosted models. The
//!     user picks WHICH model MARC27 should serve (`gpt-5.5`,
//!     `claude-sonnet-4`, `mistral-large-latest`, …). **MARC27's own
//!     internal vendor keys never leave the platform backend.** PRISM
//!     does not see them.
//!   - **Local LLM** — chat goes to a user-supplied OpenAI-compatible URL
//!     (Ollama at `:11434/v1`, llama.cpp `--server`, vLLM, etc.). No
//!     keys leave the user's machine — strictly local.
//!   - **Direct provider** — chat goes straight to a vendor (Anthropic
//!     / OpenAI / Mistral / …) using **the user's OWN API key**, which
//!     PRISM reads from a named env var at request time and never
//!     persists to disk. This is the user's choice; their keys, their
//!     call. The hygiene rule that matters is that *MARC27's* own
//!     platform keys stay on MARC27.
//!
//! The chat target is **independent** of MARC27 platform tools. MARC27
//! tools (knowledge graph, discourse, marketplace, materials project,
//! …) work regardless of which chat target is selected, so long as
//! `prism login` was run at some point. This lets a user run a local
//! llama for chat while still pulling tool results from MARC27's
//! materials-science backbone — that's the whole point of decoupling.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Where chat turns are routed. Read at boot, hot-swappable at runtime
/// via `prism use ...` or the in-chat `/use` slash command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum ChatTarget {
    /// Default. Chat routed via MARC27 platform proxy. Token comes from
    /// `~/.prism/credentials.json` written by `prism login` — we don't
    /// store the token in this file (rotation, security).
    ///
    /// `model` is the upstream model id MARC27 should serve us
    /// (`gpt-5.5`, `claude-sonnet-4`, `mistral-large-latest`, …). When
    /// `None`, PRISM falls back to its compiled-in default
    /// (`forge_chat::DEFAULT_MODEL_ID`). Stored here so the same
    /// session can be reproduced across restarts and so `/use marc27
    /// --model x` can swap mid-session without forge needing its own
    /// model state.
    Marc27 {
        #[serde(default)]
        model: Option<String>,
    },

    /// Chat routed to an OpenAI-compatible local server. The model name
    /// is whatever the local server advertises; we pass it through
    /// untouched.
    Local {
        url: String,
        model: String,
        /// Some local servers (vLLM in serve mode, some Ollama configs)
        /// want a token. Most accept any non-empty string. None means
        /// "send no Authorization header".
        #[serde(default)]
        api_key: Option<String>,
    },

    /// Chat routed direct to a vendor. The provider's API key is read
    /// from the named env var at request time — we never persist the
    /// key in this file.
    Provider {
        provider: String,
        model: String,
        /// Name of the env var holding the API key (e.g.
        /// `ANTHROPIC_API_KEY`). Inferred from `provider` if not set.
        #[serde(default)]
        api_key_env: Option<String>,
    },
}

impl Default for ChatTarget {
    fn default() -> Self {
        Self::Marc27 { model: None }
    }
}

impl ChatTarget {
    /// Short label for boot UI / `/use show` / status bar. Always lower-
    /// case, no model name (see `human_full` for that).
    pub fn label(&self) -> &'static str {
        match self {
            Self::Marc27 { .. } => "MARC27 cloud",
            Self::Local { .. } => "local",
            Self::Provider { .. } => "direct provider",
        }
    }

    /// Long-form rendering used by `/use show` and boot status. Includes
    /// model name and any user-visible target hint.
    pub fn human_full(&self) -> String {
        match self {
            Self::Marc27 { model } => match model {
                Some(m) => format!("MARC27 cloud ({m})"),
                None => "MARC27 cloud".to_string(),
            },
            Self::Local { url, model, .. } => format!("local ({url}, {model})"),
            Self::Provider {
                provider, model, ..
            } => format!("{provider} ({model})"),
        }
    }

    /// Default API-key env var for a provider when the user didn't
    /// override it. Centralises the "what env var holds the key" rule
    /// so we can update it once if a vendor renames theirs.
    pub fn default_api_key_env(provider: &str) -> &'static str {
        match provider.to_ascii_lowercase().as_str() {
            "anthropic" => "ANTHROPIC_API_KEY",
            "openai" => "OPENAI_API_KEY",
            "mistral" => "MISTRAL_API_KEY",
            "google" | "gemini" => "GEMINI_API_KEY",
            "cohere" => "COHERE_API_KEY",
            // Catch-all — caller can still set api_key_env explicitly.
            _ => "PRISM_PROVIDER_API_KEY",
        }
    }
}

/// The whole `~/.prism/config.toml` file. Today only `chat` is here;
/// keeping it as a struct so future fields (preferred default model,
/// telemetry opt-out, etc.) don't break wire compat.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrismConfig {
    #[serde(default)]
    pub chat: ChatTarget,
}

/// Resolve `~/.prism/config.toml` from `$HOME`. Returns the path even
/// if the file doesn't exist yet — caller decides whether to create.
pub fn config_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME env var not set")?;
    Ok(PathBuf::from(home).join(".prism").join("config.toml"))
}

/// Load config; if the file is missing or malformed, return the
/// default (MARC27 cloud) rather than erroring. A malformed file is
/// not fatal — the user can fix it via `prism use ...` and we'll
/// rewrite cleanly. Logging the parse error is the right balance
/// between "loud" (panic) and "silent" (forget the user's setting).
pub fn load() -> Result<PrismConfig> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(PrismConfig::default());
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    match toml::from_str::<PrismConfig>(&raw) {
        Ok(cfg) => Ok(cfg),
        Err(e) => {
            eprintln!(
                "\x1b[33m[prism]\x1b[0m config at {} is malformed ({}), \
                 falling back to default. Fix it with `prism use show` then \
                 `prism use ...`.",
                path.display(),
                e
            );
            Ok(PrismConfig::default())
        }
    }
}

/// Atomically write the config. Write to a sibling tempfile then rename
/// — rules out half-written files if the process is killed mid-write.
pub fn save(cfg: &PrismConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let raw = toml::to_string_pretty(cfg).context("serialising config")?;
    write_atomic(&path, raw.as_bytes())?;
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marc27_is_default() {
        assert_eq!(ChatTarget::default(), ChatTarget::Marc27 { model: None });
        assert_eq!(
            PrismConfig::default().chat,
            ChatTarget::Marc27 { model: None }
        );
    }

    #[test]
    fn marc27_roundtrip() {
        let cfg = PrismConfig {
            chat: ChatTarget::Marc27 {
                model: Some("gpt-5.5".to_string()),
            },
        };
        let raw = toml::to_string_pretty(&cfg).unwrap();
        let back: PrismConfig = toml::from_str(&raw).unwrap();
        assert_eq!(back.chat, cfg.chat);
    }

    #[test]
    fn local_roundtrip() {
        let cfg = PrismConfig {
            chat: ChatTarget::Local {
                url: "http://localhost:11434/v1".into(),
                model: "llama-3.1-70b".into(),
                api_key: None,
            },
        };
        let raw = toml::to_string_pretty(&cfg).unwrap();
        let back: PrismConfig = toml::from_str(&raw).unwrap();
        assert_eq!(back.chat, cfg.chat);
    }

    #[test]
    fn provider_roundtrip() {
        let cfg = PrismConfig {
            chat: ChatTarget::Provider {
                provider: "anthropic".into(),
                model: "claude-sonnet-4".into(),
                api_key_env: None,
            },
        };
        let raw = toml::to_string_pretty(&cfg).unwrap();
        let back: PrismConfig = toml::from_str(&raw).unwrap();
        assert_eq!(back.chat, cfg.chat);
    }

    #[test]
    fn human_full_renders() {
        assert_eq!(
            ChatTarget::Marc27 { model: None }.human_full(),
            "MARC27 cloud"
        );
        assert_eq!(
            ChatTarget::Marc27 {
                model: Some("gpt-5.5".to_string())
            }
            .human_full(),
            "MARC27 cloud (gpt-5.5)"
        );
        let local = ChatTarget::Local {
            url: "http://localhost:11434/v1".into(),
            model: "llama-3.1-70b".into(),
            api_key: None,
        };
        assert_eq!(
            local.human_full(),
            "local (http://localhost:11434/v1, llama-3.1-70b)"
        );
        let provider = ChatTarget::Provider {
            provider: "anthropic".into(),
            model: "claude-sonnet-4".into(),
            api_key_env: None,
        };
        assert_eq!(provider.human_full(), "anthropic (claude-sonnet-4)");
    }

    #[test]
    fn default_api_key_env_known_providers() {
        assert_eq!(
            ChatTarget::default_api_key_env("anthropic"),
            "ANTHROPIC_API_KEY"
        );
        assert_eq!(ChatTarget::default_api_key_env("OpenAI"), "OPENAI_API_KEY");
        assert_eq!(ChatTarget::default_api_key_env("gemini"), "GEMINI_API_KEY");
        assert_eq!(
            ChatTarget::default_api_key_env("some-new-vendor"),
            "PRISM_PROVIDER_API_KEY"
        );
    }
}
