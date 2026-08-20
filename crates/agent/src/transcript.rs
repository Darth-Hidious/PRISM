//! Transcript management — rolling window with lazy compaction.
//!
//! - Bounded conversation history (not unlimited)
//! - Lazy compaction — only triggered when exceeding threshold
//! - Turn budget enforcement (max turns + max tokens)
//! - Immutable session snapshots for persistence

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

// ── TurnBudget ─────────────────────────────────────────────────────

/// When the current prompt reaches this fraction of the model's usable
/// context, compaction fires. Leaves headroom for the next turn's user
/// message + tool results on top of the compacted history.
const COMPACT_AT_CONTEXT_PCT: f64 = 0.75;

/// Output-token reservation when the catalog doesn't report the model's
/// max output. Deliberately modest — reserving too much shrinks the
/// usable input window on small local models.
const DEFAULT_RESERVED_OUTPUT_TOKENS: u64 = 8_192;

/// Limits for a conversation session.
///
/// Two distinct token concerns live here — do not conflate them:
/// - `max_input_tokens` is a **cumulative spend guard** (sum of prompt
///   tokens across every call in the session; drives `exhausted`/warn).
/// - `context_window` is the model's **per-request** limit from the
///   platform catalog; it drives compaction. `None` = unknown (e.g.
///   local llama.cpp) → compaction falls back to the turn counter.
#[derive(Debug, Clone)]
pub struct TurnBudget {
    pub max_turns: usize,
    pub max_input_tokens: u64,
    pub compact_after_turns: usize,
    pub warn_at_token_pct: f64,
    /// The active model's context window (tokens), if known.
    pub context_window: Option<u64>,
    /// Tokens reserved for the model's response when computing the
    /// usable input window.
    pub reserved_output_tokens: u64,
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self {
            max_turns: 30,
            max_input_tokens: 200_000,
            compact_after_turns: 20,
            warn_at_token_pct: 0.8,
            context_window: None,
            reserved_output_tokens: DEFAULT_RESERVED_OUTPUT_TOKENS,
        }
    }
}

impl TurnBudget {
    /// Budget for a specific model, from platform-catalog metadata.
    /// `None`s are honest unknowns — they select the fallback behavior,
    /// they never assume a size.
    #[must_use]
    pub fn for_model(context_window: Option<u64>, max_output_tokens: Option<u64>) -> Self {
        Self {
            context_window,
            reserved_output_tokens: max_output_tokens.unwrap_or(DEFAULT_RESERVED_OUTPUT_TOKENS),
            ..Self::default()
        }
    }

    /// Tokens available for input once the response reservation is
    /// subtracted. `None` when the window is unknown.
    #[must_use]
    pub fn usable_context(&self) -> Option<u64> {
        self.context_window
            .map(|w| w.saturating_sub(self.reserved_output_tokens))
    }

    /// Optional cumulative-SPEND ceiling for one turn. `None` = no cap.
    ///
    /// Opt-in via `PRISM_MAX_TURN_TOKENS`, because the research loop is supposed
    /// to read and store and read and store: that is inherently many calls, each
    /// re-sending history, and a cumulative cap punishes exactly the behaviour
    /// the loop exists to perform. The principled stop signal is saturation with
    /// facts written, which the harness already computes — an accounting limit
    /// must not pre-empt it.
    #[must_use]
    pub fn max_spend_tokens() -> Option<u64> {
        std::env::var("PRISM_MAX_TURN_TOKENS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
    }

    /// Whether the turn must stop.
    ///
    /// `input_tokens` is CUMULATIVE spend, and it no longer stops a turn by
    /// itself. It used to be compared against `max_input_tokens` — a number
    /// taken from the model's per-request context window — so a turn of many
    /// modest calls died while every individual call sat at a few percent of
    /// that window. All five research runs on 2026-08-20 ended this way.
    ///
    /// The turn COUNT remains the backstop against a genuine runaway, and a
    /// spend ceiling applies only when the operator opts in.
    #[must_use]
    pub fn exhausted(&self, turns: usize, input_tokens: u64) -> bool {
        if turns >= self.max_turns {
            return true;
        }
        Self::max_spend_tokens().is_some_and(|cap| input_tokens >= cap)
    }

    /// Capacity pressure: is the NEXT request likely to crowd the window?
    ///
    /// Measures the most recent request against the usable window, which is
    /// what compaction actually relieves. `None` window (local llama.cpp with
    /// no `/props`) falls back to the turn counter.
    #[must_use]
    pub fn under_capacity_pressure(&self, last_input: u64) -> bool {
        self.usable_context()
            .is_some_and(|usable| last_input >= (usable as f64 * self.warn_at_token_pct) as u64)
    }

    /// Turn-count compaction check — the fallback when the model's
    /// context window is unknown. When it IS known, token pressure
    /// decides instead (see `TranscriptStore::should_compact`).
    #[must_use]
    pub fn should_compact(&self, turns: usize) -> bool {
        turns >= self.compact_after_turns
    }

    /// Check if a token warning should be emitted.
    #[must_use]
    pub fn should_warn(&self, input_tokens: u64) -> bool {
        input_tokens >= (self.max_input_tokens as f64 * self.warn_at_token_pct) as u64
    }
}

// ── CostEvent ──────────────────────────────────────────────────────

/// A single cost event in the audit trail.
#[derive(Debug, Clone)]
pub struct CostEvent {
    pub label: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub timestamp: f64,
}

impl CostEvent {
    #[must_use]
    pub fn new(label: impl Into<String>, input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            label: label.into(),
            input_tokens,
            output_tokens,
            timestamp: now_epoch(),
        }
    }
}

impl std::fmt::Display for CostEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:in={},out={}",
            self.label, self.input_tokens, self.output_tokens
        )
    }
}

// ── CostTracker ────────────────────────────────────────────────────

/// Append-only cost log — auditable, non-blocking.
#[derive(Debug, Clone, Default)]
pub struct CostTracker {
    pub total_input: u64,
    pub total_output: u64,
    /// Input tokens on the MOST RECENT call — the capacity signal.
    ///
    /// `total_input` is a SPEND measure: it sums every call in the turn. It says
    /// nothing about how full the context is, because each call re-sends a
    /// history that compaction may just have shrunk. Comparing that sum to the
    /// context window (which is a PER-REQUEST limit) killed five research runs
    /// that were nowhere near overflowing: measured 2026-08-20, a turn died at
    /// 212,326 cumulative across 30 calls — about 7k per call, 3.3% of the
    /// window it was supposedly exceeding.
    pub last_input: u64,
    pub events: Vec<CostEvent>,
}

impl CostTracker {
    /// Record a cost event.
    pub fn record(&mut self, label: impl Into<String>, input_tokens: u64, output_tokens: u64) {
        self.last_input = input_tokens;
        self.total_input += input_tokens;
        self.total_output += output_tokens;
        self.events
            .push(CostEvent::new(label, input_tokens, output_tokens));
    }

    /// Total tokens consumed.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.total_input + self.total_output
    }

    /// Human-readable summary.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} in, {} out ({} events)",
            self.total_input,
            self.total_output,
            self.events.len()
        )
    }
}

// ── TranscriptEntry ────────────────────────────────────────────────

/// A single entry in the conversation transcript.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    pub role: String,
    pub content: String,
    pub tool_name: Option<String>,
    pub tokens: u64,
    pub timestamp: f64,
}

impl TranscriptEntry {
    #[must_use]
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_name: None,
            tokens: 0,
            timestamp: now_epoch(),
        }
    }

    #[must_use]
    pub fn with_tool_name(mut self, name: impl Into<String>) -> Self {
        self.tool_name = Some(name.into());
        self
    }

    #[must_use]
    pub fn with_tokens(mut self, tokens: u64) -> Self {
        self.tokens = tokens;
        self
    }
}

// ── TranscriptStore ────────────────────────────────────────────────

/// Rolling-window transcript with lazy compaction.
///
/// Maintains a bounded conversation history. When `compact_after_turns`
/// is exceeded, older entries are summarized into a single system message.
pub struct TranscriptStore {
    pub budget: TurnBudget,
    pub entries: Vec<TranscriptEntry>,
    pub turn_count: usize,
    pub cost: CostTracker,
    pub session_id: String,
    compacted: bool,
    /// Prompt size (tokens) of the most recent LLM call — the actual
    /// context pressure against the model's window. Updated from
    /// provider-reported usage via `record_cost`.
    last_prompt_tokens: u64,
}

impl TranscriptStore {
    #[must_use]
    pub fn new(budget: Option<TurnBudget>) -> Self {
        Self {
            budget: budget.unwrap_or_default(),
            entries: Vec::new(),
            turn_count: 0,
            cost: CostTracker::default(),
            session_id: generate_session_id(),
            compacted: false,
            last_prompt_tokens: 0,
        }
    }

    /// Add an entry to the transcript.
    pub fn append(&mut self, entry: TranscriptEntry) {
        if entry.role == "user" || entry.role == "assistant" {
            self.turn_count += 1;
        }
        self.entries.push(entry);
        self.compacted = false;
    }

    /// Record a cost event. LLM calls (nonzero input) also update the
    /// current context-pressure reading.
    pub fn record_cost(&mut self, label: impl Into<String>, input_tokens: u64, output_tokens: u64) {
        if input_tokens > 0 {
            self.last_prompt_tokens = input_tokens;
        }
        self.cost.record(label, input_tokens, output_tokens);
    }

    /// Check if compaction should be triggered.
    ///
    /// Primary signal: **token pressure against the model's real
    /// window** — the last prompt reaching `COMPACT_AT_CONTEXT_PCT` of
    /// the usable input space. Three fat tool results on a 16k model
    /// compact after a couple of turns; twenty tiny turns on a 200k
    /// model don't compact at all. Only when the window is unknown do
    /// we fall back to the old turn counter.
    #[must_use]
    pub fn should_compact(&self) -> bool {
        if self.compacted {
            return false;
        }
        match self.budget.usable_context() {
            Some(usable) if usable > 0 => {
                self.last_prompt_tokens >= (usable as f64 * COMPACT_AT_CONTEXT_PCT) as u64
            }
            _ => self.budget.should_compact(self.turn_count),
        }
    }

    /// Compact older entries into a structured summary, keeping last N.
    ///
    /// Produces a summary with: scope, tools used, recent requests,
    /// pending work (inferred), key files, timeline. Designed so the
    /// agent can resume without losing context.
    pub fn compact(&mut self, keep_last: usize) -> Option<String> {
        if self.entries.len() <= keep_last {
            return None;
        }

        let split_at = self.entries.len() - keep_last;
        let old: Vec<TranscriptEntry> = self.entries.drain(..split_at).collect();
        let recent: Vec<TranscriptEntry> = self.entries.drain(..).collect();

        // Gather data from old entries
        let user_msgs: Vec<&TranscriptEntry> = old.iter().filter(|e| e.role == "user").collect();
        let assistant_msgs: Vec<&TranscriptEntry> =
            old.iter().filter(|e| e.role == "assistant").collect();
        let tool_calls: Vec<&TranscriptEntry> =
            old.iter().filter(|e| e.tool_name.is_some()).collect();
        let all_text: String = old
            .iter()
            .filter(|e| !e.content.is_empty())
            .map(|e| e.content.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        // Build structured summary
        let mut summary_parts = Vec::new();

        // Scope
        summary_parts.push(format!(
            "Conversation summary ({} messages compacted: {} user, {} assistant, {} tool calls)",
            old.len(),
            user_msgs.len(),
            assistant_msgs.len(),
            tool_calls.len()
        ));

        // Tools used (deduplicated, preserving order)
        if !tool_calls.is_empty() {
            let mut seen = HashSet::new();
            let mut tool_names = Vec::new();
            for entry in &tool_calls {
                if let Some(ref name) = entry.tool_name
                    && seen.insert(name.clone())
                {
                    tool_names.push(name.clone());
                }
            }
            summary_parts.push(format!("Tools used: {}", tool_names.join(", ")));
        }

        // Recent user requests (last 3)
        if !user_msgs.is_empty() {
            let recent_topics: Vec<String> = user_msgs
                .iter()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|e| truncate(&e.content.replace('\n', " "), 80))
                .collect();
            summary_parts.push(format!("Recent requests: {}", recent_topics.join(" | ")));
        }

        // Pending work (infer from keywords)
        let pending = extract_pending_work(&all_text, 3);
        if !pending.is_empty() {
            summary_parts.push(format!("Pending work: {}", pending.join("; ")));
        }

        // Key files (extract paths mentioned)
        let files = extract_key_files(&all_text, 8);
        if !files.is_empty() {
            summary_parts.push(format!("Key files: {}", files.join(", ")));
        }

        // Current state (last assistant message)
        if !assistant_msgs.is_empty() {
            let last = assistant_msgs.last().unwrap();
            let truncated = truncate(&last.content.replace('\n', " "), 150);
            summary_parts.push(format!("Last response: {truncated}"));
        }

        let summary = summary_parts.join("\n");

        // Replace entries with summary + recent
        let system_entry = TranscriptEntry {
            role: "system".to_string(),
            content: format!("[Conversation context compacted]\n{summary}"),
            tool_name: None,
            tokens: summary.split_whitespace().count() as u64,
            timestamp: now_epoch(),
        };

        self.entries = std::iter::once(system_entry).chain(recent).collect();
        self.compacted = true;

        Some(summary)
    }

    /// Check if the turn/token budget is exceeded.
    #[must_use]
    pub fn budget_exhausted(&self) -> bool {
        self.budget
            .exhausted(self.turn_count, self.cost.total_input)
    }

    /// Whether history should be compacted right now, mid-turn.
    ///
    /// `should_compact` alone counts TURNS, which never fires inside a single
    /// long turn that makes thirty tool calls — the exact shape of a research
    /// run. Cumulative input is what actually kills that run, so pressure
    /// against the token budget triggers compaction too.
    #[must_use]
    pub fn needs_compaction_under_pressure(&self) -> bool {
        if self.budget.should_compact(self.turn_count) {
            return true;
        }
        // CAPACITY, not spend. This read `cost.total_input` — the cumulative sum
        // across the turn — so it fired on turns whose history was small and
        // never fired on the one oversized request that actually needed it.
        // That is why compaction was observed "firing but not keeping up": it
        // was responding to a number it could not affect.
        self.budget.under_capacity_pressure(self.cost.last_input)
    }

    /// Return a warning message if approaching budget limits.
    #[must_use]
    pub fn budget_warning(&self) -> Option<String> {
        // Report CONTEXT pressure, which is what compaction can relieve. The old
        // line reported cumulative spend against the context window and read as
        // "you are running out of room" when the room was 97% empty.
        if let Some(usable) = self.budget.usable_context()
            && self.budget.under_capacity_pressure(self.cost.last_input)
        {
            let pct = (self.cost.last_input as f64 / usable as f64 * 100.0) as u64;
            return Some(format!(
                "Context: {}% of the usable window on the last request ({} / {})",
                pct, self.cost.last_input, usable
            ));
        }
        if let Some(cap) = TurnBudget::max_spend_tokens()
            && self.cost.total_input >= (cap as f64 * self.budget.warn_at_token_pct) as u64
        {
            return Some(format!(
                "Turn spend: {} / {} tokens (PRISM_MAX_TURN_TOKENS)",
                self.cost.total_input, cap
            ));
        }
        if self.turn_count >= self.budget.max_turns.saturating_sub(3) {
            return Some(format!(
                "Turn budget: {} / {} turns used",
                self.turn_count, self.budget.max_turns
            ));
        }
        None
    }

    /// Convert transcript to message list for LLM API.
    #[must_use]
    pub fn to_messages(&self) -> Vec<TranscriptMessage> {
        self.entries
            .iter()
            .map(|e| {
                let mut msg = TranscriptMessage {
                    role: e.role.clone(),
                    content: e.content.clone(),
                    tool_name: None,
                };
                if let Some(ref name) = e.tool_name {
                    msg.tool_name = Some(name.clone());
                }
                msg
            })
            .collect()
    }

    /// Create an immutable snapshot for persistence.
    #[must_use]
    pub fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            session_id: self.session_id.clone(),
            turn_count: self.turn_count,
            entries: self.entries.clone(),
            cost_events: self.cost.events.clone(),
            total_input_tokens: self.cost.total_input,
            total_output_tokens: self.cost.total_output,
        }
    }
}

/// A message suitable for sending to an LLM API.
#[derive(Debug, Clone)]
pub struct TranscriptMessage {
    pub role: String,
    pub content: String,
    pub tool_name: Option<String>,
}

/// Immutable session state for persistence.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub turn_count: usize,
    pub entries: Vec<TranscriptEntry>,
    pub cost_events: Vec<CostEvent>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

// ── Compaction helpers ─────────────────────────────────────────────

const FILE_EXTENSIONS: &[&str] = &[
    ".rs", ".py", ".ts", ".tsx", ".js", ".json", ".yaml", ".yml", ".toml", ".md", ".csv",
];

/// Infer pending work items from conversation text.
#[must_use]
pub fn extract_pending_work(text: &str, limit: usize) -> Vec<String> {
    let pattern = Regex::new(
        r"(?mi)(?:^|\.\s+)((?:todo|next|pending|remaining|need to|should|will)\b.{10,80})",
    )
    .expect("valid regex");

    let mut results = Vec::new();
    for cap in pattern.captures_iter(text) {
        let clean = cap[1].trim().trim_end_matches('.').to_string();
        if !clean.is_empty() && !results.contains(&clean) {
            results.push(clean);
            if results.len() >= limit {
                break;
            }
        }
    }
    results
}

/// Extract file paths mentioned in conversation text.
#[must_use]
pub fn extract_key_files(text: &str, limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut results = Vec::new();

    for word in text.split_whitespace() {
        if !word.contains('/') {
            continue;
        }
        let clean = word
            .trim_matches(|c: char| {
                matches!(
                    c,
                    '"' | '\'' | '`' | ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}'
                )
            })
            .trim_end_matches('.');

        if let Some(dot_pos) = clean.rfind('.') {
            let ext = &clean[dot_pos..];
            if FILE_EXTENSIONS.contains(&ext) {
                let mut path = clean.to_string();
                // Normalize home dir
                if path.starts_with("/Users/") {
                    let parts: Vec<&str> = path.splitn(4, '/').collect();
                    if parts.len() > 3 {
                        path = format!("~/{}", parts[3]);
                    }
                }
                if seen.insert(path.clone()) {
                    results.push(path);
                    if results.len() >= limit {
                        break;
                    }
                }
            }
        }
    }
    results
}

// ── Internal helpers ───────────────────────────────────────────────

fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn generate_session_id() -> String {
    // Simple hex ID from timestamp + small random component
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:012x}", ts & 0xFFFF_FFFF_FFFF)
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut truncated: String = s.chars().take(max_chars).collect();
        truncated.push('\u{2026}'); // …
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_budget_defaults() {
        let b = TurnBudget::default();
        assert_eq!(b.max_turns, 30);
        assert_eq!(b.max_input_tokens, 200_000);
        assert_eq!(b.compact_after_turns, 20);
        assert!((b.warn_at_token_pct - 0.8).abs() < f64::EPSILON);
    }

    /// The loop is supposed to read and store and read and store. That is
    /// inherently many calls, each re-sending a history compaction keeps small.
    ///
    /// Measured 2026-08-20: five research runs, every one killed by "budget
    /// exhausted", none anywhere near the context window. Run 2 died at 212,326
    /// cumulative over 30 calls — about 7k per call, 3.3% of the 200k window it
    /// was supposedly exceeding. The counter punished the exact behaviour the
    /// harness exists to perform.
    #[test]
    fn a_long_read_and_store_turn_is_not_killed_by_its_own_call_count() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("PRISM_MAX_TURN_TOKENS");
        unsafe { std::env::remove_var("PRISM_MAX_TURN_TOKENS") };

        let budget = TurnBudget::for_model(Some(200_000), Some(8_000));
        let mut t = TranscriptStore::new(Some(budget));

        // Forty calls of ordinary size — 280k cumulative, far past the old
        // ceiling, while no single request uses more than ~4% of the window.
        for _ in 0..40 {
            t.cost.record("read+store", 7_000, 500);
        }
        assert_eq!(t.cost.total_input, 280_000);
        assert!(
            !t.budget_exhausted(),
            "a productive 40-call turn must not be stopped by cumulative spend"
        );
        assert!(
            !t.needs_compaction_under_pressure(),
            "and nothing needs compacting: the history is small"
        );
        assert!(t.budget_warning().is_none(), "no warning either");

        // Capacity is still guarded: one oversized request does compact.
        let usable = t.budget.usable_context().unwrap();
        t.cost.record("huge", usable, 0);
        assert!(
            t.needs_compaction_under_pressure(),
            "a request that crowds the window still triggers compaction"
        );

        match previous {
            Some(v) => unsafe { std::env::set_var("PRISM_MAX_TURN_TOKENS", v) },
            None => unsafe { std::env::remove_var("PRISM_MAX_TURN_TOKENS") },
        }
    }

    #[test]
    fn budget_exhausted() {
        // Serialised: this test manipulates a process-wide env var.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("PRISM_MAX_TURN_TOKENS");
        unsafe { std::env::remove_var("PRISM_MAX_TURN_TOKENS") };

        let b = TurnBudget::default();
        assert!(!b.exhausted(10, 100));
        assert!(b.exhausted(30, 100), "the turn COUNT is still the backstop");

        // Cumulative spend no longer ends a turn on its own. It used to be
        // compared against the model's per-request context window, so a turn of
        // many modest calls died while each call sat at a few percent of that
        // window — the death of all five research runs on 2026-08-20.
        assert!(
            !b.exhausted(10, 200_000),
            "spend alone must not end a turn: read-store-read-store IS many calls"
        );
        assert!(
            !b.exhausted(10, 5_000_000),
            "no cumulative figure ends a turn unless the operator asked for a cap"
        );

        // Opt in, and it binds.
        unsafe { std::env::set_var("PRISM_MAX_TURN_TOKENS", "150000") };
        assert!(b.exhausted(10, 150_000), "an explicit cap is honoured");
        assert!(!b.exhausted(10, 149_999));

        match previous {
            Some(v) => unsafe { std::env::set_var("PRISM_MAX_TURN_TOKENS", v) },
            None => unsafe { std::env::remove_var("PRISM_MAX_TURN_TOKENS") },
        }
    }

    #[test]
    fn small_window_compacts_on_token_pressure_not_turns() {
        // The live failure this guards: a 16k model (local llama.cpp)
        // accumulated 18k of tool results in 3 turns and crashed with
        // exceed_context_size_error because compaction only counted turns.
        let mut store = TranscriptStore::new(Some(TurnBudget::for_model(Some(16_384), None)));
        store.append(TranscriptEntry::new("user", "research question"));
        store.append(TranscriptEntry::new("assistant", "calling tools"));
        assert!(!store.should_compact(), "no pressure yet");
        // usable = 16384 - 8192 reserved = 8192; threshold = 75% = 6144.
        // A 7k-token prompt exceeds it.
        store.record_cost("llm_turn", 7_000, 200);
        assert!(
            store.should_compact(),
            "token pressure on a small window must compact after 2 turns, \
             long before the 20-turn counter"
        );
    }

    #[test]
    fn big_window_does_not_compact_at_20_turns_with_tiny_usage() {
        // The owner's rule: "16k tokens does not mean you compact
        // immediately" — turn count alone must NOT trigger compaction
        // when the model has plenty of window left.
        let mut store =
            TranscriptStore::new(Some(TurnBudget::for_model(Some(200_000), Some(16_384))));
        for i in 0..25 {
            store.append(TranscriptEntry::new("user", format!("q{i}")));
            store.append(TranscriptEntry::new("assistant", format!("a{i}")));
            store.record_cost("llm_turn", 2_000, 100); // tiny prompts
        }
        assert!(
            !store.should_compact(),
            "25 turns at 2k tokens on a 200k window is 1% pressure — \
             compacting here would be the old turn-counter bug"
        );
    }

    #[test]
    fn unknown_window_falls_back_to_turn_count() {
        // Local/offline models with no catalog entry keep the old
        // conservative behavior rather than assuming a size.
        let mut store = TranscriptStore::new(Some(TurnBudget::for_model(None, None)));
        for i in 0..21 {
            store.append(TranscriptEntry::new("user", format!("q{i}")));
            store.append(TranscriptEntry::new("assistant", format!("a{i}")));
        }
        assert!(store.should_compact(), "turn-count fallback must survive");
    }

    #[test]
    fn usable_context_reserves_output() {
        let b = TurnBudget::for_model(Some(200_000), Some(32_000));
        assert_eq!(b.usable_context(), Some(168_000));
        // Reservation larger than the window saturates to zero, never
        // underflows.
        let tiny = TurnBudget::for_model(Some(4_096), Some(8_192));
        assert_eq!(tiny.usable_context(), Some(0));
        assert_eq!(TurnBudget::for_model(None, None).usable_context(), None);
    }

    #[test]
    fn cost_tracker_record_and_summary() {
        let mut tracker = CostTracker::default();
        tracker.record("turn1", 100, 50);
        tracker.record("turn2", 200, 100);
        assert_eq!(tracker.total_input, 300);
        assert_eq!(tracker.total_output, 150);
        assert_eq!(tracker.total_tokens(), 450);
        assert_eq!(tracker.events.len(), 2);
        assert!(tracker.summary().contains("300 in"));
    }

    #[test]
    fn transcript_append_increments_turns() {
        let mut store = TranscriptStore::new(None);
        store.append(TranscriptEntry::new("user", "hello"));
        store.append(TranscriptEntry::new("assistant", "hi"));
        store.append(TranscriptEntry::new("tool", "result").with_tool_name("search"));
        assert_eq!(store.turn_count, 2); // tool doesn't count
        assert_eq!(store.entries.len(), 3);
    }

    #[test]
    fn compact_returns_none_when_few_entries() {
        let mut store = TranscriptStore::new(None);
        store.append(TranscriptEntry::new("user", "hello"));
        store.append(TranscriptEntry::new("assistant", "hi"));
        assert!(store.compact(6).is_none());
    }

    #[test]
    fn compact_produces_summary() {
        let mut store = TranscriptStore::new(None);
        for i in 0..10 {
            store.append(TranscriptEntry::new("user", format!("question {i}")));
            store.append(TranscriptEntry::new("assistant", format!("answer {i}")));
        }
        store.append(
            TranscriptEntry::new("tool", "search result").with_tool_name("search_materials"),
        );

        let summary = store.compact(4);
        assert!(summary.is_some());
        let summary = summary.unwrap();
        assert!(summary.contains("Conversation summary"));
        assert!(summary.contains("user"));
        assert!(summary.contains("assistant"));
        // First entry should be the compacted system message
        assert_eq!(store.entries[0].role, "system");
        assert!(
            store.entries[0]
                .content
                .contains("[Conversation context compacted]")
        );
        // Should have system + 4 recent entries
        assert_eq!(store.entries.len(), 5);
        assert!(store.compacted);
    }

    #[test]
    fn should_compact_respects_flag() {
        let budget = TurnBudget {
            compact_after_turns: 2,
            ..Default::default()
        };
        let mut store = TranscriptStore::new(Some(budget));
        store.append(TranscriptEntry::new("user", "a"));
        store.append(TranscriptEntry::new("assistant", "b"));
        assert!(store.should_compact());

        // After compaction, should_compact returns false
        store.compact(1);
        assert!(!store.should_compact());
    }

    #[test]
    fn budget_warning_tokens() {
        // The warning must describe CONTEXT pressure, which compaction can
        // relieve. It used to compare cumulative spend against the context
        // window and read "you are running out of room" while the room was
        // nearly empty.
        let budget = TurnBudget::for_model(Some(200_000), Some(8_000));
        let mut store = TranscriptStore::new(Some(budget));

        // Thirty modest calls: a lot of SPEND, no capacity problem at all.
        for _ in 0..30 {
            store.record_cost("call", 7_000, 0);
        }
        assert!(
            store.budget_warning().is_none(),
            "210k cumulative across 30 small calls is not a context problem"
        );

        // One request that genuinely crowds the usable window.
        let usable = store.budget.usable_context().unwrap();
        store.record_cost("big", usable, 0);
        let warning = store.budget_warning().expect("a full request must warn");
        assert!(warning.contains("Context:"), "{warning}");
    }

    #[test]
    fn budget_warning_turns() {
        let budget = TurnBudget {
            max_turns: 10,
            ..Default::default()
        };
        let mut store = TranscriptStore::new(Some(budget));
        for _ in 0..8 {
            store.append(TranscriptEntry::new("user", "x"));
        }
        let warning = store.budget_warning();
        assert!(warning.is_some());
        assert!(warning.unwrap().contains("Turn budget"));
    }

    #[test]
    fn to_messages_preserves_tool_name() {
        let mut store = TranscriptStore::new(None);
        store.append(TranscriptEntry::new("tool", "result").with_tool_name("bash"));
        let msgs = store.to_messages();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].tool_name.as_deref(), Some("bash"));
    }

    #[test]
    fn snapshot_captures_state() {
        let mut store = TranscriptStore::new(None);
        store.append(TranscriptEntry::new("user", "hi"));
        store.record_cost("turn1", 50, 30);
        let snap = store.snapshot();
        assert_eq!(snap.turn_count, 1);
        assert_eq!(snap.total_input_tokens, 50);
        assert_eq!(snap.total_output_tokens, 30);
        assert_eq!(snap.entries.len(), 1);
    }

    #[test]
    fn extract_pending_work_finds_keywords() {
        let text = "done. Next: update the tests and remaining CLI polish.";
        let pending = extract_pending_work(text, 3);
        assert!(!pending.is_empty());
        assert!(pending[0].contains("Next"));
    }

    #[test]
    fn extract_key_files_finds_paths() {
        let text = "Update crates/agent/src/transcript.rs and app/agent/core.py next.";
        let files = extract_key_files(text, 8);
        assert!(files.contains(&"crates/agent/src/transcript.rs".to_string()));
        assert!(files.contains(&"app/agent/core.py".to_string()));
    }

    #[test]
    fn extract_key_files_normalizes_home() {
        let text = "See /Users/someone/project/main.rs for details.";
        let files = extract_key_files(text, 8);
        assert!(files.iter().any(|f| f.starts_with("~/")));
    }
    #[test]
    fn a_long_tool_calling_turn_compacts_before_the_budget_kills_it() {
        // The failure this closes: `should_compact` counts TURNS, so a single
        // turn making thirty tool calls never compacted — it grew until the
        // cumulative-input guard ended the run, then compacted on the way out.
        let mut t = TranscriptStore::new(None);
        assert!(
            !t.needs_compaction_under_pressure(),
            "a fresh turn has nothing to compact"
        );

        // Compaction answers CAPACITY, so the trigger is the size of the last
        // request against the usable window — not the turn's cumulative spend,
        // which compaction cannot reduce.
        t.budget = TurnBudget::for_model(Some(200_000), Some(8_000));
        let usable = t.budget.usable_context().unwrap();

        for _ in 0..30 {
            t.cost.record("call", 7_000, 0);
        }
        assert!(
            !t.needs_compaction_under_pressure(),
            "210k spent across 30 small calls needs no compaction: the history is small"
        );

        let warn_at = (usable as f64 * t.budget.warn_at_token_pct) as u64;
        t.cost.record("turn", warn_at - 1, 0);
        assert!(
            !t.needs_compaction_under_pressure(),
            "below the capacity threshold nothing changes"
        );

        t.cost.record("turn", warn_at, 0);
        assert!(
            t.needs_compaction_under_pressure(),
            "at {warn_at} cumulative input tokens the turn must compact, \
             even though turn_count is still {}",
            t.turn_count
        );
    }
}
