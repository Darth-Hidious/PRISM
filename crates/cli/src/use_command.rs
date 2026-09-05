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
use crate::local_llm::{self, LocalServer, ServerState};
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
    /// Point chat at any OpenAI-compatible server by URL — loopback or a
    /// GPU box across the network.
    ///
    /// `model` is optional because the server already knows what it is
    /// serving: omitted, PRISM asks the endpoint and uses the answer when
    /// there is exactly one. Completing the command from the endpoint is
    /// also the only way to get a vLLM id right — it serves under the full
    /// Hugging Face repo path, which nobody types from memory.
    Local {
        url: String,
        model: Option<String>,
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
    /// Append a fallback target: tried, in order, when the chat target
    /// cannot answer. A marc27 target is refused (its credentials are session
    /// state the fallback path does not hold).
    FallbackAdd(ChatTarget),
    /// Print the fallback list.
    FallbackList,
    /// Drop every fallback.
    FallbackClear,
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
/// The fallback list as the reader sees it: numbered, in the order tried, or
/// how to add one.
fn fallbacks_message(cfg: &chat_config::PrismConfig) -> String {
    if cfg.fallbacks.is_empty() {
        return "Fallbacks: none — `prism use fallback add --url <server> --model <id>` or \
                `--provider <id> --model <id>`"
            .to_string();
    }
    let mut out = String::from("Fallbacks (tried in order when the chat target cannot answer):");
    for (i, target) in cfg.fallbacks.iter().enumerate() {
        out.push_str(&format!("\n  {}. {}", i + 1, target.human_full()));
    }
    out
}

/// config is saved and the next launch picks it up.
pub async fn apply(
    action: UseAction,
    live_target: Option<&Arc<RwLock<ChatTarget>>>,
    marc27_logged_in: bool,
) -> Result<UseOutcome> {
    let mut cfg = chat_config::load().unwrap_or_default();

    // The fallback actions edit the list and leave the chat target alone.
    match &action {
        UseAction::FallbackAdd(target) => {
            if matches!(target, ChatTarget::Marc27 { .. }) {
                anyhow::bail!(
                    "a marc27 target cannot be a fallback: its credentials are session state \
                     the fallback path does not hold. Name a local server (--url) or a \
                     registry provider (--provider)."
                );
            }
            cfg.fallbacks.push(target.clone());
            chat_config::save(&cfg)?;
            return Ok(UseOutcome {
                new_target: cfg.chat.clone(),
                message: fallbacks_message(&cfg),
            });
        }
        UseAction::FallbackList => {
            return Ok(UseOutcome {
                new_target: cfg.chat.clone(),
                message: fallbacks_message(&cfg),
            });
        }
        UseAction::FallbackClear => {
            cfg.fallbacks.clear();
            chat_config::save(&cfg)?;
            return Ok(UseOutcome {
                new_target: cfg.chat.clone(),
                message: fallbacks_message(&cfg),
            });
        }
        _ => {}
    }

    let next = match action {
        UseAction::Marc27 { model } => ChatTarget::Marc27 { model },
        UseAction::Local {
            url,
            model,
            api_key,
        } => {
            validate_url(&url)?;
            let model = match model {
                Some(model) => model,
                None if prism_ingest::llm::is_local_gguf_url(&url) => bail!(
                    "embedded GGUF inference requires --model <path-or-name>; names are resolved below ~/.prism/models and PRISM never downloads weights"
                ),
                None => resolve_model_from_server(&url).await?,
            };
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
                // "or run `prism use list`" straddled a string continuation
                // exactly at "run \ `prism", so the no_exit_to_cli guard's
                // physical-line scan never saw it; the splice-aware scan does.
                // The mention stays (it names this command's own subcommand),
                // the imperative goes.
                notes.push(format!(
                    "\x1b[33mUnknown provider\x1b[0m \x1b[1m{provider}\x1b[0m — PRISM will \
                     guess \x1b[2mhttps://api.{provider}.com/v1\x1b[0m, which is probably \
                     wrong. Declare it in {override_path} to fix the endpoint; \
                     `prism use list` shows the providers PRISM ships with.",
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
            let registry = Registry::load();
            let discovered = local_llm::discover_in(&registry).await;
            return Ok(UseOutcome {
                new_target: cfg.chat.clone(),
                message: render_provider_list(&registry, &cfg.chat, marc27_logged_in, &discovered),
            });
        }
        UseAction::Show => {
            return Ok(UseOutcome {
                new_target: cfg.chat.clone(),
                message: format!(
                    "Chat:  \x1b[1m{}\x1b[0m\nTools: {tools_state}\n{fallbacks}",
                    cfg.chat.human_full(),
                    tools_state = tools_state_line(marc27_logged_in),
                    fallbacks = fallbacks_message(&cfg)
                ),
            });
        }
        UseAction::Reset => ChatTarget::Marc27 { model: None },
        UseAction::FallbackAdd(_) | UseAction::FallbackList | UseAction::FallbackClear => {
            unreachable!("handled above")
        }
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

/// Ask the server at `url` which model to use, when the user did not say.
///
/// Errors are the point here: every one of them names the endpoint, says
/// what it actually answered, and gives the user the next move. The one
/// thing this never does is pick when the answer is ambiguous — routing
/// inference somewhere the user did not choose is the surprise this whole
/// feature exists to avoid.
async fn resolve_model_from_server(url: &str) -> Result<String> {
    let Some(server) = local_llm::probe_url(url).await else {
        bail!(
            "nothing is answering at {url}.\n  \
             Start the server, or pass --model <model> to save this target anyway."
        );
    };
    if server.state == ServerState::Loading {
        bail!(
            "{url} is up but still loading a model — retry in a moment, \
             or pass --model <model>."
        );
    }
    match server.sole_model() {
        Some(only) => Ok(only.to_string()),
        None if server.models.is_empty() => bail!(
            "{url} is running but serving no models.\n  \
             Load one on the server, then re-run this command."
        ),
        None => {
            let options = server
                .models
                .iter()
                .map(|m| format!("  {}", server.use_command(m)))
                .collect::<Vec<_>>()
                .join("\n");
            bail!(
                "{url} is serving {} models — pick one:\n{options}",
                server.models.len()
            )
        }
    }
}

fn tools_state_line(marc27_logged_in: bool) -> String {
    let platform = &crate::brand::brand().platform_name;
    if marc27_logged_in {
        format!("{platform} (logged in)")
    } else {
        "\x1b[33mplatform tools unavailable\x1b[0m — not authenticated. Knowledge \
         graph, discourse and marketplace need it; local tools and chat do not."
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
///
/// `discovered` upgrades that from a claim to an observation. A local
/// provider that is actually serving stops rendering as a bare `no key`
/// row — the row names the models it holds, so a user who already has
/// Ollama running can see it from here instead of being told PRISM has no
/// local model while it does.
fn render_provider_list(
    registry: &Registry,
    current: &ChatTarget,
    marc27_logged_in: bool,
    discovered: &[LocalServer],
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
        let mark = if current_id.as_deref() == Some(p.id.as_str()) {
            "\x1b[36m❯\x1b[0m"
        } else {
            " "
        };
        let row = |status: &str, blurb: &str| {
            format!(
                "{mark} {status} \x1b[1m{name:<width$}\x1b[0m  \x1b[2m{id}\x1b[0m  \x1b[2m{blurb}\x1b[0m\n",
                name = p.display_name(),
                id = p.id,
            )
        };

        // A provider we can see serving gets one row per running server —
        // llama-server on both 8080 and 8081 is two different servers with
        // two different models, and collapsing them would hide one.
        let running: Vec<&LocalServer> = discovered
            .iter()
            .filter(|s| s.provider_id == p.id)
            .collect();
        if running.is_empty() {
            // The platform's credential is a login session, not an env var,
            // so it is judged on login state; everyone else on their key.
            let ready = if p.platform {
                marc27_logged_in
            } else {
                p.key_present()
            };
            let status = if ready {
                "\x1b[32mready\x1b[0m    "
            } else if p.platform {
                "\x1b[2mlogin   \x1b[0m "
            } else {
                "\x1b[2mno key  \x1b[0m "
            };
            out.push_str(&row(status, &p.blurb()));
            continue;
        }
        for server in running {
            let status = match server.state {
                ServerState::Ready => "\x1b[32mrunning\x1b[0m  ",
                ServerState::Loading => "\x1b[33mloading\x1b[0m  ",
            };
            // Name the URL whenever it is not the one the registry declares,
            // so an alternate port is visibly a different server.
            let blurb = if p.base_url.as_deref() == Some(server.base_url.as_str()) {
                server.summary()
            } else {
                format!("{} · {}", server.summary(), server.base_url)
            };
            out.push_str(&row(status, &blurb));
        }
    }

    // The bundled ONNX embedder is the part of PRISM that already needs no
    // account at all, so a user weighing that question should see it here.
    out.push_str(&format!(
        "\n  \x1b[2mEmbeddings:\x1b[0m {}\n",
        local_llm::onnx_embedder().summary()
    ));

    // Prefer a command the user can run as-is over one they must complete.
    let ready_to_run = discovered
        .iter()
        .find(|s| s.state == ServerState::Ready && s.sole_model().is_some())
        .or_else(|| {
            discovered
                .iter()
                .find(|s| s.state == ServerState::Ready && !s.models.is_empty())
        });
    let local_line = match ready_to_run {
        Some(server) => server.use_command(&server.models[0]),
        None => "prism use local --url http://localhost:11434/v1 --model <model>".to_string(),
    };
    out.push_str(&format!(
        "\n  \x1b[2mSwitch:\x1b[0m prism use provider <id> --model <model>\n  \
         \x1b[2mLocal: \x1b[0m {local_line}\n  \
         \x1b[2mAdd:   \x1b[0m declare any OpenAI-compatible endpoint in {path}\n",
        path = crate::providers::user_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "~/.prism/providers.toml".to_string()),
    ));

    if discovered.iter().any(|s| s.state == ServerState::Ready) {
        // Measured, not guessed: a 12B thinking model timed out on ingest at
        // the 300s default while a 3B non-thinking model finished, because
        // reasoning tokens come out of the same generation budget.
        out.push_str(
            "\n  \x1b[2mNote: a reasoning model spends its generation budget on thinking — \
             for\n        `prism ingest`, prefer a non-thinking model or raise the timeout.\x1b[0m\n",
        );
    }
    out
}

fn validate_url(url: &str) -> Result<()> {
    if prism_ingest::llm::is_local_gguf_url(url) {
        return Ok(());
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        bail!(
            "URL must start with http:// or https://, or equal gguf://local for embedded weights (got {url:?})"
        );
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
    use super::*;

    /// All tests here mutate `$HOME` (a process-global) to isolate the
    /// on-disk config. They serialise on the ONE shared env lock
    /// (`prism_runtime::offline::test_support::ENV_LOCK`) — this module
    /// used to declare its own `static ENV_LOCK`, which serialised it only
    /// against itself: a test elsewhere in this binary that overrides HOME
    /// under the shared lock raced it and opened its store inside a HOME
    /// this module had already swapped and deleted. A genuine failure, in
    /// the full parallel run only. Aliasing, not declaring, is the fix
    /// (see `boot_checks::ENV_LOCK` — same defect, tenth and eleventh
    /// occurrence).
    ///
    /// `prior_home` restores the REAL `$HOME` on drop: leaving it pointed
    /// at a deleted tempdir poisoned every later HOME-reading test in the
    /// binary.
    struct IsolatedHome {
        prior_home: Option<std::ffi::OsString>,
        _tmp: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for IsolatedHome {
        fn drop(&mut self) {
            // SAFETY: `_guard` (the shared env lock) is still held while
            // this runs — struct fields drop after `drop()` returns.
            unsafe {
                match self.prior_home.take() {
                    Some(home) => std::env::set_var("HOME", home),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    fn isolated_home() -> IsolatedHome {
        let guard = prism_runtime::offline::test_support::env_lock();
        let prior_home = std::env::var_os("HOME");
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: the shared ENV_LOCK serialises env mutation across every
        // test in this binary that takes it.
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }
        IsolatedHome {
            prior_home,
            _tmp: tmp,
            _guard: guard,
        }
    }

    #[tokio::test]
    async fn fallbacks_add_list_clear_roundtrip() {
        let _h = isolated_home();
        let local = ChatTarget::Local {
            url: "http://10.0.0.2:8080/v1".into(),
            model: "qwen".into(),
            api_key: None,
        };
        let provider = ChatTarget::Provider {
            provider: "groq".into(),
            model: "llama".into(),
            api_key_env: None,
        };
        apply(UseAction::FallbackAdd(local.clone()), None, true)
            .await
            .unwrap();
        let out = apply(UseAction::FallbackAdd(provider.clone()), None, true)
            .await
            .unwrap();
        assert!(out.message.contains("1. "), "{}", out.message);
        assert!(out.message.contains("2. "), "{}", out.message);
        assert_eq!(
            chat_config::load().unwrap().fallbacks,
            vec![local, provider]
        );
        // The chat target itself is untouched.
        assert_eq!(out.new_target, ChatTarget::Marc27 { model: None });
        // `show` names them too.
        let shown = apply(UseAction::Show, None, true).await.unwrap();
        assert!(shown.message.contains("Fallbacks"), "{}", shown.message);

        let cleared = apply(UseAction::FallbackClear, None, true).await.unwrap();
        assert!(cleared.message.contains("none"), "{}", cleared.message);
        assert!(chat_config::load().unwrap().fallbacks.is_empty());
    }

    #[tokio::test]
    async fn a_marc27_fallback_is_refused() {
        let _h = isolated_home();
        let err = apply(
            UseAction::FallbackAdd(ChatTarget::Marc27 { model: None }),
            None,
            true,
        )
        .await
        .expect_err("marc27 cannot be a fallback");
        assert!(format!("{err:#}").contains("session state"), "{err:#}");
        assert!(chat_config::load().unwrap().fallbacks.is_empty());
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
                model: Some("llama-3.1-70b".into()),
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
            std::env::remove_var("OPENAI_API_KEY");
        }
        // A BUNDLED provider: the warning names the key that provider actually
        // reads. This used to select `anthropic`, which bbd88398 deliberately
        // stopped bundling ("added by the user or not at all") — so the test
        // was asserting the key of a provider PRISM no longer ships, and the
        // message it got was the unknown-provider warning instead.
        let out = apply(
            UseAction::Provider {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                api_key_env: None,
            },
            None,
            true,
        )
        .await
        .unwrap();
        assert!(
            out.message.contains("OPENAI_API_KEY"),
            "expected warning about missing env var, got: {}",
            out.message
        );
        assert!(out.message.contains("not set"));
    }

    /// Choosing a provider PRISM does not bundle says so, and says where to
    /// declare it — it does not silently guess an endpoint in silence.
    /// Anthropic is the deliberate example: `providers.rs` asserts PRISM must
    /// not ship it, so this is the behaviour a user selecting it must get.
    #[tokio::test]
    async fn an_unbundled_provider_says_it_is_unknown() {
        let _h = isolated_home();
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
            out.message.contains("Unknown provider"),
            "an unbundled provider must be named as unknown, got: {}",
            out.message
        );
        assert!(
            out.message.contains("providers.toml"),
            "and must say where to declare it, got: {}",
            out.message
        );
    }

    #[tokio::test]
    async fn reset_returns_to_marc27() {
        let _h = isolated_home();
        // First switch to local.
        apply(
            UseAction::Local {
                url: "http://localhost:11434/v1".into(),
                model: Some("x".into()),
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
                model: Some("qwen2.5".into()),
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
    async fn embedded_gguf_target_persists_without_a_server_probe() {
        let _h = isolated_home();
        let out = apply(
            UseAction::Local {
                url: prism_ingest::llm::LOCAL_GGUF_URL.into(),
                model: Some("functiongemma-270m.gguf".into()),
                api_key: None,
            },
            None,
            false,
        )
        .await
        .unwrap();
        assert!(out.message.contains("gguf://local"));
        assert!(out.message.contains("functiongemma-270m.gguf"));
        match chat_config::load().unwrap().chat {
            ChatTarget::Local { url, model, .. } => {
                assert_eq!(url, prism_ingest::llm::LOCAL_GGUF_URL);
                assert_eq!(model, "functiongemma-270m.gguf");
            }
            other => panic!("expected local GGUF target, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn embedded_gguf_target_requires_a_model() {
        let _h = isolated_home();
        let error = apply(
            UseAction::Local {
                url: prism_ingest::llm::LOCAL_GGUF_URL.into(),
                model: None,
                api_key: None,
            },
            None,
            false,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("--model <path-or-name>"));
    }

    #[tokio::test]
    async fn local_rejects_bare_host() {
        let _h = isolated_home();
        let err = apply(
            UseAction::Local {
                url: "localhost:11434/v1".into(),
                model: Some("x".into()),
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
        // Must state the fact, never instruct the reader to go and run a
        // command — no surface may tell a human to leave and type something.
        assert!(logged_out.message.contains("not authenticated"));
        assert!(!logged_out.message.contains("prism login"));
        // Logged out must not read as "PRISM is broken": it says what is
        // unavailable AND what still works.
        assert!(
            logged_out.message.contains("local tools and chat do not"),
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
        // And the converse, mirroring `providers.rs`: PRISM must NOT ship an
        // Anthropic provider (bbd88398 — "added by the user or not at all").
        // Without this the list could regrow it and only providers.rs would
        // notice, which is how these two tests drifted apart in the first
        // place.
        assert!(
            !out.message.contains("anthropic"),
            "PRISM must not ship an Anthropic provider, got: {}",
            out.message
        );
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
        // NB: no assertion on the local providers here. `apply` probes this
        // machine, so their rows depend on whether a server happens to be
        // running — see `render_lists_a_keyless_local_provider_as_ready`,
        // which pins that claim against an injected discovery result instead.
        unsafe { std::env::remove_var("GROQ_API_KEY") };
    }

    /// Discovery-independent rendering. `render_provider_list` is pure, so
    /// these pin the listing against an injected result rather than against
    /// whatever is listening on the developer's machine.
    fn render(discovered: &[LocalServer]) -> String {
        render_provider_list(
            &Registry::builtin().unwrap(),
            &ChatTarget::Marc27 { model: None },
            false,
            discovered,
        )
    }

    fn found(provider_id: &str, url: &str, models: &[&str]) -> LocalServer {
        LocalServer {
            provider_id: provider_id.to_string(),
            name: format!("{provider_id} (local)"),
            base_url: url.to_string(),
            models: models.iter().map(|m| m.to_string()).collect(),
            state: ServerState::Ready,
        }
    }

    /// With nothing running, a keyless local provider is still listed as
    /// usable — there is no credential for it to be missing.
    #[test]
    fn render_lists_a_keyless_local_provider_as_ready() {
        let out = render(&[]);
        assert!(provider_line(&out, "ollama").contains("ready"));
        // Nothing to run yet, so the command keeps its placeholder.
        assert!(out.contains("--model <model>"), "{out}");
        // And no reasoning caveat, because no model was found to caveat.
        assert!(!out.contains("reasoning model"), "{out}");
    }

    /// The headline fix: a running server stops reading as a bare `no key`
    /// row and names the models it is actually holding.
    #[test]
    fn render_names_the_models_a_running_server_holds() {
        let out = render(&[found(
            "ollama",
            "http://localhost:11434/v1",
            &["qwen2.5:3b", "llama3.2:1b"],
        )]);
        let row = provider_line(&out, "ollama");
        assert!(row.contains("running"), "{row}");
        assert!(row.contains("2 models (qwen2.5:3b, llama3.2:1b)"), "{row}");

        // The footer command is now runnable as-is.
        assert!(
            out.contains("prism use local --url http://localhost:11434/v1 --model qwen2.5:3b"),
            "{out}"
        );
        // The `Switch:` provider hint keeps its placeholder — it is generic
        // by nature — but the local line must no longer carry one.
        assert!(
            !out.contains("prism use local --url http://localhost:11434/v1 --model <model>"),
            "{out}"
        );
        // A found model earns the reasoning-budget caveat.
        assert!(out.contains("reasoning model"), "{out}");
    }

    /// Two llama-servers on different ports are two servers, not one.
    /// Collapsing them would hide a model the user can actually use.
    #[test]
    fn render_gives_each_running_server_its_own_row() {
        let out = render(&[
            found("llamacpp", "http://localhost:8080/v1", &["a.gguf"]),
            found("llamacpp", "http://localhost:8081/v1", &["b.gguf"]),
        ]);
        let rows: Vec<&str> = out
            .lines()
            .filter(|l| l.contains("\x1b[2mllamacpp\x1b[0m"))
            .collect();
        assert_eq!(rows.len(), 2, "expected one row per server, got {rows:?}");
        assert!(out.contains("a.gguf") && out.contains("b.gguf"), "{out}");
        // The non-default port must be visible, or the rows are ambiguous.
        assert!(out.contains("http://localhost:8081/v1"), "{out}");
    }

    /// A server still loading is reported as such — not as ready, and not
    /// as absent.
    #[test]
    fn render_marks_a_loading_server_as_loading() {
        let mut loading = found("vllm", "http://localhost:8000/v1", &[]);
        loading.state = ServerState::Loading;
        let out = render(&[loading]);
        let row = provider_line(&out, "vllm");
        assert!(row.contains("loading"), "{row}");
        assert!(!row.contains("running"), "{row}");
        // Not a usable model, so it must not become the suggested command.
        assert!(out.contains("--model <model>"), "{out}");
    }

    /// The bundled ONNX embedder is reported on every listing — it is the
    /// part of PRISM that already needs no account at all.
    #[test]
    fn render_reports_the_bundled_onnx_embedder() {
        let out = render(&[]);
        assert!(out.contains("Embeddings:"), "{out}");
        assert!(out.contains("local ONNX"), "{out}");
        assert!(out.contains("384-dim"), "{out}");
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
                model: Some("qwen".into()),
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
