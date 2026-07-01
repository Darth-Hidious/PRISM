//! Command registry — the palette primitive (modeled on opencode).
//!
//! opencode's command palette lists every reachable *command* (a
//! declarative action with metadata: title, description, category,
//! keybinding) and dispatches the selected one. This cleanly separates
//! *what the TUI can do* (the catalog) from *how it is triggered*
//! (keybinds / palette / slash-commands).
//!
//! PRISM ports that primitive: existing slash-commands and Ctrl-*
//! toggles become entries in [`CATALOG`], and the Ctrl-P palette lists
//! them with fuzzy filtering. New capabilities register a [`Command`]
//! here and gain palette + (later) which-key support for free.

/// A single palette-reachable command.
///
/// Mirrors opencode's command metadata shape: `name`, `title`,
/// `description`, `category`, keybinding, and a `suggested` flag that
/// floats an entry to the top when the palette query is empty.
#[derive(Debug, Clone)]
pub struct Command {
    /// Stable dispatch id, e.g. `"help.show"`. Matched by [`App::dispatch_command`].
    pub id: &'static str,
    /// Short human label shown in the palette row.
    pub title: &'static str,
    /// One-line description of what the command does.
    pub description: &'static str,
    /// Grouping for future which-key / grouped rendering.
    pub category: &'static str,
    /// Keybinding hint shown right-aligned in the palette footer.
    pub keybind: &'static str,
    /// When true, the entry floats above the rest for an empty query.
    pub suggested: bool,
}

/// The full set of palette-reachable commands.
///
/// Add an entry here, wire it in [`App::dispatch_command`], and it is
/// immediately launchable from the palette. Keep ids in `verb.subject`
/// form to match opencode's command vocabulary.
pub static CATALOG: &[Command] = &[
    Command {
        id: "help.show",
        title: "Help",
        description: "Show keys & commands",
        category: "Help",
        keybind: "/help",
        suggested: true,
    },
    Command {
        id: "which_key.show",
        title: "Keybindings",
        description: "Open the which-key panel",
        category: "Help",
        keybind: "?",
        suggested: true,
    },
    Command {
        id: "theme.list",
        title: "Switch theme",
        description: "Pick a color theme",
        category: "Display",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "session.new",
        title: "New session",
        description: "Back — clear and start fresh",
        category: "Session",
        keybind: "Backspace",
        suggested: true,
    },
    Command {
        id: "gh.show",
        title: "GitHub",
        description: "Issues, PRs & CI status",
        category: "GitHub",
        keybind: "palette",
        suggested: true,
    },
    Command {
        id: "cost.show",
        title: "Cost & tokens",
        description: "Session cost breakdown",
        category: "Session",
        keybind: "/cost",
        suggested: false,
    },
    Command {
        id: "model.show",
        title: "Model",
        description: "Active model + how to switch",
        category: "Session",
        keybind: "/model",
        suggested: false,
    },
    Command {
        id: "mcp.show",
        title: "Tools & MCP",
        description: "MCP configuration",
        category: "Session",
        keybind: "/mcp",
        suggested: false,
    },
    Command {
        id: "goal.set",
        title: "Set goal",
        description: "Standing goal sent to the agent each turn",
        category: "Session",
        keybind: "/goal <text>",
        suggested: false,
    },
    Command {
        id: "chat.clear",
        title: "Clear chat",
        description: "Clear the transcript",
        category: "Session",
        keybind: "Ctrl-L",
        suggested: false,
    },
    Command {
        id: "app.exit",
        title: "Quit",
        description: "Exit PRISM",
        category: "App",
        keybind: "Ctrl-C",
        suggested: false,
    },
    Command {
        id: "thinking.toggle",
        title: "Toggle thinking",
        description: "Show/hide reasoning tokens",
        category: "Display",
        keybind: "Ctrl-T",
        suggested: true,
    },
    Command {
        id: "metrics.toggle",
        title: "Toggle metrics",
        description: "Show/hide tok/s meter",
        category: "Display",
        keybind: "Ctrl-M",
        suggested: false,
    },
    Command {
        id: "cost.toggle",
        title: "Toggle cost bar",
        description: "Show/hide cost in the status bar",
        category: "Display",
        keybind: "Ctrl-$",
        suggested: false,
    },
    Command {
        id: "input.focus",
        title: "Focus input",
        description: "Jump to the message input",
        category: "Navigation",
        keybind: "i / Tab",
        suggested: false,
    },
    Command {
        id: "workspace.activity",
        title: "Workspace: Activity",
        description: "Switch sidebar to Activity",
        category: "Navigation",
        keybind: "←/→",
        suggested: false,
    },
    Command {
        id: "workspace.tools",
        title: "Workspace: Tools",
        description: "Switch sidebar to Tools",
        category: "Navigation",
        keybind: "←/→",
        suggested: false,
    },
    Command {
        id: "workspace.files",
        title: "Workspace: Files",
        description: "Switch sidebar to Files",
        category: "Navigation",
        keybind: "←/→",
        suggested: false,
    },
];

pub fn catalog() -> &'static [Command] {
    CATALOG
}

/// Fuzzy match with a relevance score.
///
/// Returns `Some(score)` when the query matches `title + description + id`
/// (case-insensitive), `None` otherwise. Matching is two-tiered, the way
/// real fuzzy finders (and opencode's filter) rank:
///
/// 1. **Contiguous substring** — the query appears verbatim. Strongly
///    preferred; earlier and word-bounded matches win. This is why `"theme"`
///    ranks `Switch theme` above scattered-letter matches.
/// 2. **Subsequence fallback** — every query char appears in order, scored
///    with consecutive/boundary bonuses and a per-gap penalty.
pub fn fuzzy_match(cmd: &Command, query: &str) -> Option<isize> {
    let q = query.trim();
    if q.is_empty() {
        return Some(0);
    }
    let needle: Vec<char> = q.to_lowercase().chars().collect();
    let haystack: Vec<char> = format!("{} {} {}", cmd.title, cmd.description, cmd.id)
        .to_lowercase()
        .chars()
        .collect();

    // 1) Contiguous substring — literal / word match.
    if needle.len() <= haystack.len()
        && let Some(pos) = haystack
            .windows(needle.len())
            .position(|w| w == needle.as_slice())
    {
        let before = pos == 0 || !haystack[pos - 1].is_alphanumeric();
        let after =
            pos + needle.len() >= haystack.len() || !haystack[pos + needle.len()].is_alphanumeric();
        let mut score: isize = 10_000 - pos as isize;
        if before {
            score += 1_000;
        }
        if after {
            score += 500;
        }
        return Some(score);
    }

    // 2) Subsequence fallback with gap penalty.
    let mut score: isize = 0;
    let mut consecutive: isize = 0;
    let mut last: isize = -1;
    let mut matched = 0usize;
    for (i, c) in haystack.iter().enumerate() {
        if matched < needle.len() && *c == needle[matched] {
            let gap = (i as isize - last - 1).max(0);
            score -= gap * 3;
            consecutive += 1;
            let boundary = i == 0 || !haystack[i - 1].is_alphanumeric();
            score += if boundary { 30 } else { 2 } + consecutive * 4;
            last = i as isize;
            matched += 1;
        } else {
            consecutive = 0;
        }
    }

    (matched == needle.len()).then_some(score)
}

/// All commands matching `query`, ordered for palette display.
///
/// Empty query: suggested entries first, then alphabetical by title.
/// Non-empty query: by descending relevance, then alphabetical.
pub fn fuzzy_sorted(query: &str) -> Vec<&'static Command> {
    let q = query.trim();
    let mut scored: Vec<(isize, bool, &'static Command)> = CATALOG
        .iter()
        .filter_map(|c| fuzzy_match(c, q).map(|s| (s, c.suggested, c)))
        .collect();

    if q.is_empty() {
        scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.title.cmp(b.2.title)));
    } else {
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.title.cmp(b.2.title)));
    }

    scored.into_iter().map(|(_, _, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_no_duplicate_ids() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "duplicate command ids in catalog");
    }

    #[test]
    fn empty_query_lists_everything_suggested_first() {
        let list = fuzzy_sorted("");
        assert_eq!(
            list.len(),
            CATALOG.len(),
            "empty query must return all commands"
        );

        let first_suggested = list.iter().position(|c| c.suggested);
        let first_other = list.iter().position(|c| !c.suggested);
        if let (Some(s), Some(o)) = (first_suggested, first_other) {
            assert!(
                s < o,
                "suggested commands must precede the rest on empty query"
            );
        }
    }

    #[test]
    fn fuzzy_matches_subsequence_across_fields() {
        // "hlp" is a subsequence of title "Help" — should match help.show.
        let ids: Vec<&str> = fuzzy_sorted("hlp").iter().map(|c| c.id).collect();
        assert!(
            ids.contains(&"help.show"),
            "expected help.show to match 'hlp'"
        );

        // "tool" appears in Tools & MCP / workspace.tools titles.
        let ids: Vec<&str> = fuzzy_sorted("tool").iter().map(|c| c.id).collect();
        assert!(ids.contains(&"mcp.show") || ids.contains(&"workspace.tools"));
    }

    #[test]
    fn fuzzy_rejects_non_matching_query() {
        assert!(fuzzy_sorted("zzzzzz").is_empty());
    }

    #[test]
    fn prefix_query_ranks_target_first() {
        // "quit" should rank app.exit (title "Quit") at or near the top.
        let ids: Vec<&str> = fuzzy_sorted("quit").iter().map(|c| c.id).collect();
        assert_eq!(ids.first().copied(), Some("app.exit"));
    }

    #[test]
    fn theme_query_ranks_theme_list_first() {
        let ids: Vec<&str> = fuzzy_sorted("theme").iter().map(|c| c.id).collect();
        assert_eq!(ids.first().copied(), Some("theme.list"), "got: {ids:?}");
    }
}
