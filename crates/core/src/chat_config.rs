//! `~/.prism/config.toml` — the user-visible configuration file PRISM owns.
//!
//! Right now it stores only one thing: the **chat target** (where chat
//! turns get sent). Three first-class options:
//!
//!   - **Hosted platform** (the zero-setup default) — chat goes through
//!     the platform's proxy, which fronts hundreds of hosted models. The
//!     user picks WHICH model the platform should serve. **The platform's
//!     own internal vendor keys never leave its backend.** PRISM does not
//!     see them.
//!   - **Local LLM** — chat goes to a user-supplied OpenAI-compatible URL
//!     (Ollama at `:11434/v1`, llama.cpp `--server`, vLLM, etc.). No
//!     keys leave the user's machine — strictly local.
//!   - **Direct provider** — chat goes straight to any vendor in the
//!     [`crate::providers`] registry (OpenAI, Anthropic, Google, Groq,
//!     OpenRouter, …) using **the user's OWN API key**, read from a named
//!     env var at request time and never persisted to disk. This is the
//!     user's choice; their keys, their call. The hygiene rule that
//!     matters is that the *platform's* own keys stay on the platform.
//!
//! The chat target is **independent** of the platform's tools, and the
//! reverse is also true: chat works with no account at all via the other
//! two options. Platform tools (knowledge graph, discourse, marketplace)
//! need `prism login`; local tools, notebooks and workflows do not. That
//! separation is the whole point — PRISM is a complete product without
//! the platform, and the platform is the easiest way to run it, not a
//! requirement.
//!
//! The `mode = "marc27"` value serialized into this file is a **frozen
//! wire identifier** — existing installs on disk depend on it — so it
//! does not move when the platform's display name changes. See
//! [`crate::brand`].

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
    /// `None`, PRISM falls back to its compiled-in default. Stored here so
    /// the same session can be reproduced across restarts and so `/use
    /// marc27 --model x` can swap the model mid-session.
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
    ///
    /// `dead_code` allowed: lands in the follow-up native
    /// `AppCommand::Use` slash-command implementation. Keeping the
    /// method on the enum so that PR is purely additive (no
    /// changes to chat_config).
    #[allow(dead_code)]
    pub fn label(&self) -> String {
        match self {
            Self::Marc27 { .. } => crate::brand::brand().platform_name.clone(),
            Self::Local { .. } => "local".to_string(),
            Self::Provider { .. } => "direct provider".to_string(),
        }
    }

    /// Long-form rendering used by `/use show` and boot status. Includes
    /// model name and any user-visible target hint.
    ///
    /// The platform's name comes from `brand.toml` (see [`crate::brand`]),
    /// not a literal — this string is on the boot screen and in every
    /// `/use show`, so it was one of the most-repeated brand spellings in
    /// the tree. The `marc27` serde tag underneath is a frozen wire
    /// identifier and does NOT move with the brand.
    pub fn human_full(&self) -> String {
        let platform = &crate::brand::brand().platform_name;
        match self {
            Self::Marc27 { model } => match model {
                Some(m) => format!("{platform} ({m})"),
                None => platform.clone(),
            },
            Self::Local { url, model, .. } => format!("local ({url}, {model})"),
            Self::Provider {
                provider, model, ..
            } => format!("{provider} ({model})"),
        }
    }

    // NOTE: `default_api_key_env` used to live here as a hardcoded
    // `match` over five vendors. It now lives in
    // [`crate::providers::default_api_key_env`], reading the shipped
    // `providers.toml` plus the user's `~/.prism/providers.toml`
    // override — so adding a vendor, or renaming the env var one of them
    // uses, is a config edit rather than a release.
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
    if let Ok(override_path) = std::env::var("PRISM_CONFIG_PATH") {
        return Ok(PathBuf::from(override_path));
    }
    let home = std::env::var_os("HOME").context("HOME env var not set")?;
    Ok(PathBuf::from(home).join(".prism").join("config.toml"))
}

/// Load config; if the file is missing or malformed, return the
/// default (the hosted platform) rather than erroring. A malformed file is
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

/// Has the user actually chosen where chat goes?
///
/// [`load`] cannot answer this. It returns the hosted-platform default for
/// a missing file, which is byte-identical to what a user who deliberately
/// chose the hosted platform gets — so "did they decide?" and "what did
/// they decide?" are two different questions and only one of them has an
/// answer in `PrismConfig`.
///
/// Onboarding needs the first one. It used to ask whether *platform
/// credentials* existed, which is a different question again: someone who
/// picked "own key" or a local server never gets credentials, so they were
/// shown the three-step first-run wizard on every single launch. That is
/// the sharpest possible contradiction of "PRISM works standalone".
pub fn chat_target_is_configured() -> bool {
    config_path()
        .map(|p| target_is_configured_at(&p))
        .unwrap_or(false)
}

/// [`chat_target_is_configured`] against an explicit path, so it is
/// testable without mutating `$HOME` out from under other tests.
///
/// A file with no `[chat]` table is NOT a choice — `serde(default)` would
/// silently turn it into the hosted-platform default, which is exactly the
/// ambiguity this function exists to remove. A malformed file, on the other
/// hand, is a user who chose and then broke it: re-running the wizard would
/// overwrite their intent, so it counts as configured and `load` reports
/// the parse error instead.
fn target_is_configured_at(path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    match toml::from_str::<toml::Value>(&raw) {
        Ok(value) => value.get("chat").and_then(|c| c.get("mode")).is_some(),
        Err(_) => true,
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

/// Write `bytes` to `path` atomically and owner-only.
///
/// 0600 because this file can hold a secret: `prism use local --api-key
/// sk-…` persists that key verbatim in the `[chat]` table. Plain
/// `std::fs::write` inherited the umask — 0644 on most distros — leaving
/// any other local user able to read it, while `credentials.json` next door
/// was already 0600 for exactly the same reason.
///
/// The mode is set when the temp file is CREATED, not on the final path
/// after the rename: fixing it afterwards leaves a window in which the
/// finished file is world-readable. It is unconditional rather than
/// "only when a key is present" — a predicate over which fields count as
/// secret is one new field away from being wrong, and nothing but PRISM
/// reads this file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    // `mode` only applies to a file this call CREATES, so a 0644 temp file
    // left behind by an earlier crash would otherwise be reused as-is and
    // renamed into place still world-readable.
    let _ = std::fs::remove_file(&tmp);
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("writing {}", tmp.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    }
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
        // Asserted against `brand.toml`, not a literal: this is what
        // makes a company rename a one-file edit instead of a test sweep.
        let platform = &crate::brand::brand().platform_name;
        assert_eq!(
            ChatTarget::Marc27 { model: None }.human_full(),
            platform.as_str()
        );
        assert_eq!(
            ChatTarget::Marc27 {
                model: Some("gpt-5.5".to_string())
            }
            .human_full(),
            format!("{platform} (gpt-5.5)")
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

    /// `prism use local --api-key sk-…` puts a live secret in this file, in
    /// plaintext. It was written with a plain `fs::write`, so it inherited
    /// the umask — 0644 on most distros — and any other local user could
    /// read the key, while `credentials.json` next door was already 0600.
    #[test]
    #[cfg(unix)]
    fn config_holding_an_api_key_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        let cfg = PrismConfig {
            chat: ChatTarget::Local {
                url: "http://localhost:11434/v1".into(),
                model: "llama-3.1-70b".into(),
                api_key: Some("sk-super-secret".into()),
            },
        };
        write_atomic(&path, toml::to_string_pretty(&cfg).unwrap().as_bytes()).unwrap();

        // The key really is in there — this is what makes the mode matter.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("sk-super-secret"),
            "the key is persisted verbatim, so the file is a secret"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "config.toml can hold an API key and must be owner-only"
        );
    }

    /// The onboarding predicate. Nothing on disk ⇒ nobody has chosen ⇒ the
    /// wizard is genuinely a first launch.
    #[test]
    fn no_config_file_means_no_choice_has_been_made() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!target_is_configured_at(&tmp.path().join("config.toml")));
    }

    /// THE defect: a standalone user. `choose_provider` (own key) and
    /// `choose_local_model` (a server on this machine) both write a chat
    /// target and never produce platform credentials — so onboarding, which
    /// keyed on credentials, ran again on every launch.
    #[test]
    fn choosing_own_key_or_a_local_server_counts_as_configured() {
        let tmp = tempfile::tempdir().unwrap();
        for target in [
            ChatTarget::Provider {
                provider: "anthropic".into(),
                model: "claude-sonnet-4".into(),
                api_key_env: None,
            },
            ChatTarget::Local {
                url: "http://localhost:11434/v1".into(),
                model: "llama-3.1-70b".into(),
                api_key: None,
            },
            ChatTarget::Marc27 {
                model: Some("gpt-5.5".into()),
            },
        ] {
            let path = tmp.path().join("config.toml");
            let cfg = PrismConfig {
                chat: target.clone(),
            };
            write_atomic(&path, toml::to_string_pretty(&cfg).unwrap().as_bytes()).unwrap();
            assert!(
                target_is_configured_at(&path),
                "{target:?} is a choice the user made"
            );
        }
    }

    /// A file that exists but says nothing about chat is not a choice —
    /// `serde(default)` would quietly render it as the hosted platform,
    /// which is the ambiguity this predicate exists to remove.
    #[test]
    fn a_file_without_a_chat_table_is_not_a_choice() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        for body in ["", "# nothing here\n", "[something_else]\nkey = 1\n"] {
            std::fs::write(&path, body).unwrap();
            assert!(
                !target_is_configured_at(&path),
                "{body:?} should not count as a configured chat target"
            );
        }
    }

    /// A user who chose and then broke the file has still chosen. Re-running
    /// the wizard would overwrite their intent; `load` already reports the
    /// parse error loudly.
    #[test]
    fn a_malformed_file_still_counts_as_a_choice() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "[chat\nmode = broken").unwrap();
        assert!(target_is_configured_at(&path));
    }

    /// A 0644 temp file left by an earlier crash must not be reused and
    /// renamed into place still world-readable — `OpenOptions::mode` only
    /// applies to a file the call creates.
    #[test]
    #[cfg(unix)]
    fn a_stale_world_readable_temp_file_cannot_leak_the_key() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        let stale = path.with_extension("toml.tmp");
        std::fs::write(&stale, b"leftover").unwrap();
        std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_atomic(&path, b"chat = {}\n").unwrap();

        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    /// Reduce a source file to the prose a reader actually sees, so a claim
    /// is still found when it is line-wrapped across comment markers or
    /// `println!` calls.
    ///
    /// Without this the check is inert: in `providers.toml` the promise is
    /// split as `…PRISM never writes your\n# key to disk.`, and in
    /// `onboarding.rs` as `…PRISM never");\n println!("  writes it to
    /// disk.`. A plain `contains` matches neither, which is precisely the
    /// kind of test that passes while the defect ships.
    fn user_visible_prose(text: &str) -> String {
        let mut s = text.to_string();
        for scaffold in [
            "println!", "print!", "//!", "///", "#", "(", ")", "\"", ";", "\\n", "—",
        ] {
            s = s.replace(scaffold, " ");
        }
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The shipped text must not promise something the code does not do.
    /// `ChatTarget::Local` has an `api_key` field that `prism use local
    /// --api-key` fills and `save` persists, so an unqualified "never writes
    /// your key to disk" is false wherever a user can read it.
    #[test]
    fn shipped_text_makes_no_blanket_never_writes_your_key_promise() {
        const BANNED: &[&str] = &[
            "PRISM never writes your key to disk",
            "PRISM never writes it to disk",
            "never stored here or anywhere else by PRISM",
        ];
        for (what, text) in [
            ("providers.toml", include_str!("../providers.toml")),
            ("providers.rs", include_str!("providers.rs")),
            ("onboarding.rs", include_str!("../../cli/src/onboarding.rs")),
        ] {
            let prose = user_visible_prose(text);
            for banned in BANNED {
                assert!(
                    !prose.contains(banned),
                    "{what} still tells the user \"{banned}\", which \
                     `prism use local --api-key` makes false"
                );
            }
        }
    }

    /// Guards the guard: prove the normaliser really does see a promise that
    /// is wrapped the way each of these files wraps it. A `contains` over
    /// the raw text matches none of these.
    #[test]
    fn the_prose_normaliser_sees_wrapped_claims() {
        for wrapped in [
            // providers.toml style
            "# key, read from the named env var at request time. PRISM never writes your\n\
             # key to disk.",
            // onboarding.rs style
            "    println!(\"  ... at request time — PRISM never\");\n\
                 println!(\"  writes it to disk. Local servers need no key.\");",
        ] {
            let prose = user_visible_prose(wrapped);
            assert!(
                prose.contains("PRISM never writes your key to disk")
                    || prose.contains("PRISM never writes it to disk"),
                "the normaliser missed a wrapped claim, so the honesty test \
                 would be inert:\n{prose}"
            );
        }
    }
}
