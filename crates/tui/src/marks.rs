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
        RefKind::Provenance => "source",
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

    /// Drop a mark by id. Used when the reader clicks a strip row, and when
    /// the object behind a mark disappears from the session.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|m| m.id != id);
        before != self.items.len()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Drop marks whose handle no longer exists, returning what was dropped.
    /// A structure that has left the cache still rode every message before
    /// this — a handle pointing at nothing, presented to the model as a thing
    /// the reader is working on.
    pub fn prune(&mut self, live: impl Fn(&Mark) -> bool) -> Vec<Mark> {
        let (kept, dropped): (Vec<Mark>, Vec<Mark>) =
            std::mem::take(&mut self.items).into_iter().partition(&live);
        self.items = kept;
        dropped
    }

    /// The marked set as it goes on the wire: kind, identity, label. Sent as
    /// its own field on every message, so the agent can hold it as a
    /// replaceable slot instead of a growing history of prefixes.
    pub fn wire(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.items
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "kind": kind_word(m.kind),
                        "id": m.id,
                        "label": m.label,
                    })
                })
                .collect(),
        )
    }
}

/// A label as it is safe to show and to send: one line, no control bytes.
///
/// `sanitize_for_render` strips escapes but keeps newlines, which is right
/// for prose and wrong for a label. A label is ONE line of screen text and
/// one field on the wire, and a newline inside it is exactly what let a
/// hostile object label forge a second block inside the user's own message.
pub fn sanitize_label(raw: &str) -> String {
    crate::sanitize::sanitize_for_render(raw)
        .replace(['\n', '\r'], " ")
        .trim()
        .to_string()
}

/// Whether an id is something the agent can actually act on.
///
/// A mark is a handle the model resolves with the tools it already has: a
/// cache key, a DOI, a file it can open, a tool it can call. An object id
/// like `sim-42` is none of those — marking it put a line in the model's
/// context that resolves to nothing, under a kind word (`file`) that the
/// panel itself contradicted.
pub fn actionable_identity(id: &str, kind: RefKind) -> Result<(), String> {
    let ok = match kind {
        RefKind::Structure => id.starts_with("cache://"),
        // A DOI is `10.<registrant>/<suffix>`, however it is prefixed.
        RefKind::Doi => {
            let bare = id
                .trim_start_matches("doi:")
                .trim_start_matches("https://doi.org/");
            bare.starts_with("10.") && bare.contains('/')
        }
        RefKind::FileLine => id.starts_with("file://"),
        RefKind::Tool => id.starts_with("tool://"),
        RefKind::Provenance => id.starts_with("provenance://"),
    };
    if ok {
        return Ok(());
    }
    Err(match kind {
        RefKind::Structure => {
            format!("{id} is not a cached structure, so there is nothing to open")
        }
        RefKind::Doi => format!("{id} is not a DOI, so the paper cannot be resolved"),
        RefKind::FileLine => format!("{id} is not a file path the agent can read"),
        RefKind::Tool => format!("{id} is not a tool the agent can call"),
        RefKind::Provenance => format!("{id} is not a source a tool result named"),
    })
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
    fn the_wire_form_carries_kind_identity_and_label() {
        let mut marks = Marks::default();
        assert_eq!(
            marks.wire(),
            serde_json::json!([]),
            "nothing marked, nothing sent"
        );
        marks.toggle(structure("cache://abc/structure.cif", "TiAl"));
        marks.toggle(Mark {
            id: "doi:10.1038/ncomms10602".to_string(),
            kind: RefKind::Doi,
            label: "Fracture toughness of CrCoNi".to_string(),
        });
        assert_eq!(
            marks.wire(),
            serde_json::json!([
                {"kind": "structure", "id": "cache://abc/structure.cif", "label": "TiAl"},
                {"kind": "paper", "id": "doi:10.1038/ncomms10602",
                 "label": "Fracture toughness of CrCoNi"},
            ])
        );
    }

    /// A mark is a handle the model can resolve. An object id that resolves
    /// to nothing was marked anyway, as a `file` the panel itself denied.
    #[test]
    fn only_identities_the_agent_can_resolve_may_be_marked() {
        assert!(actionable_identity("cache://abc/structure.cif", RefKind::Structure).is_ok());
        assert!(actionable_identity("10.1038/ncomms10602", RefKind::Doi).is_ok());
        assert!(actionable_identity("doi:10.1038/x", RefKind::Doi).is_ok());
        assert!(actionable_identity("file:///tmp/a.rs", RefKind::FileLine).is_ok());
        assert!(actionable_identity("tool://web", RefKind::Tool).is_ok());
        // The shapes that used to be marked and could not be acted on.
        let why = actionable_identity("sim-42", RefKind::FileLine).unwrap_err();
        assert!(why.contains("sim-42") && why.contains("file path"), "{why}");
        assert!(actionable_identity("job-7", RefKind::Structure).is_err());
        assert!(actionable_identity("Ti-6Al-4V", RefKind::Doi).is_err());
    }

    #[test]
    fn a_mark_can_be_taken_back_and_a_dead_handle_is_pruned() {
        let mut marks = Marks::default();
        marks.toggle(structure("cache://a", "A"));
        marks.toggle(structure("cache://b", "B"));
        assert!(marks.remove("cache://a"));
        assert!(
            !marks.remove("cache://a"),
            "removing twice is not an error, just false"
        );
        marks.toggle(structure("cache://c", "C"));
        let dropped = marks.prune(|m| m.id == "cache://b");
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].id, "cache://c");
        assert_eq!(marks.len(), 1);
        marks.clear();
        assert!(marks.is_empty());
    }
}
