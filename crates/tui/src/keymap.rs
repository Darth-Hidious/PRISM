//! Keymap registry — the which-key primitive (modeled on opencode).
//!
//! opencode's which-key panel renders its full keymap (`config/keybind.ts`)
//! grouped by category, scrollable, as a discoverable reference. PRISM ports
//! that: every TUI keybinding lives in [`KEYMAP`] as data, and the which-key
//! panel (`?`) renders it grouped. This makes the keyset self-documenting and
//! ends the drift between the Help modal's hand-written rows and reality.
//!
//! In a later patch the Help modal will render from this same registry so
//! there is a single source of truth. For now the two intentionally overlap
//! (display strings only, low drift risk).

/// A single keybinding row for the which-key panel.
#[derive(Debug, Clone)]
pub struct KeyBinding {
    /// The key(s), e.g. `"Ctrl-P"` or `"↑↓ / k j"`.
    pub keys: &'static str,
    /// What it does.
    pub description: &'static str,
    /// Grouping shown as a section header.
    pub category: &'static str,
}

/// The full TUI keyset, grouped by category in display order.
///
/// Add a row here whenever a new key is wired in `App::handle_key` so the
/// panel stays accurate.
pub static KEYMAP: &[KeyBinding] = &[
    // ── Commands & palette ──────────────────────────────────────────
    KeyBinding {
        keys: "Ctrl-P",
        description: "Command palette — run any command",
        category: "Commands & palette",
    },
    KeyBinding {
        keys: "?",
        description: "This keybindings panel",
        category: "Commands & palette",
    },
    KeyBinding {
        keys: "Ctrl-L",
        description: "Clear the chat transcript",
        category: "Commands & palette",
    },
    KeyBinding {
        keys: "Ctrl-C",
        description: "Quit PRISM",
        category: "Commands & palette",
    },
    KeyBinding {
        keys: "/<command>",
        description: "Slash commands: /help /cost /model /mcp /goal",
        category: "Commands & palette",
    },
    // ── Navigation ──────────────────────────────────────────────────
    KeyBinding {
        keys: "Tab",
        description: "Cycle focus: input → workspace → chat",
        category: "Navigation",
    },
    KeyBinding {
        keys: "i",
        description: "Focus the message input",
        category: "Navigation",
    },
    KeyBinding {
        keys: "Esc",
        description: "Leave a panel / cancel",
        category: "Navigation",
    },
    KeyBinding {
        keys: "PgUp / PgDn",
        description: "Scroll transcript by a page",
        category: "Navigation",
    },
    KeyBinding {
        keys: "↑↓ / k j",
        description: "Scroll transcript one line",
        category: "Navigation",
    },
    KeyBinding {
        keys: "g / G",
        description: "Jump to top / bottom",
        category: "Navigation",
    },
    KeyBinding {
        keys: "o",
        description: "Open a link from the transcript (chat focus)",
        category: "Navigation",
    },
    KeyBinding {
        keys: "mouse wheel",
        description: "Scroll transcript",
        category: "Navigation",
    },
    // ── Display ─────────────────────────────────────────────────────
    KeyBinding {
        keys: "Ctrl-T",
        description: "Toggle thinking tokens",
        category: "Display",
    },
    KeyBinding {
        keys: "Ctrl-M",
        description: "Toggle tok/s metrics",
        category: "Display",
    },
    KeyBinding {
        keys: "Ctrl-$",
        description: "Toggle cost in the status bar",
        category: "Display",
    },
    KeyBinding {
        keys: "Ctrl-P → theme",
        description: "Switch color theme (via the command palette)",
        category: "Display",
    },
    // ── Workspace sidebar ───────────────────────────────────────────
    KeyBinding {
        keys: "← / →",
        description: "Switch Activity / Tools / Files",
        category: "Workspace sidebar",
    },
    KeyBinding {
        keys: "↑ / ↓",
        description: "Move selection",
        category: "Workspace sidebar",
    },
    KeyBinding {
        keys: "Enter",
        description: "Open details for the selected item (tool / file / event)",
        category: "Workspace sidebar",
    },
    KeyBinding {
        keys: "Space",
        description: "Expand the selected item inline",
        category: "Workspace sidebar",
    },
    // ── Approvals ───────────────────────────────────────────────────
    KeyBinding {
        keys: "y",
        description: "Approve the pending tool",
        category: "Approvals",
    },
    KeyBinding {
        keys: "a",
        description: "Allow all tools for this session",
        category: "Approvals",
    },
    KeyBinding {
        keys: "n",
        description: "Deny the pending tool",
        category: "Approvals",
    },
    // ── Input editing ───────────────────────────────────────────────
    KeyBinding {
        keys: "Enter",
        description: "Send the message",
        category: "Input editing",
    },
    KeyBinding {
        keys: "Ctrl-A / Ctrl-E",
        description: "Move to start / end of line",
        category: "Input editing",
    },
    KeyBinding {
        keys: "Ctrl-U / Ctrl-K",
        description: "Delete to start / end of line",
        category: "Input editing",
    },
    KeyBinding {
        keys: "Ctrl-W",
        description: "Delete the previous word",
        category: "Input editing",
    },
    KeyBinding {
        keys: "Ctrl-D",
        description: "Delete the next character",
        category: "Input editing",
    },
    KeyBinding {
        keys: "← / →",
        description: "Move the cursor",
        category: "Input editing",
    },
];

/// Categories in the order they first appear in [`KEYMAP`].
pub fn categories() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for b in KEYMAP {
        if !out.contains(&b.category) {
            out.push(b.category);
        }
    }
    out
}

/// All bindings belonging to a category, in [`KEYMAP`] order.
pub fn bindings_in(category: &str) -> impl Iterator<Item = &'static KeyBinding> {
    KEYMAP.iter().filter(move |b| b.category == category)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn categories_preserve_keymap_order() {
        let cats = categories();
        // Every category is the first occurrence order from KEYMAP.
        let mut seen = HashSet::new();
        let mut ordered: Vec<&str> = Vec::new();
        for b in KEYMAP {
            if seen.insert(b.category) {
                ordered.push(b.category);
            }
        }
        assert_eq!(cats, ordered);
    }

    #[test]
    fn every_binding_has_a_known_category() {
        let cats: HashSet<&str> = categories().into_iter().collect();
        for b in KEYMAP {
            assert!(
                cats.contains(b.category),
                "binding {:?} has unknown category",
                b.keys
            );
        }
    }

    #[test]
    fn bindings_in_returns_only_that_category() {
        for cat in categories() {
            for b in bindings_in(cat) {
                assert_eq!(b.category, cat);
            }
        }
    }

    #[test]
    fn keymap_covers_core_keys() {
        let keys: HashSet<&str> = KEYMAP.iter().map(|b| b.keys).collect();
        for must in ["Ctrl-P", "?", "Ctrl-C", "Tab", "y", "a", "n", "Enter"] {
            assert!(keys.contains(must), "keymap missing core key {must:?}");
        }
    }
}
