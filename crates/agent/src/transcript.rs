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

/// Limits for a conversation session.
#[derive(Debug, Clone)]
pub struct TurnBudget {
    pub max_turns: usize,
    pub max_input_tokens: u64,
    pub compact_after_turns: usize,
    pub warn_at_token_pct: f64,
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self {
            max_turns: 30,
            max_input_tokens: 200_000,
            compact_after_turns: 20,
            warn_at_token_pct: 0.8,
        }
    }
}

impl TurnBudget {
    /// Check if the budget is exhausted.
    #[must_use]
    pub fn exhausted(&self, turns: usize, input_tokens: u64) -> bool {
        turns >= self.max_turns || input_tokens >= self.max_input_tokens
    }

    /// Check if compaction should be triggered.
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
    pub events: Vec<CostEvent>,
}

impl CostTracker {
    /// Record a cost event.
    pub fn record(&mut self, label: impl Into<String>, input_tokens: u64, output_tokens: u64) {
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

    /// Record a cost event.
    pub fn record_cost(&mut self, label: impl Into<String>, input_tokens: u64, output_tokens: u64) {
        self.cost.record(label, input_tokens, output_tokens);
    }

    /// Check if compaction should be triggered.
    #[must_use]
    pub fn should_compact(&self) -> bool {
        self.budget.should_compact(self.turn_count) && !self.compacted
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
                if let Some(ref name) = entry.tool_name {
                    if seen.insert(name.clone()) {
                        tool_names.push(name.clone());
                    }
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

    /// Return a warning message if approaching budget limits.
    #[must_use]
    pub fn budget_warning(&self) -> Option<String> {
        if self.budget.should_warn(self.cost.total_input) {
            let pct =
                (self.cost.total_input as f64 / self.budget.max_input_tokens as f64 * 100.0) as u64;
            return Some(format!(
                "Token budget: {}% used ({} / {})",
                pct, self.cost.total_input, self.budget.max_input_tokens
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

    #[test]
    fn budget_exhausted() {
        let b = TurnBudget::default();
        assert!(!b.exhausted(10, 100));
        assert!(b.exhausted(30, 100));
        assert!(b.exhausted(10, 200_000));
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
        assert!(store.entries[0]
            .content
            .contains("[Conversation context compacted]"));
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
        let mut store = TranscriptStore::new(None);
        store.record_cost("big", 170_000, 0);
        let warning = store.budget_warning();
        assert!(warning.is_some());
        assert!(warning.unwrap().contains("Token budget"));
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
}
