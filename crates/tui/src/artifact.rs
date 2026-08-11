//! Workspace artifact state and formatting policy.
//!
//! Artifact I/O is performed by the agent backend. This module owns only the
//! typed, sanitized state consumed by the pure Ratatui renderer.

use std::{
    io::{self, Write},
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::sanitize::sanitize_for_render;

/// Explicit policy for loading and presenting session artifacts.
///
/// Keeping operational limits here makes truncation and refresh behavior
/// visible and testable instead of hiding magic numbers in event handlers.
#[derive(Debug, Clone)]
pub struct ArtifactPolicy {
    /// Maximum metadata rows requested from the store in one refresh.
    ///
    /// The renderer warns when this ceiling is reached, so a bounded query is
    /// never presented as a complete session history.
    pub list_limit: u64,
    /// Delay used to coalesce startup/turn-complete refresh requests.
    pub refresh_debounce: Duration,
    /// Cadence for refreshing metadata and the display age while idle.
    pub refresh_interval: Duration,
    /// Delay before retrying while the active agent turn owns the tool worker.
    pub busy_retry_delay: Duration,
    /// Fixed number of sidebar lines allocated to each artifact row.
    pub item_lines: usize,
    /// Additional inline-detail lines reserved for the expanded selection.
    pub expanded_lines: usize,
    /// Maximum serialized bytes retained in the interactive detail panel.
    ///
    /// The full value remains in the artifact store; this bound keeps a large
    /// fetch from becoming a multi-megabyte allocation on every render frame.
    pub inspection_bytes: usize,
}

impl Default for ArtifactPolicy {
    fn default() -> Self {
        Self {
            list_limit: 1_000,
            refresh_debounce: Duration::from_millis(150),
            refresh_interval: Duration::from_secs(30),
            busy_retry_delay: Duration::from_secs(1),
            item_lines: 3,
            expanded_lines: 3,
            inspection_bytes: 200 * 1024,
        }
    }
}

/// Whether an artifact has entered the knowledge graph.
///
/// Unknown wire values are retained verbatim. In particular, a missing or
/// malformed value never collapses into `NotPromoted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactPromotion {
    Promoted,
    NotPromoted,
    Unknown(Option<String>),
}

impl ArtifactPromotion {
    fn from_value(value: Option<&Value>) -> Self {
        match value {
            Some(Value::Bool(true)) => Self::Promoted,
            Some(Value::Bool(false)) => Self::NotPromoted,
            Some(Value::String(raw)) => Self::Unknown(Some(sanitize_for_render(raw))),
            Some(raw) => Self::Unknown(Some(sanitize_for_render(&raw.to_string()))),
            None => Self::Unknown(None),
        }
    }

    /// Verbatim unknown value, or `None` when the field was not reported.
    pub fn unknown_raw(&self) -> Option<&str> {
        match self {
            Self::Unknown(raw) => raw.as_deref(),
            Self::Promoted | Self::NotPromoted => None,
        }
    }
}

/// One artifact metadata row in the Workspace sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceArtifact {
    pub id: String,
    pub tool: String,
    pub summary: String,
    /// `None` is the store's explicit SQL `NULL`, used for non-list results.
    pub record_count: Option<u64>,
    pub bytes_size: u64,
    /// Original timestamp retained for inspection and honest fallback display.
    pub created_at: String,
    /// Human-readable age computed once at state ingress, never in render.
    pub age: String,
    pub promotion: ArtifactPromotion,
    pub session_id: String,
}

impl WorkspaceArtifact {
    /// Parse one backend row without inventing missing required metadata.
    pub fn from_value(value: &Value, now: DateTime<Utc>) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "artifact row is not an object".to_string())?;

        let required_text = |field: &str| -> Result<String, String> {
            let raw = object
                .get(field)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("artifact row has no valid `{field}`"))?;
            let clean = sanitize_for_render(raw);
            if matches!(field, "artifact_id" | "session_id") {
                if clean != raw {
                    return Err(format!("artifact row `{field}` contains terminal controls"));
                }
                if clean.trim().is_empty() {
                    return Err(format!("artifact row `{field}` is empty"));
                }
            }
            Ok(clean)
        };

        let record_count = match object.get("record_count") {
            Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .ok_or_else(|| "artifact row has invalid `record_count`".to_string())?,
            ),
            None => return Err("artifact row has no `record_count` field".to_string()),
        };
        let bytes_size = object
            .get("bytes_size")
            .and_then(Value::as_u64)
            .ok_or_else(|| "artifact row has no valid `bytes_size`".to_string())?;
        let created_at = required_text("created_at")?;

        Ok(Self {
            id: required_text("artifact_id")?,
            tool: required_text("tool")?,
            summary: required_text("summary")?,
            record_count,
            bytes_size,
            age: format_age(&created_at, now),
            created_at,
            promotion: ArtifactPromotion::from_value(object.get("promoted_to_kg")),
            session_id: required_text("session_id")?,
        })
    }
}

/// Health/loading state for the current session's artifact store.
#[derive(Debug, Clone, Default)]
pub enum ArtifactStoreState {
    /// A request has not completed yet (including a backend-busy retry).
    #[default]
    Loading,
    /// The store opened and returned a valid list. The list may be empty.
    Ready(Vec<WorkspaceArtifact>),
    /// The store could not be queried or returned a malformed response.
    Unavailable(String),
}

/// Format an exact byte count compactly for the narrow sidebar.
pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes_f = bytes as f64;
    if bytes_f >= GIB {
        format!("{:.1} GiB", bytes_f / GIB)
    } else if bytes_f >= MIB {
        format!("{:.1} MiB", bytes_f / MIB)
    } else if bytes_f >= KIB {
        format!("{:.1} KiB", bytes_f / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Pretty-print fetched content into a bounded, render-safe buffer.
///
/// Serialization stops once the policy budget is reached. The banner is
/// explicit that the store still owns the complete value.
pub fn format_artifact_content(value: &Value, max_bytes: usize) -> String {
    struct BoundedWriter {
        bytes: Vec<u8>,
        limit: usize,
        truncated: bool,
    }

    impl Write for BoundedWriter {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            let remaining = self.limit.saturating_sub(self.bytes.len());
            if input.len() <= remaining {
                self.bytes.extend_from_slice(input);
                return Ok(input.len());
            }
            self.bytes.extend_from_slice(&input[..remaining]);
            self.truncated = true;
            Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "artifact display policy limit reached",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = BoundedWriter {
        bytes: Vec::with_capacity(max_bytes),
        limit: max_bytes,
        truncated: false,
    };
    let serialized = serde_json::to_writer_pretty(&mut writer, value);
    while std::str::from_utf8(&writer.bytes).is_err() {
        writer.bytes.pop();
    }
    let mut rendered = String::from_utf8(writer.bytes).expect("validated UTF-8 prefix");
    if writer.truncated {
        rendered.push_str(&format!(
            "\n\n---\nArtifact display truncated at {} by policy; full content remains in the artifact store.",
            format_bytes(u64::try_from(max_bytes).unwrap_or(u64::MAX))
        ));
    } else if serialized.is_err() {
        return "Artifact content could not be rendered".to_string();
    }
    rendered
}

fn format_age(created_at: &str, now: DateTime<Utc>) -> String {
    let Ok(parsed) = DateTime::parse_from_rfc3339(created_at) else {
        return created_at.to_string();
    };
    let seconds = now
        .signed_duration_since(parsed.with_timezone(&Utc))
        .num_seconds();
    let future = seconds < 0;
    let magnitude = seconds.unsigned_abs();
    let compact = if magnitude < 60 {
        if magnitude < 5 {
            "now".to_string()
        } else {
            format!("{magnitude}s")
        }
    } else if magnitude < 3_600 {
        format!("{}m", magnitude / 60)
    } else if magnitude < 86_400 {
        format!("{}h", magnitude / 3_600)
    } else {
        format!("{}d", magnitude / 86_400)
    };
    if future && compact != "now" {
        format!("in {compact}")
    } else if compact == "now" {
        compact
    } else {
        format!("{compact} ago")
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 11, 12, 0, 0)
            .single()
            .expect("valid fixed time")
    }

    #[test]
    fn parses_complete_metadata_and_age() {
        let artifact = WorkspaceArtifact::from_value(
            &json!({
                "artifact_id": "art_123",
                "tool": "materials_search",
                "summary": "12 candidates",
                "record_count": 12,
                "bytes_size": 1536,
                "created_at": "2026-08-11T10:00:00+00:00",
                "promoted_to_kg": true,
                "session_id": "session-1"
            }),
            now(),
        )
        .expect("valid artifact");

        assert_eq!(artifact.age, "2h ago");
        assert_eq!(artifact.promotion, ArtifactPromotion::Promoted);
        assert_eq!(format_bytes(artifact.bytes_size), "1.5 KiB");
    }

    #[test]
    fn malformed_promotion_is_preserved_not_downgraded_to_false() {
        let artifact = WorkspaceArtifact::from_value(
            &json!({
                "artifact_id": "art_unknown",
                "tool": "future_tool",
                "summary": "future schema",
                "record_count": null,
                "bytes_size": 10,
                "created_at": "not-a-timestamp",
                "promoted_to_kg": "queued",
                "session_id": "session-1"
            }),
            now(),
        )
        .expect("unknown promotion remains a valid row");

        assert_eq!(
            artifact.promotion,
            ArtifactPromotion::Unknown(Some("queued".to_string()))
        );
        assert_ne!(artifact.promotion, ArtifactPromotion::NotPromoted);
        assert_eq!(artifact.age, "not-a-timestamp");
    }

    #[test]
    fn missing_required_metadata_is_not_an_empty_healthy_row() {
        let error = WorkspaceArtifact::from_value(
            &json!({
                "artifact_id": "art_broken",
                "tool": "materials_search",
                "summary": "missing size",
                "record_count": null,
                "created_at": "2026-08-11T10:00:00+00:00",
                "promoted_to_kg": false,
                "session_id": "session-1"
            }),
            now(),
        )
        .expect_err("missing bytes must not be invented");

        assert!(error.contains("bytes_size"), "{error}");
    }

    #[test]
    fn blank_artifact_id_is_rejected() {
        let error = WorkspaceArtifact::from_value(
            &json!({
                "artifact_id": "   ",
                "tool": "materials_search",
                "summary": "invalid identity",
                "record_count": 1,
                "bytes_size": 10,
                "created_at": "2026-08-11T10:00:00+00:00",
                "promoted_to_kg": false,
                "session_id": "session-1"
            }),
            now(),
        )
        .expect_err("blank identity must not enter healthy state");

        assert_eq!(error, "artifact row `artifact_id` is empty");
    }

    #[test]
    fn fetched_content_is_bounded_with_an_explicit_banner() {
        let rendered = format_artifact_content(
            &json!({"result": "abcdefghij", "session_id": "session-1"}),
            12,
        );

        assert!(rendered.contains("Artifact display truncated at 12 B by policy"));
        assert!(!rendered.contains("abcdefghij"));
    }
}
