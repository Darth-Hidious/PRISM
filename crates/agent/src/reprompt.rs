// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Pre-flight reprompter — recognise a request PRISM cannot answer well, and
//! ask ONE question instead of confidently answering the question the user did
//! not mean.
//!
//! Two distinct failures, both present in the owner's example ("please research
//! the web and find companies that can do that in Poland"):
//!
//! 1. **Underspecification** — which process, which material, what tolerance.
//! 2. **Misrouting** — that is not a materials-property question at all. It is
//!    supplier discovery. Answering it as a materials query produces confident
//!    irrelevance, which is worse than asking.
//!
//! Same enforcement shape as [`crate::execution_contract`]: prompt text is the
//! weakest form of enforcement, so the decision of *whether* to consider
//! reprompting is taken deterministically in the harness. Per the Agent
//! Execution Contract — do not let the model decide WHETHER the required work
//! happens, only HOW.
//!
//! # The cost contract (the test that matters most)
//!
//! A well-formed expert query must pay NOTHING for this feature. That is
//! structural, not a tuning target:
//!
//! - [`triage`] is a pure function over the user's words. No I/O, no LLM, no
//!   allocation beyond one tokenization. It runs in microseconds.
//! - It escalates only on POSITIVE evidence of a problem (an out-of-domain
//!   routing marker, or a short vague directive with no concrete subject).
//!   Absence of detail is never itself a trigger — experts write terse.
//! - Only an escalated turn reaches [`classify`], the one cheap LLM call.
//!
//! So "What is the yield strength of Inconel 718 at 650 C?" adds zero tokens
//! and zero network round-trips. `preflight_is_free_for_expert_queries` asserts
//! exactly that against a client whose endpoint cannot resolve.
//!
//! # Deliberately NOT here
//!
//! Supplier and competitive-landscape discovery themselves. PRISM has no
//! company registry and no procurement data; this module's job is to RECOGNISE
//! that intent and say so plainly, never to fake it with a web search dressed
//! up as materials science.

use crate::types::AgentConfig;
use prism_llm::{ChatMessage, LlmClient};

// ── Intent taxonomy ──────────────────────────────────────────────────

/// What the user is actually asking for. Getting this wrong is worse than
/// asking, which is why the tags are coarse and the fallback is [`Other`]
/// (= proceed, ask nothing).
///
/// [`Other`]: Intent::Other
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// A property, value, or datum about a specific material.
    MaterialsData,
    /// Papers, publications, prior art, what has been reported.
    Literature,
    /// Which companies / shops / vendors can make or supply something.
    /// **PRISM cannot serve this.**
    SupplierDiscovery,
    /// Market landscape, competitors, market size. **PRISM cannot serve this.**
    CompetitiveLandscape,
    /// Designing or improving a synthesis / manufacturing / heat-treatment route.
    ProcessDesign,
    /// Running, deploying, or operating jobs, nodes, or infrastructure.
    ComputeOps,
    /// Anything else — software work, chit-chat, follow-ups. Always proceeds.
    Other,
}

impl Intent {
    /// Stable tag: the classifier's output vocabulary AND the
    /// never-ask-twice ledger key. One string, one meaning.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Intent::MaterialsData => "materials_data",
            Intent::Literature => "literature",
            Intent::SupplierDiscovery => "supplier",
            Intent::CompetitiveLandscape => "competitive",
            Intent::ProcessDesign => "process_design",
            Intent::ComputeOps => "compute_ops",
            Intent::Other => "other",
        }
    }

    /// Every tag, for exhaustive iteration.
    const ALL: [Intent; 7] = [
        Intent::MaterialsData,
        Intent::Literature,
        Intent::SupplierDiscovery,
        Intent::CompetitiveLandscape,
        Intent::ProcessDesign,
        Intent::ComputeOps,
        Intent::Other,
    ];

    /// Can PRISM actually serve this intent today? `false` means the honest
    /// answer is "no, and here is what I can do instead" — never a silent
    /// degrade into a generic web search.
    #[must_use]
    pub fn is_served(self) -> bool {
        !matches!(
            self,
            Intent::SupplierDiscovery | Intent::CompetitiveLandscape
        )
    }

    /// Deterministic routing hint handed to the model when the turn proceeds.
    /// Names the capability that serves the intent, so the agent does not have
    /// to rediscover it — and, for unserved intents, states the gap.
    fn route_hint(self) -> &'static str {
        match self {
            Intent::MaterialsData => {
                "materials property/data lookup — use query / knowledge_entity against the \
                 knowledge graph and cite what comes back; do not answer from memory."
            }
            Intent::Literature => {
                "literature / prior-art search — use research (or start_background_research \
                 for a deep one) and prior_art_search. Not a supplier question."
            }
            Intent::ProcessDesign => {
                "synthesis or process design — retrieve the process window from the graph \
                 and literature first, then reason; state every assumption you make."
            }
            Intent::ComputeOps => {
                "compute / deploy operation — use the node, compute, deploy or run tools \
                 rather than describing what the user should type."
            }
            Intent::SupplierDiscovery | Intent::CompetitiveLandscape => {
                "supplier / competitive discovery — PRISM HAS NO SUCH CAPABILITY. No company \
                 registry, no procurement data. Say so plainly. You may run a web search only \
                 if you label it as a plain web search with no vetting behind it; never present \
                 it as a materials-science answer or as a vetted supplier match."
            }
            // Never routed: `decide` returns Proceed before reaching here.
            Intent::Other => "",
        }
    }

    /// The ONE consolidated question to ask when this intent is unserved, or
    /// when its required subject is missing. Options are drawn from what PRISM
    /// can actually do — a non-expert cannot answer "what tolerance?", but can
    /// pick from a list.
    fn question(self) -> &'static str {
        match self {
            Intent::SupplierDiscovery => {
                "That is a supplier-discovery question, not a materials question, and PRISM \
                 cannot answer it. It has no company registry, no capability directory and no \
                 procurement data — anything it told you about who can machine this would be a \
                 guess dressed up as an answer.\n\n\
                 Here is what it can actually do. Which one do you want?\n\
                 1. Specify the part properly first — material, process, tolerance, quantity — \
                 so you have something precise to send to shops you already know.\n\
                 2. Prior art and literature on the process itself: what it takes to make this \
                 part in this material, and where it usually goes wrong.\n\
                 3. A plain web search, labelled as a plain web search — no vetting, no \
                 materials judgement behind it.\n\n\
                 Reply with 1, 2 or 3 (or tell me the part and material and I will start there)."
            }
            Intent::CompetitiveLandscape => {
                "That is a market/competitive question, not a materials question, and PRISM \
                 cannot answer it. It indexes materials data, literature and process knowledge \
                 — not market share, pricing or company positioning.\n\n\
                 What it can do instead. Which one?\n\
                 1. The published technical landscape: who has reported work on this material \
                 or process, from the literature.\n\
                 2. A capability comparison on technical grounds — what a given process can and \
                 cannot achieve for this part.\n\
                 3. A plain web search, labelled as such, with no analysis behind it.\n\n\
                 Reply with 1, 2 or 3."
            }
            Intent::MaterialsData => {
                "Which material, and which property? Give me as much of this as you have — \
                 partial is fine, one line:\n\
                 1. The material — a grade if you know it (Inconel 718, Ti-6Al-4V, 316L), \
                 otherwise the family (\"a nickel superalloy\") or just the application.\n\
                 2. The property — strength, fatigue life, thermal conductivity, corrosion, \
                 density, something else.\n\
                 3. The condition it matters at — temperature, heat treatment, as-built vs \
                 machined.\n\n\
                 Any one of the three is enough for me to start."
            }
            Intent::Literature => {
                "What should I search the literature for? Give me either:\n\
                 1. a material or process (e.g. \"laser powder bed fusion of AlSi10Mg\"), or\n\
                 2. the problem you are trying to solve (e.g. \"cracking in a thin-wall part\"), \
                 or\n\
                 3. a specific paper, author or DOI to start from.\n\n\
                 Any one of those is enough to start."
            }
            Intent::ProcessDesign => {
                "\"Better\" needs a direction before I can do anything useful. Two things, one \
                 line:\n\n\
                 Which material — a grade if you have it, otherwise the application.\n\n\
                 And better at what:\n\
                 1. Strength or hardness\n\
                 2. High-temperature life (creep, oxidation)\n\
                 3. Corrosion or environmental resistance\n\
                 4. Manufacturability — printability, machinability, weldability\n\
                 5. Cost or supply risk\n\n\
                 Pick a number, and say what must NOT get worse."
            }
            Intent::ComputeOps => {
                "Which operation, and on what? Pick one:\n\
                 1. Run a workflow or job\n\
                 2. Deploy or serve something\n\
                 3. Check status — nodes, jobs, deployments, billing\n\
                 4. Provision or estimate compute\n\n\
                 Then name the target (workflow name, deployment, node, or job id)."
            }
            // Never asked: Other always proceeds.
            Intent::Other => "",
        }
    }
}

// ── Stage A: deterministic triage (no LLM, no tokens) ────────────────

/// Verdict of the deterministic pre-flight gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Triage {
    /// Run the turn exactly as before. Costs nothing. **The default.**
    Proceed,
    /// Positive evidence of a routing or specification problem — worth one
    /// cheap classifier call.
    Classify,
}

/// Decide, with no LLM and no I/O, whether this message is even a CANDIDATE
/// for reprompting.
///
/// Escalates only on positive evidence:
/// 1. an out-of-domain routing marker (supplier / vendor / competitor
///    vocabulary) — these are strongly not materials-property questions, and a
///    concrete subject does not rescue them ("companies in Poland that can
///    machine Inconel 718" is still supplier discovery); or
/// 2. a short vague directive ("make my alloy better") with no concrete
///    subject anywhere in it.
///
/// Everything else proceeds. Terseness alone is never a trigger — experts are
/// terse, and interrogating them makes the feature net negative.
#[must_use]
pub fn triage(user_message: &str) -> Triage {
    let lower = user_message.to_lowercase();
    let words = tokenize(&lower);
    if words.is_empty() {
        return Triage::Proceed;
    }
    if has_routing_marker(&lower, &words) {
        return Triage::Classify;
    }
    if words.len() >= VAGUE_MIN_WORDS
        && words.len() <= VAGUE_MAX_WORDS
        && is_vague_directive(&words)
        && !has_concrete_subject(user_message)
    {
        return Triage::Classify;
    }
    Triage::Proceed
}

/// Below this, a message is chit-chat ("hi", "help me"), not a directive worth
/// an LLM call.
const VAGUE_MIN_WORDS: usize = 3;
/// Above this, the user has written enough that the vagueness rule stops being
/// evidence of anything. The routing rule is deliberately NOT length-bounded.
const VAGUE_MAX_WORDS: usize = 12;

/// Single words that mark a supplier / procurement / market request. Matched on
/// whole tokens so `company` never fires inside another word.
const ROUTING_WORDS: &[&str] = &[
    "supplier",
    "suppliers",
    "vendor",
    "vendors",
    "company",
    "companies",
    "manufacturer",
    "manufacturers",
    "subcontractor",
    "subcontractors",
    "foundry",
    "fabricator",
    "fabricators",
    "distributor",
    "distributors",
    "rfq",
    "quotation",
    "procurement",
    "procure",
    "sourcing",
    "competitor",
    "competitors",
];

/// Multi-word markers, matched on the lowercased message.
const ROUTING_PHRASES: &[&str] = &[
    "machine shop",
    "job shop",
    "who can make",
    "who can do",
    "who can machine",
    "who can print",
    "who can supply",
    "who makes",
    "who supplies",
    "market share",
    "market size",
    "competitive landscape",
];

fn has_routing_marker(lower: &str, words: &[&str]) -> bool {
    words.iter().any(|w| ROUTING_WORDS.contains(w))
        || ROUTING_PHRASES.iter().any(|p| lower.contains(p))
}

/// Verbs whose object needs a direction before the request means anything.
const VAGUE_VERBS: &[&str] = &[
    "make", "improve", "optimize", "optimise", "enhance", "upgrade", "better", "boost", "refine",
    "fix", "help",
];

/// Words that leave the object unnamed — a possessive or a deictic.
const VAGUE_OBJECTS: &[&str] = &[
    "my",
    "our",
    "this",
    "that",
    "it",
    "mine",
    "ours",
    "them",
    "these",
    "those",
    "something",
    "stuff",
    "things",
    "me",
    "us",
];

/// Comparatives that assert a direction without naming one.
const BARE_COMPARATIVES: &[&str] = &[
    "better", "best", "good", "great", "improved", "faster", "cheaper", "stronger", "nicer", "more",
];

/// A vague directive needs a directive verb AND either an unnamed object or a
/// bare comparative. "Make my alloy better" has all three; "make a 20 mm cube"
/// has neither of the latter two.
fn is_vague_directive(words: &[&str]) -> bool {
    let verb = words.iter().any(|w| VAGUE_VERBS.contains(w));
    let unnamed = words.iter().any(|w| VAGUE_OBJECTS.contains(w));
    let comparative = words.iter().any(|w| BARE_COMPARATIVES.contains(w));
    verb && (unnamed || comparative)
}

/// Does the message name something concrete? Deliberately generous — a false
/// "yes" costs nothing (the turn proceeds, which is the status quo), a false
/// "no" risks interrogating someone who was perfectly clear.
///
/// Signals, on the ORIGINAL casing:
/// - a token containing a digit (`718`, `650`, `316L`, `Ti-6Al-4V`, `0.02`)
/// - a path, URL or file extension
/// - a quoted string
/// - a capitalised word that is not the first word of the message (a proper
///   noun: `Inconel`, `Hastelloy`, `Poland`)
fn has_concrete_subject(message: &str) -> bool {
    if message.contains('/')
        || message.contains('\\')
        || message.contains('"')
        || message.contains('`')
    {
        return true;
    }
    let mut first = true;
    for token in message.split_whitespace() {
        let trimmed = token.trim_matches(|c: char| !c.is_alphanumeric());
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.chars().any(|c| c.is_ascii_digit()) {
            return true;
        }
        if !first
            && trimmed.chars().count() > 1
            && trimmed.chars().next().is_some_and(char::is_uppercase)
        {
            return true;
        }
        first = false;
    }
    false
}

/// Lowercase alphabetic-or-digit words.
fn tokenize(lower: &str) -> Vec<&str> {
    lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect()
}

// ── Stage B: cheap LLM intent classification ─────────────────────────

/// The whole classifier prompt. Kept to one screen on purpose: it is the entire
/// recurring cost of this feature, and it only runs on escalated turns.
const CLASSIFIER_SYSTEM: &str = "\
Classify the user's request into exactly ONE intent. Answer with the tag only — \
no punctuation, no explanation.

materials_data  a property, value or datum about a material
literature      papers, publications, prior art, what has been reported
supplier        finding companies, shops, vendors or manufacturers that can make, \
machine, print or supply something
competitive     market landscape, competitors, market share, pricing, positioning
process_design  designing or improving a synthesis, manufacturing or heat-treatment route
compute_ops     running, deploying or operating jobs, nodes, deployments or infrastructure
other           anything else, including software work and conversation";

/// Ask the cheap model for the intent. `None` on any failure — a broken or slow
/// classifier must never block or delay a turn, so every error path proceeds.
async fn classify(llm: &LlmClient, model: &str, user_message: &str) -> Option<Intent> {
    let mut config = llm.config().clone();
    config.model = model.to_string();
    // Hard latency bound: a hung classifier may not stall a user's turn.
    config.timeout_secs = config.timeout_secs.min(CLASSIFIER_TIMEOUT_SECS);
    // The answer is one tag. Anything more is the model padding.
    config.max_output_tokens = Some(CLASSIFIER_MAX_OUTPUT_TOKENS);
    let client = LlmClient::new(config);

    let messages = [
        ChatMessage {
            role: "system".to_string(),
            content: Some(CLASSIFIER_SYSTEM.to_string()),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some(user_message.to_string()),
            tool_calls: None,
            tool_call_id: None,
        },
    ];
    let response = match client.chat_with_tools(&messages, &[]).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, "reprompt: classifier unavailable — proceeding");
            return None;
        }
    };
    if let Some(usage) = &response.usage {
        tracing::debug!(
            model = %model,
            prompt_tokens = usage.prompt_tokens,
            completion_tokens = usage.completion_tokens,
            "reprompt: classifier cost"
        );
    }
    parse_intent(response.message.content.as_deref().unwrap_or(""))
}

/// Longest a classifier call may take before the turn gives up on it and runs
/// unchanged. A user waiting on their own question is the failure being avoided.
const CLASSIFIER_TIMEOUT_SECS: u64 = 15;
/// One tag is the whole answer.
const CLASSIFIER_MAX_OUTPUT_TOKENS: u64 = 16;

/// Pull the tag out of the model's reply. Lenient about surrounding prose,
/// strict about the vocabulary: an unrecognised reply yields `None`, which
/// proceeds.
fn parse_intent(reply: &str) -> Option<Intent> {
    let lower = reply.to_lowercase();
    // Longest tag first, so a reply like "supplier or other" resolves to the
    // specific tag, and `other` — a common English word — is only reached when
    // nothing else fits.
    let mut tags = Intent::ALL;
    tags.sort_by_key(|i| std::cmp::Reverse(i.tag().len()));
    tags.into_iter().find(|i| lower.contains(i.tag()))
}

// ── Decision ─────────────────────────────────────────────────────────

/// What the harness should do with this turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preflight {
    /// Run the turn unchanged.
    Proceed,
    /// Answer the turn with this question and do not run the agent loop.
    Ask {
        /// User-visible text — one consolidated question with options.
        question: String,
        /// Never-ask-twice ledger key (the intent tag).
        key: &'static str,
    },
    /// Run the turn, but hand the model a deterministic routing hint first.
    Route {
        /// One line, injected as a system message for this turn only.
        hint: String,
    },
}

/// Scratchpad `step_type` for the never-ask-twice ledger. The scratchpad
/// outlives the turn on both the TUI and service paths, so it is the session
/// memory this needs — with no new plumbing, and without polluting either the
/// user's transcript or the model's context.
pub const LEDGER_STEP: &str = "reprompt";

/// Prefix of the injected routing hint. Load-bearing: the turn strips its own
/// scaffolding from `history` on completion by matching this.
pub const ROUTE_HINT_PREFIX: &str = "<system-reminder>PRE-FLIGHT ROUTING — ";

/// Run the pre-flight.
///
/// Cost contract: for a message that [`triage`] passes, this returns
/// [`Preflight::Proceed`] having performed no I/O whatsoever.
///
/// `asked_before` is the set of intent tags already asked about this session —
/// the same slot is never asked twice. When it has been asked, the turn
/// proceeds with the routing hint rather than repeating the question.
///
/// `can_ask` is false on unattended turns (subagents, research task steps).
/// There is no human on those paths, so a question would become a dead tool
/// result; they get the routing hint instead, which still carries the honesty.
pub async fn preflight(
    llm: &LlmClient,
    config: &AgentConfig,
    user_message: &str,
    asked_before: &[String],
    can_ask: bool,
) -> Preflight {
    if !enabled() {
        return Preflight::Proceed;
    }
    if triage(user_message) == Triage::Proceed {
        return Preflight::Proceed;
    }
    let Some(intent) = classify(llm, &classifier_model(config), user_message).await else {
        return Preflight::Proceed;
    };
    decide(intent, user_message, asked_before, can_ask)
}

/// The deterministic half of the decision, split out so it is testable without
/// a model. The LLM supplies the intent; the harness decides what happens with
/// it — the model never gets to choose whether the check applies.
#[must_use]
pub fn decide(
    intent: Intent,
    user_message: &str,
    asked_before: &[String],
    can_ask: bool,
) -> Preflight {
    if intent == Intent::Other {
        return Preflight::Proceed;
    }
    let hint = || Preflight::Route {
        hint: format!(
            "{ROUTE_HINT_PREFIX}{}</system-reminder>",
            intent.route_hint()
        ),
    };
    // Already asked this session, or nobody there to answer → route, never
    // re-ask. The hint carries the same honesty the question would have.
    if !can_ask || asked_before.iter().any(|k| k == intent.tag()) {
        return hint();
    }
    // Unserved: say so once, plainly, with what PRISM can do instead.
    // Served but with no concrete subject: one consolidated question.
    // Served with a subject: proceed — prefer a stated assumption over asking.
    if !intent.is_served() || !has_concrete_subject(user_message) {
        return Preflight::Ask {
            question: intent.question().to_string(),
            key: intent.tag(),
        };
    }
    hint()
}

/// `PRISM_REPROMPT=0` / `false` / `off` disables the whole pre-flight.
fn enabled() -> bool {
    env_flag_enabled(std::env::var("PRISM_REPROMPT").ok().as_deref())
}

/// Env-independent core of [`enabled`], so the kill switch is testable without
/// mutating process-global state from a threaded test binary.
fn env_flag_enabled(value: Option<&str>) -> bool {
    match value {
        Some(v) => {
            let v = v.trim();
            !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off"))
        }
        None => true,
    }
}

/// Model used for the one classification call. `PRISM_REPROMPT_MODEL` selects a
/// cheap one; unset falls back to the turn's own model.
///
/// Falling back to the turn's model rather than picking a "cheap" one from the
/// catalog is deliberate: a silent provider switch is exactly the landmine that
/// burned real money once already. The user pays the model they already chose,
/// on ~150 prompt tokens, only on an escalated turn.
fn classifier_model(config: &AgentConfig) -> String {
    std::env::var("PRISM_REPROMPT_MODEL")
        .ok()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| config.model.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── The test that matters most: experts pass through untouched ──

    /// A well-formed expert query must never be escalated. If any of these
    /// starts costing a classifier call, the feature is net negative.
    #[test]
    fn expert_queries_pass_through_deterministically() {
        for q in [
            "What is the yield strength of Inconel 718 at 650 C?",
            "Creep rupture life of CMSX-4 at 1050 C, 137 MPa?",
            "Compare fatigue performance of as-built vs HIPed Ti-6Al-4V.",
            "Thermal conductivity of AlSi10Mg in the as-printed condition",
            "What laser power and scan speed give <0.5% porosity in 316L?",
            "Show me papers on hydrogen embrittlement in martensitic steels",
            "Run the alloy-screen workflow on node gpu-2",
            "deploy the ingest service to staging",
            "read crates/agent/src/prompts.rs and tell me what it does",
            "Why does my Rust build fail with E0507?",
            "hi",
            "thanks, that's what I needed",
            "yes, go ahead",
        ] {
            assert_eq!(triage(q), Triage::Proceed, "expert query escalated: {q}");
        }
    }

    /// Zero I/O on the pass-through path, proven rather than asserted in prose:
    /// the client points at an address that cannot resolve, so ANY network
    /// attempt would surface as a delay and a changed verdict. It returns
    /// Proceed, immediately.
    #[tokio::test]
    async fn preflight_is_free_for_expert_queries() {
        let llm = LlmClient::new(prism_llm::LlmConfig {
            base_url: "http://127.0.0.1:1/unreachable".to_string(),
            model: "does-not-exist".to_string(),
            timeout_secs: 300,
            ..Default::default()
        });
        let config = AgentConfig::default();
        let started = std::time::Instant::now();
        let verdict = preflight(
            &llm,
            &config,
            "What is the yield strength of Inconel 718 at 650 C?",
            &[],
            true,
        )
        .await;
        let elapsed = started.elapsed();
        assert_eq!(verdict, Preflight::Proceed);
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "pass-through must not do I/O; took {elapsed:?}"
        );
    }

    /// The deterministic gate's own latency, measured. Budget is generous by
    /// three orders of magnitude precisely so this never becomes flaky while
    /// still failing loudly if someone puts real work in `triage`.
    #[test]
    fn triage_latency_is_negligible() {
        let q = "What is the yield strength of Inconel 718 at 650 C after a standard \
                 solution and double-age heat treatment?";
        let started = std::time::Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(triage(std::hint::black_box(q)));
        }
        let elapsed = started.elapsed();
        eprintln!("triage: {:?} per expert query", elapsed / 1000);
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "1000 triages took {elapsed:?} — triage is on every turn's hot path"
        );
    }

    // ── The two failures from the owner's example ──────────────────

    #[test]
    fn supplier_and_market_requests_are_escalated() {
        for q in [
            "please research the web and find companies that can do that in Poland",
            "Find companies in Poland that can do this machining",
            "who can machine this part for us in the EU",
            "I need a supplier for titanium powder",
            "which vendors sell this alloy",
            "what's the competitive landscape for metal AM in Germany",
            "who are our competitors in this space",
            "find me a machine shop that does 5-axis milling",
            // A concrete subject does NOT rescue a supplier question.
            "Find companies in Poland that can machine Inconel 718 to 0.02 mm",
        ] {
            assert_eq!(triage(q), Triage::Classify, "not escalated: {q}");
        }
    }

    #[test]
    fn bare_improvement_directives_are_escalated() {
        for q in [
            "Make my alloy better",
            "can you improve this",
            "optimize my material",
            "make it stronger",
            "help me make this better",
        ] {
            assert_eq!(triage(q), Triage::Classify, "not escalated: {q}");
        }
    }

    /// The same vague verb, once a subject is present, is a real request.
    #[test]
    fn a_named_subject_defuses_the_vagueness_rule() {
        for q in [
            "Make Inconel 718 more printable",
            "improve the sintering schedule for AlSi10Mg",
            "optimize this for 316L",
            "help me fix crates/agent/src/reprompt.rs",
        ] {
            assert_eq!(triage(q), Triage::Proceed, "over-escalated: {q}");
        }
    }

    // ── Decision layer ─────────────────────────────────────────────

    #[test]
    fn unserved_intents_are_answered_honestly_not_faked() {
        let Preflight::Ask { question, key } = decide(
            Intent::SupplierDiscovery,
            "find companies in Poland that can machine Inconel 718",
            &[],
            true,
        ) else {
            panic!("supplier discovery must be answered, not silently attempted");
        };
        assert_eq!(key, "supplier");
        assert!(question.contains("cannot answer it"));
        assert!(question.contains("no company registry"));
        // Offers a real alternative, and does not disguise a web search.
        assert!(question.contains("plain web search"));
    }

    /// One question, not three. A non-expert answers a list; they cannot answer
    /// an interrogation.
    #[test]
    fn a_missing_subject_yields_exactly_one_question() {
        let Preflight::Ask { question, key } =
            decide(Intent::ProcessDesign, "Make my alloy better", &[], true)
        else {
            panic!("expected a question");
        };
        assert_eq!(key, "process_design");
        // …and it offers options rather than an open "could you clarify?".
        for option in ["1.", "2.", "3.", "4.", "5."] {
            assert!(question.contains(option), "missing option {option}");
        }
    }

    /// The bar is ONE consolidated question, every time — never an
    /// interrogation, and never an open-ended "could you clarify?".
    #[test]
    fn every_question_asks_exactly_one_thing_and_offers_options() {
        for intent in Intent::ALL {
            if intent == Intent::Other {
                continue;
            }
            let q = intent.question();
            assert!(
                q.matches('?').count() <= 1,
                "{} asks more than one question: {q}",
                intent.tag()
            );
            assert!(
                q.contains("1.") && q.contains("2."),
                "{} offers no concrete options: {q}",
                intent.tag()
            );
            assert!(
                !q.to_lowercase().contains("could you clarify"),
                "{} falls back to an open-ended ask",
                intent.tag()
            );
        }
    }

    #[test]
    fn a_served_intent_with_a_subject_proceeds_with_a_route_hint() {
        let Preflight::Route { hint } = decide(
            Intent::Literature,
            "papers on hydrogen embrittlement in Inconel 718",
            &[],
            true,
        ) else {
            panic!("a well-formed served request must not be interrogated");
        };
        assert!(hint.starts_with(ROUTE_HINT_PREFIX));
        assert!(hint.contains("prior_art_search"));
    }

    /// The naive-keyword failure mode, closed: the deterministic gate fires on
    /// "companies", the classifier says literature, and the turn proceeds
    /// routed correctly instead of being interrogated.
    #[test]
    fn a_misfiring_keyword_is_corrected_by_the_classifier() {
        let q = "which companies have published on additive manufacturing of Inconel 718";
        assert_eq!(triage(q), Triage::Classify);
        assert!(matches!(
            decide(Intent::Literature, q, &[], true),
            Preflight::Route { .. }
        ));
    }

    #[test]
    fn the_same_slot_is_never_asked_twice() {
        let q = "find companies in Poland that can do this machining";
        let Preflight::Ask { key, .. } = decide(Intent::SupplierDiscovery, q, &[], true) else {
            panic!("first ask expected");
        };
        // Second time, with the ledger carrying the key: no repeat question.
        let second = decide(Intent::SupplierDiscovery, q, &[key.to_string()], true);
        assert!(
            matches!(second, Preflight::Route { .. }),
            "asked the same slot twice: {second:?}"
        );
        // …and the hint still carries the honesty, so the agent cannot quietly
        // web-search its way into pretending it has the capability.
        let Preflight::Route { hint } = second else {
            unreachable!()
        };
        assert!(hint.contains("NO SUCH CAPABILITY"));
    }

    /// Unattended turns (subagents, research task steps) have nobody to answer
    /// a question — it would land as a dead tool result. They get the routing
    /// hint instead, which carries the same honesty about the capability gap.
    #[test]
    fn an_unattended_turn_is_routed_never_asked() {
        for (intent, message) in [
            (Intent::SupplierDiscovery, "find companies in Poland"),
            (Intent::ProcessDesign, "make my alloy better"),
        ] {
            let verdict = decide(intent, message, &[], false);
            assert!(
                matches!(verdict, Preflight::Route { .. }),
                "{} asked on an unattended path: {verdict:?}",
                intent.tag()
            );
        }
    }

    #[test]
    fn other_never_asks() {
        assert_eq!(
            decide(Intent::Other, "make my thing better", &[], true),
            Preflight::Proceed
        );
    }

    // ── Classifier reply parsing ───────────────────────────────────

    #[test]
    fn tags_round_trip_and_parse_out_of_prose() {
        for intent in Intent::ALL {
            assert_eq!(parse_intent(intent.tag()), Some(intent));
            assert_eq!(parse_intent(&format!("  {}\n", intent.tag())), Some(intent));
        }
        assert_eq!(
            parse_intent("The tag is: supplier."),
            Some(Intent::SupplierDiscovery)
        );
        // Unrecognised → None → the turn proceeds. Never a guess.
        assert_eq!(parse_intent("I'm not sure what you mean"), None);
        assert_eq!(parse_intent(""), None);
    }

    #[test]
    fn every_askable_intent_has_a_question_and_a_hint() {
        for intent in Intent::ALL {
            if intent == Intent::Other {
                continue;
            }
            assert!(!intent.question().is_empty(), "{}", intent.tag());
            assert!(!intent.route_hint().is_empty(), "{}", intent.tag());
        }
        assert!(Intent::Other.question().is_empty());
    }

    #[test]
    fn the_kill_switch_short_circuits_everything() {
        for off in ["0", "false", "FALSE", "off", " off "] {
            assert!(!env_flag_enabled(Some(off)), "{off} should disable");
        }
        for on in [None, Some(""), Some("1"), Some("true")] {
            assert!(env_flag_enabled(on), "{on:?} should leave it enabled");
        }
    }

    #[test]
    fn concrete_subject_detection() {
        for yes in [
            "Inconel 718",
            "at 650 C",
            "crates/agent/src/lib.rs",
            "the alloy Hastelloy X",
            "make \"that thing\" better",
        ] {
            assert!(has_concrete_subject(yes), "should be concrete: {yes}");
        }
        for no in ["make my alloy better", "improve this", "help us with it"] {
            assert!(!has_concrete_subject(no), "should be vague: {no}");
        }
    }
}
