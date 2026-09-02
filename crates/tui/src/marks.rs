// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! What the reader has marked for the agent.
//!
//! A reference the reader can open is a handle; marking it is saying "this
//! one — work with it". Marks are shared state between the person and the
//! agent: they are shown in the workspace, and every message the reader
//! sends carries them, so the agent's next turn starts from the same objects
//! the reader is looking at. Nothing is narrated; the handle itself travels.
//!
//! A mark holds an identity the agent can act on — a `cache://` key, a DOI,
//! a file, a tool name — never a payload. What it points at is fetched by
//! whoever needs it, the same rule the reference registry follows.

use crate::refs::RefKind;

/// One marked handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    /// The identity the reference resolves — what the agent receives.
    pub id: String,
    pub kind: RefKind,
    /// The words the reader saw; shown beside the id so the strip reads.
    pub label: String,
}

/// The reader's marks, in the order they were made.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Marks {
    items: Vec<Mark>,
}

/// The word a mark's kind goes by on screen and on the wire.
pub fn kind_word(kind: RefKind) -> &'static str {
    match kind {
        RefKind::Structure => "structure",
        RefKind::Doi => "paper",
        RefKind::FileLine => "file",
        RefKind::Tool => "tool",
    }
}

impl Marks {
    /// Mark or unmark: returns `true` when the handle is now marked.
    pub fn toggle(&mut self, mark: Mark) -> bool {
        if let Some(pos) = self.items.iter().position(|m| m.id == mark.id) {
            self.items.remove(pos);
            false
        } else {
            self.items.push(mark);
            true
        }
    }

    pub fn is_marked(&self, id: &str) -> bool {
        self.items.iter().any(|m| m.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Mark> {
        self.items.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The block prefixed to the reader's next message, or nothing when
    /// nothing is marked. One line per handle: kind, identity, the label the
    /// reader saw. The identity is what the agent's tools resolve.
    pub fn context_block(&self) -> Option<String> {
        if self.items.is_empty() {
            return None;
        }
        let mut block = String::from("[Marked for you]\n");
        for m in &self.items {
            block.push_str(&format!("- {} {} ({})\n", kind_word(m.kind), m.id, m.label));
        }
        Some(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn structure(id: &str, label: &str) -> Mark {
        Mark {
            id: id.to_string(),
            kind: RefKind::Structure,
            label: label.to_string(),
        }
    }

    #[test]
    fn a_second_toggle_unmarks_and_order_is_the_order_of_marking() {
        let mut marks = Marks::default();
        assert!(marks.toggle(structure("cache://a", "A")));
        assert!(marks.toggle(Mark {
            id: "doi:10.1/x".to_string(),
            kind: RefKind::Doi,
            label: "Paper X".to_string(),
        }));
        assert!(marks.is_marked("cache://a"));
        assert_eq!(marks.len(), 2);
        assert!(
            !marks.toggle(structure("cache://a", "A")),
            "the second click unmarks"
        );
        assert!(!marks.is_marked("cache://a"));
        let ids: Vec<&str> = marks.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["doi:10.1/x"]);
    }

    #[test]
    fn the_context_block_carries_kind_identity_and_label_or_nothing() {
        let mut marks = Marks::default();
        assert_eq!(marks.context_block(), None, "nothing marked, nothing sent");
        marks.toggle(structure("cache://abc/structure.cif", "TiAl"));
        marks.toggle(Mark {
            id: "doi:10.1038/ncomms10602".to_string(),
            kind: RefKind::Doi,
            label: "Fracture toughness of CrCoNi".to_string(),
        });
        let block = marks.context_block().unwrap();
        assert_eq!(
            block,
            "[Marked for you]\n\
             - structure cache://abc/structure.cif (TiAl)\n\
             - paper doi:10.1038/ncomms10602 (Fracture toughness of CrCoNi)\n"
        );
    }
}
