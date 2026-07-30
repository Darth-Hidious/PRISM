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
use crate::providers::Registry;

/// Subcommand variants. Mirrors the clap enum in main.rs but kept
/// independent so this module doesn't pull in clap-specific types —
/// the slash-command parser builds these manually from a tokenised
/// chat input. Identical semantics either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseAction {
    /// Stay on the hosted platform (the zero-setup default), but pin a
    /// specific model it should serve. `marc27` is the frozen wire id for
    /// this route — it does not change when the brand does.
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
    /// Print the provider registry and which entries have credentials
    /// present right now. Read-only — changes nothing.
    List,
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
            let registry = Registry::load();
            let known = registry.get(&provider);
            // Resolve which env var to read at request time. We never
            // store the key itself, only the env var name. That means
            // rotating the key is `export NEW_KEY=...` with no PRISM
            // restart required.
            let env_name = api_key_env
                .clone()
                .unwrap_or_else(|| crate::providers::default_api_key_env(&registry, &provider));
            // Probe the env var so we can warn early. Don't error —
            // user might want to set the chat target now and the env
            // var later.
            let key_present = std::env::var(&env_name)
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            let target = ChatTarget::Provider {
                provider: provider.clone(),
                model,
                api_key_env: Some(env_name.clone()),
            };

            // Two things can be wrong, and they are different problems:
            // an id we have no endpoint for (we will fall back to a guess
            // that is probably wrong), and a key that is not exported yet.
            // Say both, at the moment the user can still act on them,
            // rather than letting the next chat turn 401 mysteriously.
            let mut notes = Vec::new();
            if known.is_none() {
                notes.push(format!(
                    "\x1b[33mUnknown provider\x1b[0m \x1b[1m{provider}\x1b[0m — PRISM will \
                     guess \x1b[2mhttps://api.{provider}.com/v1\x1b[0m, which is probably \
                     wrong. Declare it in {override_path} to fix the endpoint, or run \
                     `prism use list` to see the providers PRISM ships with.",
                    override_path = crate::providers::user_path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "~/.prism/providers.toml".to_string()),
                ));
            }
            if !key_present {
                let docs = known
                    .and_then(|p| p.docs.as_deref())
                    .map(|d| format!(" Get one at \x1b[4m{d}\x1b[0m."))
                    .unwrap_or_default();
                notes.push(format!(
                    "\x1b[33mWarning:\x1b[0m env var \x1b[1m{env_name}\x1b[0m is not set — \
                     chat will fail with an auth error until you `export {env_name}=...` \
                     in your shell.{docs}"
                ));
            }

            if !notes.is_empty() {
                // Save anyway: the user asked for this target, and the
                // config is not what is broken. The message says what is.
                cfg.chat = target.clone();
                chat_config::save(&cfg)?;
                if let Some(live) = live_target {
                    *live.write().await = target.clone();
                }
                return Ok(UseOutcome {
                    new_target: target,
                    message: format!(
                        "\u{2713} Chat:  \x1b[1m{}\x1b[0m\n  Tools: {tools_state}\n\n{notes}",
                        cfg.chat.human_full(),
                        tools_state = tools_state_line(marc27_logged_in),
                        notes = notes.join("\n\n"),
                    ),
                });
            }
            target
        }
        UseAction::List => {
            return Ok(UseOutcome {
                new_target: cfg.chat.clone(),
                message: render_provider_list(&Registry::load(), &cfg.chat, marc27_logged_in),
            });
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
    let platform = &crate::brand::brand().platform_name;
    if marc27_logged_in {
        format!("{platform} (logged in)")
    } else {
        "\x1b[33mplatform tools unavailable\x1b[0m — run `prism login` for knowledge \
         graph, discourse, marketplace. Local tools and chat work without it."
            .to_string()
    }
}

/// Render the provider registry: what PRISM can route chat to, and which
/// entries are ready to use right now.
///
/// The readiness column is the point. A user with `OPENAI_API_KEY`
/// exported should see at a glance that they can chat today, with no
/// account anywhere — that is what makes the provider list a real
/// alternative rather than a documented one.
fn render_provider_list(
    registry: &Registry,
    current: &ChatTarget,
    marc27_logged_in: bool,
) -> String {
    let current_id = match current {
        ChatTarget::Marc27 { .. } => Some("marc27".to_string()),
        ChatTarget::Provider { provider, .. } => Some(provider.to_ascii_lowercase()),
        // A raw URL target matches no registry id — nothing to mark.
        ChatTarget::Local { .. } => None,
    };

    let width = registry
        .all()
        .iter()
        .map(|p| p.display_name().chars().count())
        .max()
        .unwrap_or(0)
        .max(4);

    // Neutral ordering. The registry declares the hosted platform first,
    // and rendering it in that order put it at the top of every listing —
    // which reads as PRISM promoting one provider over the peers it is
    // otherwise interchangeable with. Sorting by display name makes
    // position carry no editorial weight. The ❯ still marks whichever
    // target is currently selected, whatever that happens to be.
    let mut listed: Vec<_> = registry.all().iter().collect();
    listed.sort_by_key(|p| p.display_name().to_lowercase());

    let mut out = String::from("Providers PRISM can route chat to:\n\n");
    for p in listed {
        // The platform's credential is a login session, not an env var,
        // so it is judged on login state; everyone else on their key.
        let ready = if p.platform {
            marc27_logged_in
        } else {
            p.key_present()
        };
        let mark = if current_id.as_deref() == Some(p.id.as_str()) {
            "\x1b[36m❯\x1b[0m"
        } else {
            " "
        };
        let status = if ready {
            "\x1b[32mready\x1b[0m    "
        } else if p.platform {
            "\x1b[2mlogin   \x1b[0m "
        } else {
            "\x1b[2mno key  \x1b[0m "
        };
        out.push_str(&format!(
            "{mark} {status} \x1b[1m{name:<width$}\x1b[0m  \x1b[2m{id}\x1b[0m  \x1b[2m{blurb}\x1b[0m\n",
            name = p.display_name(),
            id = p.id,
            blurb = p.blurb(),
        ));
    }

    out.push_str(&format!(
        "\n  \x1b[2mSwitch:\x1b[0m prism use provider <id> --model <model>\n  \
         \x1b[2mLocal: \x1b[0m prism use local --url http://localhost:11434/v1 --model <model>\n  \
         \x1b[2mAdd:   \x1b[0m declare any OpenAI-compatible endpoint in {path}\n",
        path = crate::providers::user_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "~/.prism/providers.toml".to_string()),
    ));
    out
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
        assert!(out.message.contains(&crate::brand::brand().platform_name));
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
        assert!(logged_out.message.contains("prism login"));
        // Logged out must not read as "PRISM is broken": it says what is
        // unavailable AND what still works.
        assert!(
            logged_out
                .message
                .contains("Local tools and chat work without it"),
            "logged-out message should name what still works, got: {}",
            logged_out.message
        );
    }

    #[tokio::test]
    async fn list_shows_every_provider_and_marks_the_current_one() {
        let _h = isolated_home();
        let out = apply(UseAction::List, None, false).await.unwrap();
        // A real alternative list, not just the platform.
        for id in [
            "marc27",
            "openai",
            "anthropic",
            "google",
            "openrouter",
            "groq",
            "cerebras",
            "zai",
            "mistral",
            "deepseek",
            "xai",
            "ollama",
            "llamacpp",
            "lmstudio",
        ] {
            assert!(out.message.contains(id), "{id} missing from `use list`");
        }
        // Default target is the platform, so it should be marked current.
        assert_eq!(out.new_target, ChatTarget::Marc27 { model: None });
        assert!(out.message.contains("prism use provider"));
        assert!(out.message.contains("providers.toml"));
    }

    #[tokio::test]
    async fn list_reports_credential_presence_per_provider() {
        let _h = isolated_home();
        // SAFETY: ENV_LOCK (held by isolated_home) serialises env mutation.
        unsafe {
            std::env::set_var("GROQ_API_KEY", "gsk-test");
            std::env::remove_var("DEEPSEEK_API_KEY");
        }
        let out = apply(UseAction::List, None, false).await.unwrap();
        assert!(provider_line(&out.message, "groq").contains("ready"));
        assert!(provider_line(&out.message, "deepseek").contains("no key"));
        // Local servers need no key, so they are always usable.
        assert!(provider_line(&out.message, "ollama").contains("ready"));
        unsafe { std::env::remove_var("GROQ_API_KEY") };
    }

    /// The listing row for one provider. Matches the id column by its
    /// exact dim-wrapped form so `zai` cannot match inside another id and
    /// an uppercase env var in the blurb cannot match either.
    fn provider_line(rendered: &str, id: &str) -> String {
        let needle = format!("\x1b[2m{id}\x1b[0m");
        rendered
            .lines()
            .find(|l| l.contains(&needle))
            .unwrap_or_else(|| panic!("no row for {id} in:\n{rendered}"))
            .to_string()
    }

    #[tokio::test]
    async fn list_platform_readiness_follows_login_not_an_env_var() {
        let _h = isolated_home();
        let out_in = apply(UseAction::List, None, true).await.unwrap();
        assert!(provider_line(&out_in.message, "marc27").contains("ready"));
        let out_out = apply(UseAction::List, None, false).await.unwrap();
        assert!(provider_line(&out_out.message, "marc27").contains("login"));
    }

    #[tokio::test]
    async fn list_does_not_mutate_config() {
        let _h = isolated_home();
        apply(
            UseAction::Local {
                url: "http://localhost:11434/v1".into(),
                model: "qwen".into(),
                api_key: None,
            },
            None,
            true,
        )
        .await
        .unwrap();
        apply(UseAction::List, None, true).await.unwrap();
        match chat_config::load().unwrap().chat {
            ChatTarget::Local { ref model, .. } => assert_eq!(model, "qwen"),
            other => panic!("`use list` must be read-only, config became {other:?}"),
        }
    }

    /// A registry provider gets its declared env var, not the generic
    /// catch-all the old hardcoded match fell back to for anything
    /// outside its five known vendors.
    #[tokio::test]
    async fn provider_resolves_registry_env_var() {
        let _h = isolated_home();
        unsafe { std::env::remove_var("GROQ_API_KEY") };
        let out = apply(
            UseAction::Provider {
                provider: "groq".into(),
                model: "llama-3.3-70b".into(),
                api_key_env: None,
            },
            None,
            true,
        )
        .await
        .unwrap();
        match out.new_target {
            ChatTarget::Provider { api_key_env, .. } => {
                assert_eq!(api_key_env.as_deref(), Some("GROQ_API_KEY"));
            }
            other => panic!("expected Provider, got {other:?}"),
        }
        assert!(out.message.contains("GROQ_API_KEY"));
        // Missing key ⇒ point at where to get one.
        assert!(
            out.message.contains("console.groq.com"),
            "should surface the docs URL, got: {}",
            out.message
        );
    }

    /// An unknown slug is saved (the user asked for it) but flagged
    /// honestly, at selection time, with the fix — rather than 401ing
    /// mid-turn against a host that does not exist.
    #[tokio::test]
    async fn unknown_provider_warns_and_points_at_the_override_file() {
        let _h = isolated_home();
        let out = apply(
            UseAction::Provider {
                provider: "some-new-vendor".into(),
                model: "m".into(),
                api_key_env: None,
            },
            None,
            true,
        )
        .await
        .unwrap();
        assert!(out.message.contains("Unknown provider"));
        assert!(out.message.contains("probably \u{1b}[2mwrong") || out.message.contains("wrong"));
        assert!(out.message.contains("providers.toml"));
        // Still persisted — we do not refuse the user's choice.
        assert!(matches!(
            chat_config::load().unwrap().chat,
            ChatTarget::Provider { .. }
        ));
    }

    /// A known provider with its key exported is the happy path: saved
    /// with no warnings at all.
    #[tokio::test]
    async fn known_provider_with_key_has_no_warnings() {
        let _h = isolated_home();
        unsafe { std::env::set_var("OPENAI_API_KEY", "sk-test") };
        let out = apply(
            UseAction::Provider {
                provider: "OpenAI".into(),
                model: "gpt-4o".into(),
                api_key_env: None,
            },
            None,
            true,
        )
        .await
        .unwrap();
        assert!(!out.message.contains("Warning"), "got: {}", out.message);
        assert!(!out.message.contains("Unknown provider"));
        assert!(out.message.contains("openai (gpt-4o)"));
        unsafe { std::env::remove_var("OPENAI_API_KEY") };
    }

    /// The rename test for this surface: user-visible platform text comes
    /// from `brand.toml`, so a company rename does not need a sweep here.
    #[tokio::test]
    async fn platform_text_comes_from_brand() {
        let _h = isolated_home();
        let brand = crate::brand::brand();
        let out = apply(UseAction::Show, None, true).await.unwrap();
        assert!(
            out.message.contains(&brand.platform_name),
            "expected brand platform name in: {}",
            out.message
        );
        let list = apply(UseAction::List, None, true).await.unwrap();
        assert!(list.message.contains(&brand.tagline));
    }
}
