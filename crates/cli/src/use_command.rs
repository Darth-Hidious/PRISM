//! Handler for `prism use ...` (the shell command) and `/use ...`
//! (the in-chat slash command). Both surfaces share this code so the
//! behaviour and output stay identical — the only difference is who
//! parses the args.
//!
//! The handler does three things:
//!   1. Mutate `~/.prism/config.toml` via [`chat_config::save`].
//!   2. Render a short, opinionated summary of the new state — what
//!      changed, what didn't, what the user should know next.
//!   3. (When called from inside a running chat session via the slash
//!      command) signal the in-process platform-bridge router to swap
//!      its `ChatTarget` so the next turn uses the new upstream
//!      without restarting prism. The bridge exposes a setter on the
//!      shared `Arc<RwLock<ChatTarget>>`; we hand the write here.
//!
//! The "what didn't change" line is deliberate: every variant of
//! `prism use` only touches the chat target. Tools, retrieval, login
//! state — all left alone. Saying that explicitly avoids the user
//! thinking `prism use local` logs them out.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::sync::RwLock;

use crate::chat_config::{self, ChatTarget};

/// Subcommand variants. Mirrors the clap enum in main.rs but kept
/// independent so this module doesn't pull in clap-specific types —
/// the slash-command parser builds these manually from a tokenised
/// chat input. Identical semantics either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseAction {
    /// Stay on MARC27 cloud (the default), but pin a specific model
    /// MARC27 should serve. Useful when the user wants gpt-5.5 vs
    /// claude-sonnet-4 vs mistral-large without changing route.
    Marc27 {
        model: Option<String>,
    },
    Local {
        url: String,
        model: String,
        api_key: Option<String>,
    },
    Provider {
        provider: String,
        model: String,
        api_key_env: Option<String>,
    },
    Show,
    Reset,
}

/// Result returned to the caller. Stays as data so the CLI surface and
/// the slash-command surface can render it however they want — the
/// CLI prints it to stdout, the slash command pumps it into a chat
/// turn. Centralising the rendering text in `render` keeps the two
/// surfaces from drifting.
#[derive(Debug, Clone)]
pub struct UseOutcome {
    /// `dead_code` allowed: read by the native `AppCommand::Use`
    /// follow-up to update the boot status bar after a hot-swap. The
    /// CLI surface today only renders `message`; the slash-command
    /// surface will read both.
    #[allow(dead_code)]
    pub new_target: ChatTarget,
    pub message: String,
}

/// Apply the action. If `live_target` is `Some`, hot-swaps the running
/// bridge so the next chat turn uses the new upstream. Pass `None`
/// when running before prism boots (the shell `prism use` path) —
/// config is saved and the next launch picks it up.
pub async fn apply(
    action: UseAction,
    live_target: Option<&Arc<RwLock<ChatTarget>>>,
    marc27_logged_in: bool,
) -> Result<UseOutcome> {
    let mut cfg = chat_config::load().unwrap_or_default();

    let next = match action {
        UseAction::Marc27 { model } => ChatTarget::Marc27 { model },
        UseAction::Local {
            url,
            model,
            api_key,
        } => {
            validate_url(&url)?;
            ChatTarget::Local {
                url,
                model,
                api_key,
            }
        }
        UseAction::Provider {
            provider,
            model,
            api_key_env,
        } => {
            let provider = provider.to_ascii_lowercase();
            // Resolve which env var to read at request time. We never
            // store the key itself, only the env var name. That means
            // rotating the key is `export NEW_KEY=...` with no PRISM
            // restart required.
            let env_name = api_key_env
                .clone()
                .unwrap_or_else(|| ChatTarget::default_api_key_env(&provider).to_string());
            // Probe the env var so we can warn early. Don't error —
            // user might want to set the chat target now and the env
            // var later.
            let key_present = std::env::var(&env_name)
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            let target = ChatTarget::Provider {
                provider,
                model,
                api_key_env: Some(env_name.clone()),
            };
            if !key_present {
                // Save the config, but the message will tell the user
                // their next chat will fail until they set the env var.
                cfg.chat = target.clone();
                chat_config::save(&cfg)?;
                if let Some(live) = live_target {
                    *live.write().await = target.clone();
                }
                return Ok(UseOutcome {
                    new_target: target,
                    message: format!(
                        "Saved chat target. \x1b[33mWarning:\x1b[0m env var \
                         \x1b[1m{env_name}\x1b[0m is not set — chat will fail \
                         with an auth error until you `export {env_name}=...` \
                         in your shell.\nTools: {tools_state}.",
                        tools_state = tools_state_line(marc27_logged_in)
                    ),
                });
            }
            target
        }
        UseAction::Show => {
            return Ok(UseOutcome {
                new_target: cfg.chat.clone(),
                message: format!(
                    "Chat:  \x1b[1m{}\x1b[0m\nTools: {tools_state}",
                    cfg.chat.human_full(),
                    tools_state = tools_state_line(marc27_logged_in)
                ),
            });
        }
        UseAction::Reset => ChatTarget::Marc27 { model: None },
    };

    cfg.chat = next.clone();
    chat_config::save(&cfg)?;
    if let Some(live) = live_target {
        *live.write().await = next.clone();
    }

    let message = format!(
        "\u{2713} Chat:  \x1b[1m{}\x1b[0m\n  Tools: {tools_state}",
        next.human_full(),
        tools_state = tools_state_line(marc27_logged_in)
    );

    Ok(UseOutcome {
        new_target: next,
        message,
    })
}

fn tools_state_line(marc27_logged_in: bool) -> String {
    if marc27_logged_in {
        "MARC27 cloud (logged in)".to_string()
    } else {
        "\x1b[33mdisabled\x1b[0m — run `prism login` to enable knowledge \
         graph, discourse, marketplace"
            .to_string()
    }
}

fn validate_url(url: &str) -> Result<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        bail!("URL must start with http:// or https:// (got {url:?})");
    }
    // Heuristic: a typical OpenAI-compat endpoint ends in `/v1`. Don't
    // hard-fail without it (some local servers expose at /, others
    // mount at /api/v1) but warn-style messaging is the caller's job.
    let _ = url
        .parse::<url::Url>()
        .with_context(|| format!("invalid URL {url:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Serialise tests in this module — all of them mutate `$HOME` to
    /// isolate the on-disk config, and that's a process-global. Without
    /// this lock, parallel tests stomp each other's HOME and the wrong
    /// tempdir gets read at load time. The lock is tests-only; it adds
    /// no runtime cost in the actual binary.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct IsolatedHome {
        _tmp: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    fn isolated_home() -> IsolatedHome {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: ENV_LOCK serialises this test module's HOME mutation.
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }
        IsolatedHome {
            _tmp: tmp,
            _guard: guard,
        }
    }

    #[tokio::test]
    async fn show_default_is_marc27() {
        let _h = isolated_home();
        let out = apply(UseAction::Show, None, true).await.unwrap();
        assert_eq!(out.new_target, ChatTarget::Marc27 { model: None });
        assert!(out.message.contains("MARC27 cloud"));
    }

    #[tokio::test]
    async fn marc27_with_model_persists() {
        let _h = isolated_home();
        let out = apply(
            UseAction::Marc27 {
                model: Some("gpt-5.5".to_string()),
            },
            None,
            true,
        )
        .await
        .unwrap();
        assert_eq!(
            out.new_target,
            ChatTarget::Marc27 {
                model: Some("gpt-5.5".to_string())
            }
        );
        assert!(out.message.contains("gpt-5.5"));
        let reloaded = chat_config::load().unwrap();
        assert_eq!(
            reloaded.chat,
            ChatTarget::Marc27 {
                model: Some("gpt-5.5".to_string())
            }
        );
    }

    #[tokio::test]
    async fn local_persists_and_renders() {
        let _h = isolated_home();
        let out = apply(
            UseAction::Local {
                url: "http://localhost:11434/v1".into(),
                model: "llama-3.1-70b".into(),
                api_key: None,
            },
            None,
            true,
        )
        .await
        .unwrap();
        match out.new_target {
            ChatTarget::Local { url, model, .. } => {
                assert_eq!(url, "http://localhost:11434/v1");
                assert_eq!(model, "llama-3.1-70b");
            }
            other => panic!("expected Local, got {other:?}"),
        }
        // Reload from disk to verify save round-tripped.
        let reloaded = chat_config::load().unwrap();
        match reloaded.chat {
            ChatTarget::Local { ref url, .. } => {
                assert_eq!(url, "http://localhost:11434/v1");
            }
            other => panic!("expected Local on reload, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn provider_warns_when_env_missing() {
        let _h = isolated_home();
        // SAFETY: tests are single-threaded for env var mutation.
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        let out = apply(
            UseAction::Provider {
                provider: "anthropic".into(),
                model: "claude-sonnet-4".into(),
                api_key_env: None,
            },
            None,
            true,
        )
        .await
        .unwrap();
        assert!(
            out.message.contains("ANTHROPIC_API_KEY"),
            "expected warning about missing env var, got: {}",
            out.message
        );
        assert!(out.message.contains("not set"));
    }

    #[tokio::test]
    async fn reset_returns_to_marc27() {
        let _h = isolated_home();
        // First switch to local.
        apply(
            UseAction::Local {
                url: "http://localhost:11434/v1".into(),
                model: "x".into(),
                api_key: None,
            },
            None,
            true,
        )
        .await
        .unwrap();
        // Now reset.
        let out = apply(UseAction::Reset, None, true).await.unwrap();
        assert_eq!(out.new_target, ChatTarget::Marc27 { model: None });
        let reloaded = chat_config::load().unwrap();
        assert_eq!(reloaded.chat, ChatTarget::Marc27 { model: None });
    }

    #[tokio::test]
    async fn live_target_swaps() {
        let _h = isolated_home();
        let live = Arc::new(RwLock::new(ChatTarget::Marc27 { model: None }));
        apply(
            UseAction::Local {
                url: "http://localhost:11434/v1".into(),
                model: "qwen2.5".into(),
                api_key: None,
            },
            Some(&live),
            false,
        )
        .await
        .unwrap();
        let observed = live.read().await.clone();
        match observed {
            ChatTarget::Local { ref model, .. } => assert_eq!(model, "qwen2.5"),
            other => panic!("expected Local in live target, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn local_rejects_bare_host() {
        let _h = isolated_home();
        let err = apply(
            UseAction::Local {
                url: "localhost:11434/v1".into(),
                model: "x".into(),
                api_key: None,
            },
            None,
            true,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("http://"));
    }

    #[tokio::test]
    async fn tools_state_reflects_login() {
        let _h = isolated_home();
        let logged_in = apply(UseAction::Show, None, true).await.unwrap();
        assert!(logged_in.message.contains("logged in"));
        let logged_out = apply(UseAction::Show, None, false).await.unwrap();
        assert!(logged_out.message.contains("disabled"));
        assert!(logged_out.message.contains("prism login"));
    }
}
