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
use crate::{LoginMode, perform_full_login, prompt_select};

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

/// Run the wizard iff this looks like a genuine first launch: no
/// credentials stored AND we have a real terminal to prompt on. Piped or
/// automated invocations (tui-driver, CI, `prism | cat`) must never block
/// on stdin, so we bail early there and let the normal boot flow handle
/// the not-logged-in state.
pub async fn run_if_first_launch(
    paths: &PrismPaths,
    endpoints: &PlatformEndpoints,
    python: &Path,
) -> Result<()> {
    let state = paths.load_cli_state().unwrap_or_default();
    if state.credentials.is_some() || !io::stdin().is_terminal() {
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
    match choose_route()? {
        Route::Platform => {
            println!("\n  A browser window will open to sign in. If it doesn't, copy the");
            println!("  link and code shown below into any browser on any device.\n");
            perform_full_login(
                paths,
                endpoints,
                python,
                LoginMode::Device { no_browser: false },
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
    }

    done();
    Ok(())
}

/// How a first-run user wants chat routed.
enum Route {
    /// The hosted platform — no keys, no local install.
    Platform,
    /// Any provider in the registry, on the user's own key (or a local
    /// server needing no key at all).
    OwnProvider,
}

struct RouteChoice {
    route: Route,
    label: &'static str,
    blurb: String,
}

/// Ask how to route chat. Bare Enter picks the platform: it is the option
/// that needs nothing installed and no key pasted.
fn choose_route() -> Result<Route> {
    let brand = crate::brand::brand();
    println!("  PRISM talks to any OpenAI-compatible model. Two ways to start:");

    let options = [
        RouteChoice {
            route: Route::Platform,
            label: "Hosted",
            blurb: format!("{} — {}", brand.platform_name, brand.tagline),
        },
        RouteChoice {
            route: Route::OwnProvider,
            label: "Own key",
            blurb: "OpenAI, Anthropic, Groq, Ollama, … — your key, your bill".to_string(),
        },
    ];
    let chosen = prompt_select("Route", &options, |o| {
        format!("{:<10} \x1b[2m{}\x1b[0m", o.label, o.blurb)
    })?;
    Ok(match chosen.route {
        Route::Platform => Route::Platform,
        Route::OwnProvider => Route::OwnProvider,
    })
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
        let ready = if p.key_present() {
            "\x1b[32m✓ key found\x1b[0m"
        } else {
            "\x1b[2m  no key   \x1b[0m"
        };
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
