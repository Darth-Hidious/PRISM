//! Scratchpad: append-only execution log for the agent.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A single entry in the scratchpad.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScratchpadEntry {
    /// ISO 8601 timestamp.
    pub timestamp: String,
    /// One of: "tool_call", "observation", "decision", "error".
    pub step_type: String,
    /// Tool name, if this entry relates to a tool invocation.
    pub tool_name: Option<String>,
    /// Human-readable summary of what happened.
    pub summary: String,
    /// Optional structured data associated with this entry.
    pub data: Option<Value>,
}

/// Append-only log of agent actions, decisions, and findings.
///
/// The agent writes an entry after every tool call automatically.
/// Can be serialized to Markdown for reports or displayed in the REPL.
#[derive(Debug, Clone, Default)]
pub struct Scratchpad {
    entries: Vec<ScratchpadEntry>,
}

impl Scratchpad {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an entry with the current UTC timestamp.
    pub fn log(
        &mut self,
        step_type: &str,
        tool_name: Option<&str>,
        summary: &str,
        data: Option<Value>,
    ) {
        self.entries.push(ScratchpadEntry {
            timestamp: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            step_type: step_type.to_string(),
            tool_name: tool_name.map(String::from),
            summary: summary.to_string(),
            data,
        });
    }

    /// Return the entries as a slice.
    pub fn entries(&self) -> &[ScratchpadEntry] {
        &self.entries
    }

    /// Render the scratchpad as a Markdown section.
    pub fn to_markdown(&self) -> String {
        if self.entries.is_empty() {
            return "## Methodology\n\n*No actions recorded.*\n".to_string();
        }
        let mut lines = vec!["## Methodology".to_string(), String::new()];
        for (i, e) in self.entries.iter().enumerate() {
            let tool_str = match &e.tool_name {
                Some(name) => format!(" (`{}`)", name),
                None => String::new(),
            };
            lines.push(format!(
                "{}. **{}**{} \u{2014} {}",
                i + 1,
                e.step_type,
                tool_str,
                e.summary
            ));
            lines.push(format!("   *{}*", e.timestamp));
        }
        lines.join("\n")
    }

    /// Plain-text summary for the agent to read its own log.
    pub fn to_text(&self) -> String {
        if self.entries.is_empty() {
            return "Scratchpad is empty.".to_string();
        }
        let mut lines = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            let tool_str = match &e.tool_name {
                Some(name) => format!(" ({})", name),
                None => String::new(),
            };
            lines.push(format!(
                "{}. [{}]{} {} @ {}",
                i + 1,
                e.step_type,
                tool_str,
                e.summary,
                e.timestamp
            ));
        }
        lines.join("\n")
    }

    /// Serialize entries to a Vec of serde_json::Value.
    pub fn to_json(&self) -> Vec<Value> {
        self.entries
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .collect()
    }

    /// Restore a Scratchpad from serialized entries.
    pub fn from_entries(entries: Vec<ScratchpadEntry>) -> Self {
        Self { entries }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_scratchpad_is_empty() {
        let pad = Scratchpad::new();
        assert!(pad.entries().is_empty());
    }

    #[test]
    fn log_appends_entry() {
        let mut pad = Scratchpad::new();
        pad.log("tool_call", Some("search"), "searched for X", None);
        pad.log("observation", None, "found Y", Some(json!({"count": 3})));
        assert_eq!(pad.entries().len(), 2);
        assert_eq!(pad.entries()[0].step_type, "tool_call");
        assert_eq!(pad.entries()[0].tool_name.as_deref(), Some("search"));
        assert_eq!(pad.entries()[1].step_type, "observation");
        assert!(pad.entries()[1].tool_name.is_none());
    }

    #[test]
    fn to_markdown_empty() {
        let pad = Scratchpad::new();
        let md = pad.to_markdown();
        assert!(md.contains("No actions recorded"));
    }

    #[test]
    fn to_markdown_with_entries() {
        let mut pad = Scratchpad::new();
        pad.log("tool_call", Some("db_query"), "queried users", None);
        pad.log("decision", None, "chose plan A", None);
        let md = pad.to_markdown();
        assert!(md.starts_with("## Methodology"));
        assert!(md.contains("**tool_call** (`db_query`)"));
        assert!(md.contains("**decision**"));
        assert!(md.contains("chose plan A"));
    }

    #[test]
    fn to_text_empty() {
        let pad = Scratchpad::new();
        assert_eq!(pad.to_text(), "Scratchpad is empty.");
    }

    #[test]
    fn to_text_with_entries() {
        let mut pad = Scratchpad::new();
        pad.log("error", Some("compute"), "timeout", None);
        let text = pad.to_text();
        assert!(text.starts_with("1. [error] (compute) timeout @"));
    }

    #[test]
    fn to_json_roundtrip() {
        let mut pad = Scratchpad::new();
        pad.log(
            "tool_call",
            Some("fetch"),
            "fetched data",
            Some(json!({"url": "http://x"})),
        );
        let json_vec = pad.to_json();
        assert_eq!(json_vec.len(), 1);
        assert_eq!(json_vec[0]["step_type"], "tool_call");
        assert_eq!(json_vec[0]["tool_name"], "fetch");
    }

    #[test]
    fn from_entries_restores() {
        let entries = vec![
            ScratchpadEntry {
                timestamp: "2026-01-01T00:00:00Z".into(),
                step_type: "decision".into(),
                tool_name: None,
                summary: "decided X".into(),
                data: None,
            },
            ScratchpadEntry {
                timestamp: "2026-01-01T00:01:00Z".into(),
                step_type: "tool_call".into(),
                tool_name: Some("search".into()),
                summary: "searched".into(),
                data: Some(json!({"q": "test"})),
            },
        ];
        let pad = Scratchpad::from_entries(entries);
        assert_eq!(pad.entries().len(), 2);
        assert_eq!(pad.entries()[0].summary, "decided X");
        assert_eq!(pad.entries()[1].tool_name.as_deref(), Some("search"));
    }

    #[test]
    fn timestamp_is_iso_format() {
        let mut pad = Scratchpad::new();
        pad.log("tool_call", None, "test", None);
        let ts = &pad.entries()[0].timestamp;
        // Should match YYYY-MM-DDTHH:MM:SSZ
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 20);
    }
}
