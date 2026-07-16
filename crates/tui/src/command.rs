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
    // ── Science — what a materials scientist came here to do ─────────
    Command {
        id: "knowledge.open",
        title: "Knowledge",
        description: "Search & ingest — one pane",
        category: "Science",
        keybind: "palette",
        suggested: true,
    },
    // Alias of knowledge.open (Search tab): hidden while browsing so
    // the list has one Knowledge entry, still matched when typed.
    Command {
        id: "sci.search",
        title: "Search literature & KG",
        description: "Find papers, entities & facts",
        category: "Science",
        keybind: "palette",
        suggested: true,
    },
    Command {
        id: "sci.properties",
        title: "Material properties",
        description: "Structure, lattice, moduli, band gap…",
        category: "Science",
        keybind: "palette",
        suggested: true,
    },
    Command {
        id: "sci.simulate",
        title: "Run simulation",
        description: "Relax/MD via MACE + pyiron",
        category: "Science",
        keybind: "palette",
        suggested: true,
    },
    Command {
        id: "sci.predict",
        title: "Predict properties",
        description: "ML prediction for a composition",
        category: "Science",
        keybind: "palette",
        suggested: true,
    },
    Command {
        id: "sci.research",
        title: "Deep research",
        description: "Multi-step research with citations",
        category: "Science",
        keybind: "palette",
        suggested: true,
    },
    Command {
        id: "sci.ingest",
        title: "Ingest data/paper",
        description: "Add a file or paper to the KG",
        category: "Science",
        keybind: "palette",
        suggested: true,
    },
    Command {
        id: "compute.gpus",
        title: "Procure GPU compute",
        description: "Live GPU catalog with prices",
        category: "Science",
        keybind: "palette",
        suggested: true,
    },
    Command {
        id: "nodes.show",
        title: "Nodes",
        description: "See your connected nodes",
        category: "Science",
        keybind: "palette",
        suggested: true,
    },
    Command {
        id: "node.up",
        title: "Node up",
        description: "Bring this machine online as a node",
        category: "Science",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "node.stop",
        title: "Node stop",
        description: "Stop the local node daemon",
        category: "Science",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "node.status",
        title: "Node status",
        description: "Local daemon + platform registration",
        category: "Science",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "sci.notebook",
        title: "Notebook",
        description: "Python notebook — persistent kernel shared with the agent",
        category: "Science",
        keybind: "palette",
        suggested: true,
    },
    // ── Goals (long-running discovery campaigns) ──────────────────────
    Command {
        id: "campaign.start",
        title: "Start goal",
        description: "Long-running discovery campaign — billable",
        category: "Science",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "campaign.list",
        title: "List goals",
        description: "Discovery campaigns on this node",
        category: "Science",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "campaign.status",
        title: "Goal status",
        description: "Check a discovery campaign's progress",
        category: "Science",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "campaign.resume",
        title: "Resume goal",
        description: "Continue a paused discovery campaign — billable",
        category: "Science",
        keybind: "palette",
        suggested: false,
    },
    // ── Workflows (YAML automations) ──────────────────────────────────
    Command {
        id: "workflow.list",
        title: "Workflows",
        description: "List discovered YAML workflows",
        category: "Automation",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "workflow.show",
        title: "Show workflow",
        description: "Inspect one workflow's arguments",
        category: "Automation",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "workflow.run",
        title: "Run workflow",
        description: "Run (or dry-run) a workflow by name",
        category: "Automation",
        keybind: "palette",
        suggested: false,
    },
    // ── Marketplace ────────────────────────────────────────────────────
    Command {
        id: "marketplace.search",
        title: "Marketplace search",
        description: "Search downloadable tools & workflows",
        category: "Marketplace",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "marketplace.find",
        title: "Marketplace find",
        description: "Semantic discovery by what a tool does",
        category: "Marketplace",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "marketplace.install",
        title: "Marketplace install",
        description: "Install a tool or workflow by name",
        category: "Marketplace",
        keybind: "palette",
        suggested: false,
    },
    // ── Skills (self-authored reusable snippets) ──────────────────────
    Command {
        id: "skills.list",
        title: "Skills",
        description: "Your saved reusable shell/python skills",
        category: "Skills",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "skills.run",
        title: "Run skill",
        description: "Execute a saved skill by name",
        category: "Skills",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "skills.create",
        title: "Create skill",
        description: "Author & verify a reusable shell/python skill",
        category: "Skills",
        keybind: "palette",
        suggested: false,
    },
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
        suggested: false,
    },
    Command {
        id: "account.show",
        title: "Account",
        description: "MARC27 login / logout & status",
        category: "Account",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "sessions.show",
        title: "Sessions",
        description: "Resume a saved session",
        category: "Session",
        keybind: "palette",
        suggested: false,
    },
    // ── Backend slash commands (each runs the real command) ──────────
    Command {
        id: "tools.show",
        title: "Tool catalog",
        description: "Browse the agent tools (approval-marked)",
        category: "Reference",
        keybind: "palette",
        suggested: true,
    },
    Command {
        id: "slash.context",
        title: "Show context usage",
        description: "Live API-facing context summary",
        category: "Diagnostics",
        keybind: "/context",
        suggested: false,
    },
    Command {
        id: "slash.files",
        title: "Show tracked files",
        description: "Files currently in focus",
        category: "Diagnostics",
        keybind: "/files",
        suggested: false,
    },
    Command {
        id: "slash.tasks",
        title: "Show task list",
        description: "Pending work inferred from session",
        category: "Diagnostics",
        keybind: "/tasks",
        suggested: false,
    },
    Command {
        id: "slash.memory",
        title: "Show memory",
        description: "Recent session memory & pending work",
        category: "Diagnostics",
        keybind: "/memory",
        suggested: false,
    },
    Command {
        id: "slash.permissions",
        title: "Show tool permissions",
        description: "Tool access & blocking rules",
        category: "Diagnostics",
        keybind: "/permissions",
        suggested: false,
    },
    Command {
        id: "home.show",
        title: "Mission Control",
        description: "The launch dashboard — workflows, tools, systems",
        category: "Display",
        keybind: "palette",
        suggested: true,
    },
    Command {
        id: "status.show",
        title: "Status dashboard",
        description: "Runtime dashboard (model, session, counts, cost)",
        category: "Settings",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "slash.usage",
        title: "Show usage stats",
        description: "Usage & budget details",
        category: "Diagnostics",
        keybind: "/usage",
        suggested: false,
    },
    Command {
        id: "slash.doctor",
        title: "Run doctor checks",
        description: "Runtime diagnostics",
        category: "Diagnostics",
        keybind: "/doctor",
        suggested: false,
    },
    Command {
        id: "config.show",
        title: "Config",
        description: "View prism.toml / .mcp.json / config files",
        category: "Settings",
        keybind: "palette",
        suggested: false,
    },
    Command {
        id: "apikey.show",
        title: "API Keys",
        description: "Add provider keys (Anthropic / OpenAI / Google …)",
        category: "Settings",
        keybind: "palette",
        suggested: true,
    },
    Command {
        id: "slash.billing",
        title: "Billing & credits",
        description: "MARC27 balance, usage & prices",
        category: "Settings",
        keybind: "/billing",
        suggested: false,
    },
    Command {
        id: "slash.billing topup",
        title: "Buy credits",
        description: "Open a credit-pack checkout in the browser",
        category: "Settings",
        keybind: "/billing topup",
        suggested: false,
    },
    Command {
        id: "use.show",
        title: "Chat target",
        description: "Show the active chat route (MARC27 / local / provider)",
        category: "Settings",
        keybind: "/use show",
        suggested: false,
    },
    Command {
        id: "slash.diff",
        title: "Show diff",
        description: "Git diff for the repo",
        category: "Diagnostics",
        keybind: "/diff",
        suggested: false,
    },
    Command {
        id: "slash.compact",
        title: "Compact conversation",
        description: "Compact older conversation context",
        category: "Session",
        keybind: "/compact",
        suggested: false,
    },
    Command {
        id: "cost.show",
        title: "Cost & token details",
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
        title: "MCP servers",
        description: "External MCP setup & status",
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
        id: "links.open",
        title: "Open link",
        description: "Open a URL from the transcript in the browser",
        category: "Session",
        keybind: "o",
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
        id: "copy.toggle",
        title: "Copy mode",
        description: "Disable mouse capture for drag-to-select / copy",
        category: "Display",
        keybind: "Ctrl-Y",
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

/// Palette aliases: kept for muscle memory (they match a typed query
/// and dispatch like before) but hidden from the empty-query browse
/// list so consolidated flows appear exactly once.
pub const BROWSE_HIDDEN: &[&str] = &["sci.search", "sci.ingest"];

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
        .filter(|c| !q.is_empty() || !BROWSE_HIDDEN.contains(&c.id))
        .filter_map(|c| fuzzy_match(c, q).map(|s| (s, c.suggested, c)))
        .collect();

    if q.is_empty() {
        // Browse order: Science first (that's what the user opened PRISM
        // for), then suggested chrome, then everything else grouped by
        // category so related entries sit together instead of the old
        // alphabetical jumble.
        scored.sort_by(|a, b| {
            let sci = |c: &Command| c.category != "Science";
            sci(a.2)
                .cmp(&sci(b.2))
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.category.cmp(b.2.category))
                .then_with(|| a.2.title.cmp(b.2.title))
        });
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
            CATALOG.len() - BROWSE_HIDDEN.len(),
            "empty query must return all commands except browse-hidden aliases"
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
    fn browse_hidden_aliases_collapse_but_still_match_queries() {
        let browse_ids: Vec<&str> = fuzzy_sorted("").iter().map(|c| c.id).collect();
        for hidden in BROWSE_HIDDEN {
            assert!(
                !browse_ids.contains(hidden),
                "{hidden} must be collapsed out of the browse list"
            );
        }
        assert!(
            browse_ids.contains(&"knowledge.open"),
            "the consolidated Knowledge entry must appear in browse"
        );

        // Muscle memory: typing the old titles still finds the aliases.
        let ids: Vec<&str> = fuzzy_sorted("search literature")
            .iter()
            .map(|c| c.id)
            .collect();
        assert!(ids.contains(&"sci.search"), "got: {ids:?}");
        let ids: Vec<&str> = fuzzy_sorted("ingest").iter().map(|c| c.id).collect();
        assert!(ids.contains(&"sci.ingest"), "got: {ids:?}");
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
