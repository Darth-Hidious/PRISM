// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
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
//! - [`triage`] is a pure function over the user's words: no I/O, no LLM, one
//!   lowercase copy and one token vector. ~9 µs on a debug build.
//! - It escalates only on POSITIVE evidence of a problem (an out-of-domain
//!   routing marker, or an opening directive that names nothing at all).
//!   Absence of detail is never itself a trigger — experts write terse.
//! - Only an escalated turn reaches [`classify`], the one cheap LLM call, whose
//!   token usage is returned so the caller bills it like any other.
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
use prism_ingest::ontologies::Ontology;
use prism_llm::{ChatMessage, LlmClient, UsageInfo};

// ── Domain vocabulary (served from the ACTIVE ontology) ────────────

/// The domain words the reprompt surfaces use, served from the ACTIVE
/// ontology — never a Rust domain list. Rust keeps the STRUCTURE (three
/// slots, "any one is enough", a numbered menu); the ontology supplies its
/// own class and quantity vocabulary, so a legal deployment's menus name
/// cases and obligations while a materials deployment's name alloys and
/// properties.
#[derive(Debug, Clone)]
pub struct DomainVocabulary {
    /// The ontology's subject-kind class labels (EMMO: `Alloy`, `Material`,
    /// `Phase`, …; a legal ontology: `Case`, `Verdict`, …).
    pub subject_kinds: Vec<String>,
    /// The ontology's quantitative class labels — the measurable targets a
    /// "make it better" direction can pick from.
    pub quantity_kinds: Vec<String>,
}

impl DomainVocabulary {
    /// Derive the vocabulary from the ontology's OWN declarations: every
    /// extraction class label for subjects, every quantitative label for
    /// quantities. An ontology that declares nothing serves empty menus —
    /// the questions stay structural and name no kinds, rather than
    /// inventing domain words in Rust.
    #[must_use]
    pub fn from_ontology(ontology: &dyn Ontology) -> Self {
        let mut subject_kinds: Vec<String> = ontology
            .classes()
            .iter()
            .flat_map(|class| class.extraction_labels.iter().cloned())
            .collect();
        subject_kinds.sort();
        subject_kinds.dedup();
        let mut quantity_kinds: Vec<String> = ontology
            .quantitative_labels()
            .into_iter()
            .map(String::from)
            .collect();
        quantity_kinds.sort();
        Self {
            subject_kinds,
            quantity_kinds,
        }
    }

    /// Resolve the vocabulary for a project: the ontology its `prism.toml`
    /// selects, falling back to the built-in default when the project has
    /// no readable configuration (menus then carry the default ontology's
    /// words — logged, never silent, and still ontology-sourced).
    #[must_use]
    pub fn for_project(project_root: &std::path::Path) -> Self {
        match prism_ingest::ontologies::active_for_project_config(project_root) {
            Ok(ontology) => Self::from_ontology(ontology.as_ref()),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "reprompt: project ontology unresolvable — domain menus fall back to the default ontology"
                );
                let default = prism_ingest::ontologies::active(None)
                    .expect("the built-in default ontology is always registered");
                Self::from_ontology(default.as_ref())
            }
        }
    }

    /// The category nouns for the vagueness heuristic: the ontology's own
    /// class labels (lowercased — "my alloy", "my case") plus the
    /// language-level filler nouns below. These stand in for a subject
    /// without being one: "my alloy" is not a material, "Hastelloy" is.
    fn generic_nouns(&self) -> Vec<String> {
        let mut nouns: Vec<String> = self
            .subject_kinds
            .iter()
            .map(|kind| kind.to_lowercase())
            .collect();
        nouns.extend(LANGUAGE_FILLER_NOUNS.iter().map(|noun| (*noun).to_string()));
        nouns
    }

    /// `"an alloy, a material, …"` — the subject kinds as a menu fragment.
    fn subject_kind_list(&self) -> String {
        self.subject_kinds
            .iter()
            .map(|kind| format!("a {kind}"))
            .take(6)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Language-level filler nouns — generic ENGLISH object words, not domain
/// knowledge (they stand in for a subject in any domain). The domain half
/// of the old hardcoded category-noun list (`alloy`, `material`, `metal`,
/// …) now comes from the active ontology's declared classes instead.
const LANGUAGE_FILLER_NOUNS: &[&str] = &[
    "part",
    "parts",
    "sample",
    "component",
    "product",
    "design",
    "recipe",
    "setup",
    "system",
    "model",
    "thing",
    "things",
    "stuff",
    "something",
    "one",
    "ones",
];

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
    /// when its required subject is missing. The STRUCTURE (three slots,
    /// "any one is enough", a numbered menu) is Rust's; the KINDS are the
    /// active ontology's own declarations — a legal deployment's menus name
    /// cases and obligations, a materials deployment's name alloys and
    /// properties. No grade names or property instances are invented here:
    /// the ontology declares classes, not data.
    fn question(self, vocabulary: &DomainVocabulary) -> String {
        let kinds = vocabulary.subject_kind_list();
        let kind_phrase = if kinds.is_empty() {
            "a specific one if you know it".to_string()
        } else {
            format!("a specific one if you know it, otherwise the kind ({kinds})")
        };
        match self {
            Intent::SupplierDiscovery => {
                "That is a supplier-discovery question, and PRISM cannot answer it. It has \
                 no company registry, no capability directory and no procurement data — \
                 anything it told you about who can make this would be a guess dressed up \
                 as an answer.\n\n\
                 Here is what it can actually do. Which one do you want?\n\
                 1. Specify the request properly first — subject, process, tolerance, \
                 quantity — so you have something precise to send to shops you already know.\n\
                 2. Prior art and literature on the process itself: what it takes to do \
                 this work, and where it usually goes wrong.\n\
                 3. A plain web search, labelled as a plain web search — no vetting, no \
                 technical judgement behind it.\n\n\
                 Reply with 1, 2 or 3 (or tell me the subject and I will start there)."
                    .to_string()
            }
            Intent::CompetitiveLandscape => {
                "That is a market/competitive question, and PRISM cannot answer it. It \
                 indexes its configured domain's knowledge graph, literature and process \
                 knowledge — not market share, pricing or company positioning.\n\n\
                 What it can do instead. Which one?\n\
                 1. The published technical landscape: who has reported work on this \
                 subject or process, from the literature.\n\
                 2. A capability comparison on technical grounds — what a given process can \
                 and cannot achieve for this case.\n\
                 3. A plain web search, labelled as such, with no analysis behind it.\n\n\
                 Reply with 1, 2 or 3."
                    .to_string()
            }
            Intent::MaterialsData => {
                let quantities = if vocabulary.quantity_kinds.is_empty() {
                    "name the quantity you need directly".to_string()
                } else {
                    format!(
                        "one of the kinds this deployment models ({}) — or name another",
                        vocabulary.quantity_kinds.join(", ")
                    )
                };
                format!(
                    "Which subject, and which quantity? Give me as much of this as you \
                     have — partial is fine, one line:\n\
                     1. The subject — {kind_phrase}, or just the wider application.\n\
                     2. The quantity — {quantities}.\n\
                     3. The condition it matters under — any circumstance that changes it.\n\n\
                     Any one of the three is enough for me to start."
                )
            }
            Intent::Literature => {
                format!(
                    "What should I search the literature for? Give me either:\n\
                     1. a subject or process ({kind_phrase}), or\n\
                     2. the problem you are trying to solve, or\n\
                     3. a specific paper, author or DOI to start from.\n\n\
                     Any one of those is enough to start."
                )
            }
            Intent::ProcessDesign => {
                // The menu of improvement directions is the ontology's own
                // quantity vocabulary — what this deployment can actually
                // measure — plus an explicit "name another" slot, so the
                // menu is never empty.
                let mut options: Vec<String> = vocabulary
                    .quantity_kinds
                    .iter()
                    .map(|kind| format!("{kind} (say which one)"))
                    .collect();
                options.push("Something else — name the quantity".to_string());
                let menu = options
                    .iter()
                    .enumerate()
                    .map(|(index, option)| format!("{}. {option}", index + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "\"Better\" needs a direction before I can do anything useful. Two \
                     things, one line:\n\n\
                     Which subject — {kind_phrase}.\n\n\
                     And better at what:\n{menu}\n\n\
                     Pick a number, and say what must NOT get worse."
                )
            }
            Intent::ComputeOps => "Which operation, and on what? Pick one:\n\
                 1. Run a workflow or job\n\
                 2. Deploy or serve something\n\
                 3. Check status — nodes, jobs, deployments, billing\n\
                 4. Provision or estimate compute\n\n\
                 Then name the target (workflow name, deployment, node, or job id)."
                .to_string(),
            // Never asked: Other always proceeds.
            Intent::Other => String::new(),
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
/// 1. an out-of-domain routing marker (supplier / vendor / market vocabulary)
///    — these are strongly not materials questions, and naming a material does
///    not rescue them ("companies in Poland that can machine Inconel 718" is
///    still supplier discovery); or
/// 2. an OPENING directive that names nothing at all ("make my alloy better"):
///    a directive verb, a possessive or bare comparative, and not one word in
///    the whole message outside the closed filler vocabulary — EXCEPT a bare
///    speed request ("make it faster"), which is the terse software ask on a
///    coding-capable agent rather than materials vagueness. See
///    [`is_bare_speed_request`].
///
/// Everything else proceeds. Terseness alone is never a trigger — experts are
/// terse, and interrogating them makes the feature net negative.
///
/// `has_prior_context` suppresses rule 2 mid-conversation. "Now make it
/// stronger" names nothing on its own, but after a turn about Inconel 718 it is
/// anaphoric and perfectly clear; re-asking there is the interrogation this
/// feature exists to avoid. The routing rule is NOT suppressed — "find
/// companies in Poland" is a misroute whenever it arrives.
#[must_use]
pub fn triage(
    user_message: &str,
    has_prior_context: bool,
    vocabulary: &DomainVocabulary,
) -> Triage {
    let lower = user_message.to_lowercase();
    let words = tokenize(&lower);
    if words.is_empty() {
        return Triage::Proceed;
    }
    if has_routing_marker(&lower, &words) {
        return Triage::Classify;
    }
    let generic_nouns = vocabulary.generic_nouns();
    if !has_prior_context
        && words.len() >= VAGUE_MIN_WORDS
        && words.len() <= VAGUE_MAX_WORDS
        && is_vague_directive(&words)
        && names_nothing(&words, &generic_nouns)
        && !is_bare_speed_request(&words, &generic_nouns)
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

/// Single words that mark a supplier / procurement request. Matched on whole
/// tokens so `vendor` never fires inside another word.
///
/// Deliberately EXCLUDED, because each is ordinary materials vocabulary and
/// would tax a well-formed expert query with a classifier call it does not
/// need: `foundry` ("foundry alloy", "foundry defects"), `manufacturer`
/// ("manufacturer datasheet"), `sourcing` ("powder sourcing route"),
/// `competitor` ("Alloy 625's main competitor, C276"), `company`. The
/// company-seeking senses of those are carried by [`ROUTING_PHRASES`] instead.
const ROUTING_WORDS: &[&str] = &[
    "supplier",
    "suppliers",
    "vendor",
    "vendors",
    "companies",
    "subcontractor",
    "subcontractors",
    "distributor",
    "distributors",
    "rfq",
    "quotation",
    "procurement",
];

/// Multi-word markers, matched on the lowercased message. These carry the
/// company-seeking senses of the words kept out of [`ROUTING_WORDS`].
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
    "which company",
    "which manufacturer",
    "find a manufacturer",
    "our competitors",
    "the competition",
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
    "my", "our", "this", "that", "it", "mine", "ours", "them", "these", "those", "me", "us",
];

/// Comparatives that assert a direction without naming one.
///
/// This list is DOUBLE-DUTY — it feeds both [`is_vague_directive`] (a
/// comparative can stand in for an unnamed object) and [`names_nothing`] (as
/// closed filler vocabulary). Removing a word from it therefore changes two
/// rules at once; `faster` is handled by [`is_bare_speed_request`] instead,
/// precisely so it keeps both roles for the materials case.
const BARE_COMPARATIVES: &[&str] = &[
    "better", "best", "good", "great", "improved", "faster", "cheaper", "stronger", "nicer", "more",
];

/// CONTRACT CHANGE: the category nouns that stand in for a subject without
/// being one ("my alloy", "my case") are no longer a hardcoded Rust domain
/// list. The DOMAIN half is served from the active ontology's declared
/// classes through [`DomainVocabulary::generic_nouns`]; only the
/// language-level filler nouns ([`LANGUAGE_FILLER_NOUNS`]) stay in Rust.
/// `code` is deliberately excluded from both — it is the object of the most
/// ordinary terse request a coding-capable agent gets ("fix my code"), which
/// PRISM serves by reading the repo, not by asking which property the user
/// meant.
///
/// Function words that carry no subject. Includes the fragments an apostrophe
/// tokenizes to (`ve`, `s`, `t`, …).
const FUNCTION_WORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "of", "to", "for", "with", "at", "in", "on", "so", "if", "but",
    "is", "are", "be", "been", "do", "does", "did", "can", "could", "would", "should", "will",
    "please", "you", "i", "we", "some", "any", "just", "now", "then", "really", "up", "out", "s",
    "t", "ve", "m", "re", "ll", "d",
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

/// "Make it faster" with no category noun anywhere in it.
///
/// Speed is the one direction that is ordinarily a SOFTWARE ask on an agent
/// that reads code, runs builds and profiles — work PRISM does directly rather
/// than by asking which property the user meant. The narrowest thing that
/// separates it from the materials case is the presence of a category noun:
/// "make my process faster" names a process category and is a real
/// process-design question with no process named; "make it faster" and "make my
/// code faster" name no category at all and are the terse dev request.
///
/// Done here rather than by deleting `faster` from [`BARE_COMPARATIVES`],
/// because that constant is double-duty: dropping the word also made
/// "make my process faster" and "make the process faster" stop escalating —
/// a regression an adversarial review caught and
/// `narrowing_the_software_case_did_not_disarm_the_materials_case` now pins.
fn is_bare_speed_request(words: &[&str], generic_nouns: &[String]) -> bool {
    words.contains(&"faster")
        && !words
            .iter()
            .any(|w| generic_nouns.iter().any(|noun| noun == w))
}

/// Does the message name NOTHING — is every single word drawn from the closed
/// filler vocabulary?
///
/// This replaced a capitalisation heuristic ("a capitalised word that is not
/// the first word is a proper noun"), which made the verdict depend on the
/// user's typing habits rather than on content: `make hastelloy better` was
/// treated as nameless and interrogated, while `MAKE MY ALLOY BETTER` was
/// treated as specific and let through. A closed-vocabulary test has neither
/// failure — an unknown word is a named thing, in any casing and any language.
fn names_nothing(words: &[&str], generic_nouns: &[String]) -> bool {
    words.iter().all(|w| {
        VAGUE_VERBS.contains(w)
            || VAGUE_OBJECTS.contains(w)
            || BARE_COMPARATIVES.contains(w)
            || generic_nouns.iter().any(|noun| noun == w)
            || FUNCTION_WORDS.contains(w)
    })
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
async fn classify(
    llm: &LlmClient,
    model: &str,
    user_message: &str,
) -> (Option<Intent>, Option<UsageInfo>) {
    let mut config = llm.config().clone();
    config.model = model.to_string();
    // Per-request latency bound. NOT an absolute one: `LlmClient` applies its
    // own retry policy around the request, so a pathological backend can cost a
    // small multiple of this. It is a bound on one attempt, which is what stops
    // a default 300s timeout from parking a user's turn.
    config.timeout_secs = config.timeout_secs.min(CLASSIFIER_TIMEOUT_SECS);
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
            return (None, None);
        }
    };
    let intent = parse_intent(response.message.content.as_deref().unwrap_or(""));
    tracing::debug!(
        model = %model,
        intent = intent.map_or("none", Intent::tag),
        prompt_tokens = response.usage.as_ref().map(|u| u.prompt_tokens),
        completion_tokens = response.usage.as_ref().map(|u| u.completion_tokens),
        "reprompt: classifier"
    );
    (intent, response.usage)
}

/// Bound on ONE classifier attempt before the turn gives up on it and runs
/// unchanged. A user waiting on their own question is the failure being avoided.
const CLASSIFIER_TIMEOUT_SECS: u64 = 15;
/// The reply is one word. This is 256 rather than a token or two because
/// `LlmClient::effective_max_tokens` floors the requested output at 256 — a
/// smaller number here would be silently raised, so it would lie.
const CLASSIFIER_MAX_OUTPUT_TOKENS: u64 = 256;

/// Pull the tag out of the model's reply.
///
/// Strict on purpose. A bare tag wins outright; otherwise the reply must
/// mention EXACTLY ONE tag. Two tags ("this isn't literature, it's supplier
/// discovery") is ambiguity, and ambiguity yields `None`, which proceeds — a
/// positional or longest-match tie-break there would pick a plausible wrong
/// intent, and a wrong intent is the failure this whole module exists to
/// prevent.
fn parse_intent(reply: &str) -> Option<Intent> {
    let lower = reply.to_lowercase();
    let bare = lower.trim().trim_matches(|c: char| !c.is_alphanumeric());
    if let Some(exact) = Intent::ALL.into_iter().find(|i| i.tag() == bare) {
        return Some(exact);
    }
    let mut mentioned = Intent::ALL.into_iter().filter(|i| lower.contains(i.tag()));
    match (mentioned.next(), mentioned.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
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

/// Usage returned by the optional classifier together with the model that
/// actually incurred it. The turn can therefore add mixed-model costs without
/// repricing every token as though the primary model produced it.
pub struct PreflightUsage {
    pub usage: UsageInfo,
    pub model: String,
}

/// Prefix of the injected routing hint. Load-bearing: the turn strips any stale
/// hint out of `history` by matching this.
pub const ROUTE_HINT_PREFIX: &str = "<system-reminder>PRE-FLIGHT ROUTING — ";

/// Run the pre-flight.
///
/// Cost contract: for a message that [`triage`] passes, this returns
/// `(Preflight::Proceed, None)` having performed no I/O whatsoever.
///
/// `history` is the session's conversation, and carries BOTH pieces of session
/// state this needs, with no extra plumbing and no separate ledger:
/// - whether there is prior context (an anaphoric follow-up is not vague), and
/// - which questions have already been asked — the questions are built
///   deterministically from the session's ontology vocabulary, so a previous
///   ask is an exact match on an assistant message.
///
/// It is deliberately not the scratchpad: `service.rs` builds a fresh
/// `Scratchpad` per turn and `restore_history_and_transcript_from_messages`
/// clears it on resume, so a scratchpad ledger would be silently inert on the
/// HTTP surface and lost on every `/resume`. `history` survives both.
///
/// `can_ask` is false on unattended turns (subagents, research task steps).
/// There is no human on those paths, so a question would become a dead tool
/// result; they get the routing hint instead, which still carries the honesty.
///
/// The returned usage is the classifier's, for the caller to bill. `None` means
/// no call was made (or the backend reported no usage).
pub async fn preflight(
    llm: &LlmClient,
    config: &AgentConfig,
    user_message: &str,
    history: &[ChatMessage],
    can_ask: bool,
    vocabulary: &DomainVocabulary,
) -> (Preflight, Option<PreflightUsage>) {
    if !enabled() {
        return (Preflight::Proceed, None);
    }
    if triage(user_message, has_prior_context(history), vocabulary) == Triage::Proceed {
        return (Preflight::Proceed, None);
    }
    let model = classifier_model(config);
    let (intent, usage) = classify(llm, &model, user_message).await;
    let usage = usage.map(|usage| PreflightUsage { usage, model });
    let Some(intent) = intent else {
        return (Preflight::Proceed, usage);
    };
    (
        decide(
            intent,
            user_message,
            &asked_before(history, vocabulary),
            can_ask,
            vocabulary,
        ),
        usage,
    )
}

/// Has the assistant already spoken in this session? Only prior turns can have
/// put an assistant message in `history`; the current user message is pushed
/// before the pre-flight runs, and it is a user message.
fn has_prior_context(history: &[ChatMessage]) -> bool {
    history.iter().any(|m| m.role == "assistant")
}

/// Intents already asked about in this session, recovered from the questions
/// themselves. Exact equality against the built question text — no marker to
/// leak into the user's transcript, nothing extra to persist. The vocabulary
/// is the session's own (resolved from the same project config each turn), so
/// a re-ask under the same ontology matches exactly.
fn asked_before(history: &[ChatMessage], vocabulary: &DomainVocabulary) -> Vec<Intent> {
    Intent::ALL
        .into_iter()
        .filter(|intent| {
            let question = intent.question(vocabulary);
            !question.is_empty()
                && history.iter().any(|m| {
                    m.role == "assistant" && m.content.as_deref() == Some(question.as_str())
                })
        })
        .collect()
}

/// The deterministic half of the decision, split out so it is testable without
/// a model. The LLM supplies the intent; the harness decides what happens with
/// it — the model never gets to choose whether the check applies.
#[must_use]
pub fn decide(
    intent: Intent,
    user_message: &str,
    asked_before: &[Intent],
    can_ask: bool,
    vocabulary: &DomainVocabulary,
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
    if !can_ask || asked_before.contains(&intent) {
        return hint();
    }
    // Unserved: say so once, plainly, with what PRISM can do instead.
    // Served but naming nothing: one consolidated question.
    // Served and naming something: proceed — prefer a stated assumption.
    if !intent.is_served()
        || names_nothing(
            &tokenize(&user_message.to_lowercase()),
            &vocabulary.generic_nouns(),
        )
    {
        return Preflight::Ask {
            question: intent.question(vocabulary),
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

    // CONTRACT CHANGE: the domain words (menus, category nouns) come from
    // the ACTIVE ontology. The EMMO adapter stands in for a materials
    // deployment here; a synthetic legal vocabulary below pins that a
    // non-materials deployment gets non-materials menus.
    fn emmo_vocabulary() -> DomainVocabulary {
        DomainVocabulary::from_ontology(&prism_ingest::ontologies::EmmoOntology)
    }

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
            assert_eq!(
                triage(q, false, &emmo_vocabulary()),
                Triage::Proceed,
                "expert query escalated: {q}"
            );
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
        let (verdict, usage) = preflight(
            &llm,
            &config,
            "What is the yield strength of Inconel 718 at 650 C?",
            &[],
            true,
            &emmo_vocabulary(),
        )
        .await;
        let elapsed = started.elapsed();
        assert_eq!(verdict, Preflight::Proceed);
        assert!(usage.is_none(), "pass-through must not bill anything");
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
            std::hint::black_box(triage(std::hint::black_box(q), false, &emmo_vocabulary()));
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
            assert_eq!(
                triage(q, false, &emmo_vocabulary()),
                Triage::Classify,
                "not escalated: {q}"
            );
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
            assert_eq!(
                triage(q, false, &emmo_vocabulary()),
                Triage::Classify,
                "not escalated: {q}"
            );
        }
    }

    /// DEFECT (adversarial review): the vagueness rule taxed terse SOFTWARE
    /// requests. PRISM is a coding-capable agent — its own tests assert that
    /// "Why does my Rust build fail with E0507?" must Proceed — so these are
    /// ordinary, not vague. When the classifier answers `other` the cost is one
    /// silent round-trip the module's own doc calls "net negative"; when it
    /// answers anything else it is worse, and `reprompt_cost_parity` measured
    /// that worse case: "fix my code" was answered with "Which material, and
    /// which property?" instead of reaching the model at all.
    ///
    /// Two causes, two different fixes: `code` sat in `GENERIC_NOUNS` (a list
    /// meant for "my alloy" / "my material") and was simply removed, and a
    /// bare speed request is now excluded by `is_bare_speed_request` — NOT by
    /// deleting `faster` from `BARE_COMPARATIVES`, which is double-duty and
    /// took the materials case down with it.
    #[test]
    fn terse_software_requests_are_not_taxed() {
        for q in [
            "fix my code",
            "improve my code",
            "optimize my code",
            "make it faster",
            "make my code faster",
        ] {
            assert_eq!(
                triage(q, false, &emmo_vocabulary()),
                Triage::Proceed,
                "over-escalated: {q}"
            );
        }
    }

    /// …and the materials-domain rule this feature exists for is untouched.
    /// Both edits were domain-neutral vocabulary: every remaining generic noun
    /// still stands in for a materials/process subject, and every remaining
    /// bare comparative still asserts a materials-ish direction with no
    /// dimension named.
    #[test]
    fn narrowing_the_software_case_did_not_disarm_the_materials_case() {
        for q in [
            "Make my alloy better",
            "optimize my material",
            "make the design better",
            "can you improve this",
            "make it stronger",
            "make my part cheaper",
            // `faster` is double-duty vocabulary: it feeds BOTH
            // `is_vague_directive` (as a comparative) and `names_nothing` (as
            // filler). Narrowing the SOFTWARE case must not take the
            // materials/process case with it — "make my process faster" is a
            // process-design request with no process named.
            "make my process faster",
            "make the process faster",
            "make my recipe faster",
            "make my setup faster",
        ] {
            assert_eq!(
                triage(q, false, &emmo_vocabulary()),
                Triage::Classify,
                "no longer caught: {q}"
            );
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
            assert_eq!(
                triage(q, false, &emmo_vocabulary()),
                Triage::Proceed,
                "over-escalated: {q}"
            );
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
            &emmo_vocabulary(),
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
        // CONTRACT CHANGE: this used to pin the five HARDCODED materials
        // directions (strength, creep, corrosion, manufacturability, cost).
        // The menu is now the ontology's own quantity vocabulary plus an
        // explicit "name another" slot — for EMMO that is the declared
        // `Property` class and one fallback — so the structural assertion is
        // "a numbered menu with at least two options", not a fixed count.
        let Preflight::Ask { question, key } = decide(
            Intent::ProcessDesign,
            "Make my alloy better",
            &[],
            true,
            &emmo_vocabulary(),
        ) else {
            panic!("expected a question");
        };
        assert_eq!(key, "process_design");
        // …and it offers options rather than an open "could you clarify?".
        assert!(question.contains("1."), "missing first option: {question}");
        assert!(question.contains("2."), "missing second option: {question}");
        // The EMMO menu names its own declared quantity kind.
        assert!(question.contains("Property"), "{question}");
        assert!(question.contains("must NOT get worse"), "{question}");
    }

    /// CONTRACT CHANGE (dehardcoding): the menus and the vagueness nouns
    /// come from the ACTIVE ontology. A synthetic LEGAL vocabulary — no
    /// materials word anywhere — must produce legal menus and treat the
    /// legal category nouns as generic, exactly as EMMO's treated
    /// "alloy"/"material". Zero Rust edits, different domain, same
    /// structure.
    #[test]
    fn a_non_materials_ontology_serves_non_materials_menus() {
        struct LegalVocabulary {
            classes: Vec<prism_ingest::ontologies::ClassDecl>,
        }
        impl LegalVocabulary {
            fn new() -> Self {
                let decl = |label: &str| prism_ingest::ontologies::ClassDecl {
                    iri: prism_ingest::ontologies::Iri::new(format!(
                        "https://example.test/legal/{label}"
                    ))
                    .unwrap(),
                    pref_label: Some(label.to_string()),
                    parents: Vec::new(),
                    extraction_labels: vec![label.to_string()],
                };
                Self {
                    classes: vec![decl("Case"), decl("Verdict")],
                }
            }
        }
        impl prism_ingest::ontologies::Ontology for LegalVocabulary {
            fn id(&self) -> &'static str {
                "reprompt-test-legal"
            }
            fn version_iri(&self) -> &prism_ingest::ontologies::Iri {
                static IRI: std::sync::OnceLock<prism_ingest::ontologies::Iri> =
                    std::sync::OnceLock::new();
                IRI.get_or_init(|| {
                    prism_ingest::ontologies::Iri::new(
                        "https://example.test/ontology/legal/1".to_string(),
                    )
                    .unwrap()
                })
            }
            fn artifact_sha256(&self) -> &str {
                "0000000000000000000000000000000000000000000000000000000000000000"
            }
            fn classes(&self) -> &[prism_ingest::ontologies::ClassDecl] {
                &self.classes
            }
            fn relations(&self) -> &[prism_ingest::ontologies::RelationDecl] {
                &[]
            }
            fn is_a(
                &self,
                sub: &prism_ingest::ontologies::Iri,
                sup: &prism_ingest::ontologies::Iri,
            ) -> bool {
                sub == sup
            }
            fn quantitative_labels(&self) -> Vec<&str> {
                vec!["DamagesAmount", "SentenceLength"]
            }
        }
        let legal = DomainVocabulary::from_ontology(&LegalVocabulary::new());

        // The vague-directive rule fires on the LEGAL category noun where
        // EMMO's fired on "alloy" — and materials nouns are no longer
        // special under it.
        assert_eq!(
            triage("make my case better", false, &legal),
            Triage::Classify,
            "the ontology's own category nouns are the generic ones"
        );
        assert_eq!(
            triage("make my case better", false, &emmo_vocabulary()),
            Triage::Proceed,
            "a materials vocabulary does not know \"case\" — it names something"
        );

        // The ProcessDesign menu names the LEGAL quantity kinds.
        let Preflight::Ask { question, .. } = decide(
            Intent::ProcessDesign,
            "make my case better",
            &[],
            true,
            &legal,
        ) else {
            panic!("expected a question");
        };
        assert!(question.contains("DamagesAmount"), "{question}");
        assert!(question.contains("SentenceLength"), "{question}");
        for banned in [
            "alloy",
            "material",
            "Inconel",
            "strength",
            "creep",
            "corrosion",
            "property",
        ] {
            assert!(
                !question.to_lowercase().contains(banned),
                "materials vocabulary {banned:?} leaked into a legal menu: {question}"
            );
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
            let q = intent.question(&emmo_vocabulary());
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
            &emmo_vocabulary(),
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
        assert_eq!(triage(q, false, &emmo_vocabulary()), Triage::Classify);
        assert!(matches!(
            decide(Intent::Literature, q, &[], true, &emmo_vocabulary()),
            Preflight::Route { .. }
        ));
    }

    #[test]
    fn the_same_slot_is_never_asked_twice() {
        let q = "find companies in Poland that can do this machining";
        assert!(
            matches!(
                decide(Intent::SupplierDiscovery, q, &[], true, &emmo_vocabulary()),
                Preflight::Ask { .. }
            ),
            "first ask expected"
        );
        // Second time, with the ledger carrying the key: no repeat question.
        let second = decide(
            Intent::SupplierDiscovery,
            q,
            &[Intent::SupplierDiscovery],
            true,
            &emmo_vocabulary(),
        );
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
            let verdict = decide(intent, message, &[], false, &emmo_vocabulary());
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
            decide(
                Intent::Other,
                "make my thing better",
                &[],
                true,
                &emmo_vocabulary(),
            ),
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
            assert!(
                !intent.question(&emmo_vocabulary()).is_empty(),
                "{}",
                intent.tag()
            );
            assert!(!intent.route_hint().is_empty(), "{}", intent.tag());
        }
        assert!(Intent::Other.question(&emmo_vocabulary()).is_empty());
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

    /// The heuristic that decides "this message names nothing" must depend on
    /// CONTENT, not on the user's shift key. The two rows below are the exact
    /// regressions an adversarial review found in the previous capitalisation
    /// rule: a lowercase alloy name was treated as nameless (and interrogated),
    /// and an ALL-CAPS possessive was treated as a proper noun (and let
    /// through).
    #[test]
    fn naming_detection_is_independent_of_capitalisation() {
        for names_something in [
            "Inconel 718",
            "make hastelloy better",
            "MAKE HASTELLOY BETTER",
            "at 650 C",
            "improve the sintering schedule",
            "can you help fix this bug",
        ] {
            assert!(
                !names_nothing(
                    &tokenize(&names_something.to_lowercase()),
                    &emmo_vocabulary().generic_nouns(),
                ),
                "should be treated as naming something: {names_something}"
            );
        }
        for names_nothing_at_all in [
            "make my alloy better",
            "MAKE MY ALLOY BETTER",
            "improve this",
            "can you improve it",
            "make the design better",
        ] {
            assert!(
                names_nothing(
                    &tokenize(&names_nothing_at_all.to_lowercase()),
                    &emmo_vocabulary().generic_nouns(),
                ),
                "should be treated as naming nothing: {names_nothing_at_all}"
            );
        }
    }

    /// Ordinary materials vocabulary that happens to overlap supplier words
    /// must not tax an expert with a classifier call. Every one of these was
    /// escalated by the first version of the marker list.
    #[test]
    fn metallurgy_vocabulary_is_not_a_supplier_marker() {
        for q in [
            "What foundry defects are typical in A356 T6 sand-cast wheels?",
            "How does Alloy 625 corrosion resistance compare to its main competitor, C276?",
            "Effect of powder sourcing route (gas- vs plasma-atomised) on porosity in IN718",
            "Check the manufacturer datasheet for AlSi10Mg mechanical properties",
            "Can you help fix this bug?",
        ] {
            assert_eq!(
                triage(q, false, &emmo_vocabulary()),
                Triage::Proceed,
                "over-escalated: {q}"
            );
        }
    }

    /// A follow-up is anaphoric: "now make it stronger" names nothing on its
    /// own, but after a turn about Inconel 718 the subject is in the history
    /// and re-asking would be exactly the interrogation this must avoid.
    #[test]
    fn an_anaphoric_follow_up_is_not_treated_as_vague() {
        for q in [
            "Now make it stronger",
            "make it better",
            "ok, optimise it for cost instead",
        ] {
            assert_eq!(
                triage(q, true, &emmo_vocabulary()),
                Triage::Proceed,
                "follow-up interrogated: {q}"
            );
        }
        // The same words as an OPENING message still escalate: with no history
        // behind them they genuinely name nothing.
        assert_eq!(
            triage("Now make it stronger", false, &emmo_vocabulary()),
            Triage::Classify
        );
        assert_eq!(
            triage("make it better", false, &emmo_vocabulary()),
            Triage::Classify
        );
        // A misroute is a misroute whenever it arrives — context never excuses it.
        assert_eq!(
            triage(
                "which companies in Poland can machine it",
                true,
                &emmo_vocabulary()
            ),
            Triage::Classify
        );
    }

    /// The never-ask-twice ledger and the prior-context flag both come out of
    /// `history`, which is what resume restores on every transport.
    #[test]
    fn history_carries_the_ledger_and_the_context_flag() {
        let assistant = |text: &str| ChatMessage {
            role: "assistant".to_string(),
            content: Some(text.to_string()),
            tool_calls: None,
            tool_call_id: None,
        };
        let user = ChatMessage {
            role: "user".to_string(),
            content: Some("find companies in Poland".to_string()),
            tool_calls: None,
            tool_call_id: None,
        };

        assert!(!has_prior_context(std::slice::from_ref(&user)));
        assert!(asked_before(std::slice::from_ref(&user), &emmo_vocabulary()).is_empty());

        let after_ask = vec![
            user,
            assistant(&Intent::SupplierDiscovery.question(&emmo_vocabulary())),
        ];
        assert!(has_prior_context(&after_ask));
        assert_eq!(
            asked_before(&after_ask, &emmo_vocabulary()),
            vec![Intent::SupplierDiscovery]
        );

        // An ordinary answer is not a ledger entry.
        let ordinary = vec![assistant("Inconel 718 yields about 1030 MPa at 650 C.")];
        assert!(has_prior_context(&ordinary));
        assert!(asked_before(&ordinary, &emmo_vocabulary()).is_empty());
    }

    /// An ambiguous classifier reply must NOT be resolved by a tie-break — a
    /// plausible wrong intent is the failure this module exists to prevent.
    #[test]
    fn an_ambiguous_classifier_reply_proceeds_rather_than_guesses() {
        assert_eq!(
            parse_intent("this isn't literature, it's supplier discovery"),
            None
        );
        assert_eq!(parse_intent("materials_data or process_design"), None);
        // One tag mentioned in prose still resolves.
        assert_eq!(
            parse_intent("The tag is: competitive."),
            Some(Intent::CompetitiveLandscape)
        );
    }
}
