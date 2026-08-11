//! First-run onboarding wizard.
//!
//! The very first time someone launches `prism` (no credentials on disk)
//! we run a short guided setup instead of dropping them into the TUI on
//! silent defaults — the reason a fresh install used to show `gpt-5.5`
//! with no login: nothing ever asked. Three steps, ~30 seconds: sign in,
//! pick a model, done. Mirrors how `gh`/`stripe`/`vercel` onboard.
//!
//! Everything here reuses existing primitives — the device-flow login
//! (`perform_full_login`), the numbered prompt (`prompt_select`), and the
//! chat-target store (`chat_config`). No new auth or config machinery.

use anyhow::Result;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

use prism_runtime::{PlatformEndpoints, PrismPaths};

use crate::chat_config::{self, ChatTarget};
use crate::local_llm::{LocalServer, ServerState};
use crate::{perform_full_login, prompt_select, provider_login_mode};

/// One model offered on the onboarding shortlist. `id` is the exact
/// MARC27 catalog `model_id`; the rest is display only.
struct CuratedModel {
    id: &'static str,
    name: &'static str,
    blurb: &'static str,
    price: &'static str,
}

/// The curated shortlist shown at onboarding. Recommended first, so a
/// bare Enter accepts it. IDs verified against the live `prism models
/// list` catalog — keep them in sync if the platform renames a model.
/// (The TUI `/model` picker keeps its own preferred-ID list; this one is
/// deliberately tiny because a first-run user does not want 550 rows.)
const CURATED_MODELS: &[CuratedModel] = &[
    CuratedModel {
        id: "anthropic/claude-sonnet-5",
        name: "Claude Sonnet 5",
        blurb: "Recommended — balanced depth & speed",
        price: "$2 / $10 per M",
    },
    CuratedModel {
        id: "anthropic/claude-haiku-4.5",
        name: "Claude Haiku 4.5",
        blurb: "Fastest, cheapest Claude",
        price: "$1 / $5",
    },
    CuratedModel {
        id: "anthropic/claude-opus-4.7",
        name: "Claude Opus 4.7",
        blurb: "Deepest reasoning",
        price: "$5 / $25",
    },
    CuratedModel {
        id: "anthropic/claude-fable-5",
        name: "Claude Fable 5",
        blurb: "Frontier tier",
        price: "$10 / $50",
    },
    CuratedModel {
        id: "gpt-5.5",
        name: "GPT-5.5",
        blurb: "OpenAI",
        price: "$2 / $8",
    },
    CuratedModel {
        id: "google/gemma-4-31b-it:free",
        name: "Gemma 4 31B",
        blurb: "Free — zero cost",
        price: "free",
    },
];

/// Run the wizard iff this looks like a genuine first launch: the user has
/// not yet chosen how PRISM reaches a model, AND we have a real terminal to
/// prompt on. Piped or automated invocations (tui-driver, CI, `prism | cat`)
/// must never block on stdin, so we bail early there and let the normal boot
/// flow handle the not-set-up state.
pub async fn run_if_first_launch(
    paths: &PrismPaths,
    endpoints: &PlatformEndpoints,
    python: &Path,
) -> Result<()> {
    let state = paths.load_cli_state().unwrap_or_default();
    if !should_run_wizard(
        state.credentials.is_some(),
        chat_config::chat_target_is_configured(),
        io::stdin().is_terminal(),
    ) {
        return Ok(());
    }

    welcome();

    // Step 1 — pick how PRISM reaches a model.
    //
    // This used to be "sign in", full stop, with the login error
    // propagating out of `run_if_first_launch` — so a user without a
    // platform account could not get past the wizard into PRISM at all,
    // and `prism --offline` was the only way in. PRISM works with any
    // OpenAI-compatible endpoint and the user's own key, so the wizard
    // now asks rather than assumes.
    //
    // The hosted platform stays option 1 and the bare-Enter default: it
    // is the least-friction route (no keys, nothing to install). It is
    // just no longer the ONLY route.
    step_header(1, "Choose where your models run");
    // Look before offering. A first-run user who already has Ollama serving
    // is the single best answer to "does PRISM work without an account" —
    // and the old wizard, which only knew about hosted-vs-bring-a-key, made
    // them type a URL and a model id PRISM could simply have read.
    let local_servers = crate::local_llm::discover().await;
    match choose_route(&local_servers)? {
        Route::Platform => {
            println!("\n  Hosted login is disabled by default. Set PRISM_ALLOW_INTERACTIVE_AUTH=1");
            println!("  interactive authentication needs a TTY; a PAT also works.\n");
            perform_full_login(
                paths,
                endpoints,
                python,
                provider_login_mode(endpoints, false, true)?,
            )
            .await?;

            step_header(2, "Choose your model");
            choose_model()?;

            step_header(3, "API keys");
            api_keys_note();
        }
        Route::OwnProvider => {
            step_header(2, "Pick a provider");
            choose_provider()?;

            step_header(3, "You can sign in later");
            platform_later_note();
        }
        Route::LocalServer(server) => {
            step_header(2, "Pick a model");
            choose_local_model(&server)?;

            step_header(3, "You can sign in later");
            platform_later_note();
        }
    }

    done();
    Ok(())
}

/// Is this a genuine first launch?
///
/// The bug this replaces: the condition was `has_credentials`, full stop.
/// Credentials only exist for someone who signed in to the hosted platform,
/// so a user who picked "own key" — or a model server already running on
/// their machine — was shown the whole three-step wizard again on every
/// launch, forever. Onboarding is finished when the user has *decided*, and
/// the hosted account is one of several ways to decide.
fn should_run_wizard(
    has_credentials: bool,
    chat_target_configured: bool,
    interactive: bool,
) -> bool {
    interactive && !has_credentials && !chat_target_configured
}

/// How a first-run user wants chat routed.
#[derive(Clone)]
enum Route {
    /// The hosted platform — no keys, no local install.
    Platform,
    /// Any provider in the registry, on the user's own key (or a local
    /// server needing no key at all).
    OwnProvider,
    /// A model server already running on this machine. Offered only when
    /// one was actually found, and still chosen by the user.
    LocalServer(LocalServer),
}

struct RouteChoice {
    route: Route,
    label: String,
    blurb: String,
}

/// Ask how to route chat.
///
/// Anything already serving on this machine leads the list — it is the
/// option with nothing left to set up, no key and no account, and it is
/// offered as a choice rather than applied silently. When nothing is
/// running the list is exactly what it was: hosted, or your own key.
fn choose_route(local_servers: &[LocalServer]) -> Result<Route> {
    let brand = crate::brand::brand();
    let mut options: Vec<RouteChoice> = local_servers
        .iter()
        .filter(|s| s.state == ServerState::Ready && !s.models.is_empty())
        .map(|server| RouteChoice {
            route: Route::LocalServer(server.clone()),
            label: "On this Mac".to_string(),
            blurb: format!("{} — {}, no key, no account", server.name, server.summary()),
        })
        .collect();

    if options.is_empty() {
        println!("  PRISM talks to any OpenAI-compatible model. Two ways to start:");
    } else {
        println!("  PRISM talks to any OpenAI-compatible model — including the one");
        println!("  already running here:");
    }

    options.push(RouteChoice {
        route: Route::Platform,
        label: "Hosted".to_string(),
        blurb: format!("{} — {}", brand.platform_name, brand.tagline),
    });
    options.push(RouteChoice {
        route: Route::OwnProvider,
        label: "Own key".to_string(),
        blurb: "OpenAI, Anthropic, Groq, Ollama, … — your key, your bill".to_string(),
    });

    let chosen = prompt_select("Route", &options, |o| {
        format!("{:<12} \x1b[2m{}\x1b[0m", o.label, o.blurb)
    })?;
    Ok(chosen.route.clone())
}

/// Route chat at a server we found, once the user has picked it. The model
/// list comes from the server, so there is no id to type and no id to get
/// wrong — which is the difference between this and the own-key path.
fn choose_local_model(server: &LocalServer) -> Result<()> {
    let model = match server.sole_model() {
        Some(only) => only.to_string(),
        None => {
            println!("  {} is serving these:", server.name);
            prompt_select("Model", &server.models, |m| m.clone())?.clone()
        }
    };

    let mut cfg = chat_config::load().unwrap_or_default();
    cfg.chat = ChatTarget::Local {
        url: server.base_url.clone(),
        model: model.clone(),
        api_key: None,
    };
    chat_config::save(&cfg)?;

    println!(
        "\n  \x1b[32m✓\x1b[0m Chat routed to \x1b[1m{}\x1b[0m ({model}) at {}.",
        server.name, server.base_url
    );
    println!("  \x1b[2mNo key, no account — inference stays on this machine.\x1b[0m");
    Ok(())
}

/// Present the provider registry and persist the pick as the chat target.
///
/// Providers whose key is already exported are marked, so the common case
/// (someone who already has `OPENAI_API_KEY` in their shell) is a single
/// keypress away from a working PRISM with no account anywhere.
fn choose_provider() -> Result<()> {
    let registry = crate::providers::Registry::load();
    let choices: Vec<&crate::providers::Provider> =
        registry.all().iter().filter(|p| !p.platform).collect();
    if choices.is_empty() {
        // Cannot happen with the shipped registry, but refusing to prompt
        // over an empty list beats panicking on an index.
        return Ok(());
    }

    // Scoped to this screen on purpose. Every option below is a registry
    // provider, whose key really is env-only — but "PRISM never writes it to
    // disk" (what this used to say) is not true of PRISM as a whole:
    // `prism use local --api-key` stores the key it is handed.
    println!("  Your key is read from an env var at request time — nothing on this");
    println!("  screen writes it to disk. Local servers need no key at all.");

    let chosen = prompt_select("Provider", &choices, |p| {
        let ready = key_status(p);
        format!(
            "{:<24} {ready}  \x1b[2m{}\x1b[0m",
            p.display_name(),
            p.blurb()
        )
    })?;

    let model = prompt_line(&format!(
        "Model id for {} (e.g. the one you use today)",
        chosen.display_name()
    ))?;

    let mut cfg = chat_config::load().unwrap_or_default();
    cfg.chat = ChatTarget::Provider {
        provider: chosen.id.clone(),
        model: model.clone(),
        api_key_env: chosen.api_key_env.clone(),
    };
    chat_config::save(&cfg)?;

    println!(
        "\n  \x1b[32m✓\x1b[0m Chat routed to \x1b[1m{}\x1b[0m ({model}).",
        chosen.display_name()
    );
    match &chosen.api_key_env {
        Some(var) if !chosen.key_present() => {
            println!(
                "  \x1b[33m!\x1b[0m Set your key before chatting: \x1b[1mexport {var}=…\x1b[0m"
            );
            if let Some(docs) = &chosen.docs {
                println!("    Get one at \x1b[4m{docs}\x1b[0m");
            }
        }
        Some(var) => println!("  \x1b[2mUsing {var} from your environment.\x1b[0m"),
        None => println!("  \x1b[2mNo key needed — make sure the server is running.\x1b[0m"),
    }
    Ok(())
}

/// The readiness column on the provider picker.
///
/// Three states, not two. `Provider::key_present()` is `true` for a keyless
/// local server — correctly, since nothing can be missing — but rendering
/// that as "✓ key found" told users Ollama had found a key they had never
/// set, and implied the others were somehow less ready. A local server's
/// readiness is a different fact, so it gets its own words.
fn key_status(p: &crate::providers::Provider) -> &'static str {
    match (&p.api_key_env, p.key_present()) {
        (None, _) => "\x1b[32m✓ no key needed\x1b[0m",
        (Some(_), true) => "\x1b[32m✓ key found\x1b[0m   ",
        (Some(_), false) => "\x1b[2m  no key      \x1b[0m",
    }
}

/// Read one non-empty line. Used for the model id, which we cannot offer a
/// shortlist for without querying a provider we have no key for yet.
fn prompt_line(label: &str) -> Result<String> {
    loop {
        print!("\n{label}: ");
        io::stdout().flush()?;
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        let trimmed = buf.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
        println!("  \x1b[2mA model id is required.\x1b[0m");
    }
}

/// Close the own-key path by naming what signing in would add, without
/// implying anything is broken without it.
fn platform_later_note() {
    let brand = crate::brand::brand();
    println!("  You're set — chat, local tools, notebooks and workflows all work now.");
    println!(
        "  \x1b[2mSigning in to {} later adds the hosted knowledge graph, discourse,\x1b[0m",
        brand.display_name
    );
    println!("  \x1b[2mmarketplace and managed compute:\x1b[0m \x1b[1mprism login\x1b[0m");
    println!(
        "  \x1b[2mSwitch or compare providers any time:\x1b[0m \x1b[1mprism use list\x1b[0m\n"
    );
    print!("  Press \x1b[1mEnter\x1b[0m to continue. ");
    let _ = io::stdout().flush();
    let mut scratch = String::new();
    let _ = io::stdin().read_line(&mut scratch);
}

/// Present the shortlist and persist the pick as the default chat target.
fn choose_model() -> Result<()> {
    let brand = crate::brand::brand();
    println!(
        "  Models are served on {}'s keys — billed per call to your org's",
        brand.display_name
    );
    println!("  credits, nothing to manage. Pick a default (change any time");
    println!("  with \x1b[1m/model\x1b[0m inside PRISM):");

    let chosen = prompt_select("Model", CURATED_MODELS, |m| {
        format!("{:<18} \x1b[2m{}  ·  {}\x1b[0m", m.name, m.blurb, m.price)
    })?;

    let mut cfg = chat_config::load().unwrap_or_default();
    cfg.chat = ChatTarget::Marc27 {
        model: Some(chosen.id.to_string()),
    };
    chat_config::save(&cfg)?;

    println!(
        "\n  \x1b[32m✓\x1b[0m Default model set to \x1b[1m{}\x1b[0m \x1b[2m({})\x1b[0m.",
        chosen.name, chosen.id
    );
    Ok(())
}

/// Explain hosted-vs-BYO keys and wait for Enter. We never take a secret
/// through this prompt — we only point at the env var the user sets
/// themselves.
fn api_keys_note() {
    let brand = crate::brand::brand();
    println!(
        "  By default every model is served on {}'s keys — nothing to set up.",
        brand.display_name
    );
    println!("  To use your \x1b[1mown\x1b[0m provider key instead (billed to you directly):\n");
    println!(
        "    \x1b[2mexport OPENAI_API_KEY=…\x1b[0m   \x1b[2m# or ANTHROPIC_API_KEY, etc.\x1b[0m"
    );
    println!(
        "    \x1b[2mthen \x1b[0m\x1b[1m/use list\x1b[0m\x1b[2m inside PRISM to see every provider and switch\x1b[0m\n"
    );
    print!("  Press \x1b[1mEnter\x1b[0m to continue. ");
    let _ = io::stdout().flush();
    let mut scratch = String::new();
    let _ = io::stdin().read_line(&mut scratch);
}

fn welcome() {
    println!();
    println!("  \x1b[38;2;0;255;255m◆ PRISM\x1b[0m  \x1b[2m· AI-native materials discovery\x1b[0m");
    println!();
    println!("  \x1b[1mWelcome — let's get you set up.\x1b[0m");
    println!("  \x1b[2mThree quick steps: sign in, pick a model, and you're in.\x1b[0m");
}

fn step_header(n: u8, title: &str) {
    println!();
    println!("  \x1b[38;2;251;191;36m● Step {n} of 3\x1b[0m  \x1b[1m{title}\x1b[0m");
    println!("  \x1b[2m──────────────────────────────────────────────\x1b[0m");
}

fn done() {
    println!();
    println!("  \x1b[32m✓ All set.\x1b[0m Launching PRISM…");
    println!("  \x1b[2mTip: /model changes models, /help lists commands.\x1b[0m");
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Provider;

    fn provider(api_key_env: Option<&str>) -> Provider {
        Provider {
            id: "test".into(),
            name: None,
            base_url: Some("http://localhost:1/v1".into()),
            api_key_env: api_key_env.map(str::to_string),
            docs: None,
            platform: false,
        }
    }

    /// THE defect. A user who chose "own key" never gets platform
    /// credentials, so keying wizard-completion on credentials showed them
    /// the whole three-step first run on every single launch.
    #[test]
    fn a_configured_chat_target_finishes_onboarding_without_an_account() {
        assert!(
            !should_run_wizard(false, true, true),
            "the user has chosen — do not ask again"
        );
    }

    #[test]
    fn a_genuine_first_launch_still_runs_the_wizard() {
        assert!(should_run_wizard(false, false, true));
    }

    #[test]
    fn signing_in_also_finishes_onboarding() {
        assert!(!should_run_wizard(true, false, true));
    }

    /// Piped or automated invocations must never block on stdin.
    #[test]
    fn a_non_interactive_run_never_prompts() {
        for credentials in [false, true] {
            for configured in [false, true] {
                assert!(!should_run_wizard(credentials, configured, false));
            }
        }
    }

    /// A local server needs no key, so "✓ key found" was a claim about a
    /// key the user never set.
    #[test]
    fn a_keyless_provider_is_not_described_as_having_found_a_key() {
        let status = key_status(&provider(None));
        assert!(!status.contains("key found"), "got {status:?}");
        assert!(status.contains("no key needed"), "got {status:?}");
    }

    #[test]
    fn a_keyed_provider_reports_the_environment_honestly() {
        // SAFETY: a uniquely-named var no other test reads or writes.
        unsafe { std::env::set_var("PRISM_ONBOARDING_KEY_PROBE", "sk-x") };
        assert!(key_status(&provider(Some("PRISM_ONBOARDING_KEY_PROBE"))).contains("key found"));
        unsafe { std::env::remove_var("PRISM_ONBOARDING_KEY_PROBE") };
        assert!(key_status(&provider(Some("PRISM_ONBOARDING_KEY_PROBE"))).contains("no key"));
    }
}
