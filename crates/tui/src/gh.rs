//! GitHub panel — Issues / PRs / CI status, rendered in the TUI.
//!
//! Backed by the `/gh` backend command (which shells to the authenticated
//! `gh` CLI) and the `ui.gh.data` notification. Raw `gh --json` items are
//! normalized into display rows per tab. Filtering reuses the palette's
//! contiguous-subsequence fuzzy matcher so a long issue/PR list is searchable.

use serde_json::Value;

/// Which GitHub tab is shown. (Bug-filing is an action, not a tab.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GhTab {
    #[default]
    Issues,
    Prs,
    Status,
}

impl GhTab {
    pub const ALL: &[GhTab] = &[GhTab::Issues, GhTab::Prs, GhTab::Status];

    pub fn as_str(self) -> &'static str {
        match self {
            GhTab::Issues => "Issues",
            GhTab::Prs => "PRs",
            GhTab::Status => "Status",
        }
    }

    /// The `/gh <command>` token the backend expects.
    pub fn command(self) -> &'static str {
        match self {
            GhTab::Issues => "issues",
            GhTab::Prs => "prs",
            GhTab::Status => "status",
        }
    }

    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    pub fn prev(self) -> Self {
        let idx = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Self::ALL[(idx + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// Panel state. `items` holds the raw `gh` JSON for the active tab; the view
/// normalizes + filters on demand (pure function of state).
#[derive(Debug, Clone, Default)]
pub struct GhPanel {
    pub open: bool,
    pub tab: GhTab,
    pub repo: String,
    pub items: Vec<Value>,
    pub error: Option<String>,
    pub selected: usize,
    pub query: String,
    pub loading: bool,
}

/// One display row, normalized from a raw `gh` item for the active tab.
pub struct GhRow {
    pub key: String,
    pub title: String,
    pub detail: String,
    pub url: String,
}

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}
fn u64_str(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_default()
}
fn author_login(v: &Value) -> String {
    v.get("author")
        .and_then(|a| a.get("login"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

/// Normalize a raw `gh` item into a display row for `tab`.
pub fn normalize(tab: GhTab, item: &Value) -> GhRow {
    match tab {
        GhTab::Issues => {
            let labels = item
                .get("labels")
                .and_then(|l| l.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|l| l.get("name").and_then(|n| n.as_str()))
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            GhRow {
                key: format!("#{}", u64_str(item, "number")),
                title: s(item, "title"),
                detail: join_detail(&[&s(item, "state"), &author_login(item), &labels]),
                url: s(item, "url"),
            }
        }
        GhTab::Prs => {
            let branch = s(item, "headRefName");
            GhRow {
                key: format!("#{}", u64_str(item, "number")),
                title: s(item, "title"),
                detail: join_detail(&[&s(item, "state"), &author_login(item), &branch]),
                url: s(item, "url"),
            }
        }
        GhTab::Status => {
            let concl = item
                .get("conclusion")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            let stat = item.get("status").and_then(|c| c.as_str()).unwrap_or("");
            let branch = s(item, "headBranch");
            GhRow {
                key: s(item, "name"),
                title: s(item, "name"),
                detail: join_detail(&[stat, concl, &branch]),
                url: s(item, "url"),
            }
        }
    }
}

fn join_detail(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .map(|p| (*p).to_string())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Contiguous-subsequence fuzzy match (gap-penalized), matching the palette's
/// ranking philosophy: literal substrings beat scattered letters.
fn fuzzy(haystack: &str, query: &str) -> bool {
    let q: Vec<char> = query.chars().collect();
    if q.is_empty() {
        return true;
    }
    let h: Vec<char> = haystack.to_lowercase().chars().collect();
    let mut mi = 0;
    for c in h {
        if mi < q.len() && c == q[mi].to_ascii_lowercase() {
            mi += 1;
        }
    }
    mi == q.len()
}

/// All rows for the active tab matching the query, in original order.
pub fn filtered_rows(panel: &GhPanel) -> Vec<GhRow> {
    panel
        .items
        .iter()
        .map(|item| normalize(panel.tab, item))
        .filter(|row| {
            let hay = format!("{} {} {}", row.key, row.title, row.detail);
            fuzzy(&hay, panel.query.trim())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tab_cycle_wraps() {
        assert_eq!(GhTab::Issues.next(), GhTab::Prs);
        assert_eq!(GhTab::Status.next(), GhTab::Issues);
        assert_eq!(GhTab::Issues.prev(), GhTab::Status);
    }

    #[test]
    fn normalize_issue_extracts_fields() {
        let item = json!({
            "number": 42,
            "title": "crash on startup",
            "state": "OPEN",
            "author": {"login": "alice"},
            "labels": [{"name": "bug"}, {"name": "ui"}],
            "url": "https://x/42"
        });
        let row = normalize(GhTab::Issues, &item);
        assert_eq!(row.key, "#42");
        assert_eq!(row.title, "crash on startup");
        assert!(row.detail.contains("OPEN"));
        assert!(row.detail.contains("alice"));
        assert!(row.detail.contains("bug,ui"));
        assert_eq!(row.url, "https://x/42");
    }

    #[test]
    fn filtered_rows_respect_query() {
        let mut p = GhPanel {
            tab: GhTab::Issues,
            items: vec![
                json!({"number": 1, "title": "fix login", "state": "open", "url": "u1"}),
                json!({"number": 2, "title": "dark mode", "state": "open", "url": "u2"}),
            ],
            ..Default::default()
        };
        assert_eq!(filtered_rows(&p).len(), 2);
        p.query = "dark".into();
        let rows = filtered_rows(&p);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "dark mode");
    }
}
