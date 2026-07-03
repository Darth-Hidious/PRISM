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

    // Step 1 — sign in. Reuses the exact device-flow `prism login` uses:
    // opens the browser (or prints a paste-this URL + code) and polls.
    step_header(1, "Sign in to MARC27");
    println!("  A browser window will open to sign in. If it doesn't, copy the");
    println!("  link and code shown below into any browser on any device.\n");
    perform_full_login(
        paths,
        endpoints,
        python,
        LoginMode::Device { no_browser: false },
    )
    .await?;

    // Step 2 — pick a default model from the shortlist.
    step_header(2, "Choose your model");
    choose_model()?;

    // Step 3 — how model billing / keys work (informational, no secrets
    // collected here — the user sets their own env var in their own
    // shell if they want to bring a key).
    step_header(3, "API keys");
    api_keys_note();

    done();
    Ok(())
}

/// Present the shortlist and persist the pick as the default chat target.
fn choose_model() -> Result<()> {
    println!("  PRISM runs on MARC27's hosted models — billed per call to your");
    println!("  org's credits, no keys to manage. Pick a default (change any time");
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
    println!("  By default every model is served on MARC27's keys — nothing to set up.");
    println!("  To use your \x1b[1mown\x1b[0m provider key instead (billed to you directly):\n");
    println!(
        "    \x1b[2mexport OPENAI_API_KEY=…\x1b[0m   \x1b[2m# or ANTHROPIC_API_KEY, etc.\x1b[0m"
    );
    println!(
        "    \x1b[2mthen switch with \x1b[0m\x1b[1m/use\x1b[0m\x1b[2m inside PRISM (see `prism use --help`)\x1b[0m\n"
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
