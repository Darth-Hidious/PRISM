// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! What the reader has marked in the interface for the agent to work with.
//!
//! A mark is shared state between a person and the model, and shared state
//! has to be a SLOT, not a stream. Prefixed onto the user's message it became
//! durable history: N turns left N snapshots in the transcript, unmarking
//! never withdrew the earlier ones, and the model was left to guess which
//! list was current — the failure mode of every "context injection" that
//! forgets it is writing to a log.
//!
//! So this is one replaceable value. The interface sets it whenever it
//! changes; the turn loop rebuilds its block from whatever is set NOW and
//! puts it in the preamble, never in the durable message history. An object
//! that has been unmarked is simply absent from the next turn's block.

use std::sync::RwLock;

/// One handle the reader marked: an identity the model's tools can resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkedHandle {
    /// `structure`, `paper`, `file`, `tool` — the word the interface shows.
    pub kind: String,
    /// What the tools resolve: a `cache://` key, a DOI, a path.
    pub id: String,
    /// The words the reader saw on screen.
    pub label: String,
}

static MARKED: RwLock<Vec<MarkedHandle>> = RwLock::new(Vec::new());

/// Serializes tests that drive the slot. It is one process-wide value by
/// design — the interface has one marked set — so parallel tests would
/// otherwise read each other's marks.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Replace the marked set. The interface calls this with the CURRENT marks
/// every time it sends a message, so the slot never accumulates.
pub fn set_marked(handles: Vec<MarkedHandle>) {
    if let Ok(mut slot) = MARKED.write() {
        *slot = handles;
    }
}

/// What is marked right now.
pub fn marked() -> Vec<MarkedHandle> {
    MARKED.read().map(|slot| slot.clone()).unwrap_or_default()
}

/// Parse the wire form the interface sends (`[{kind,id,label}, …]`). Entries
/// without an id are dropped: a mark with no identity is a word, and a word
/// in the model's context that resolves to nothing is worse than no mark.
pub fn from_json(value: &serde_json::Value) -> Vec<MarkedHandle> {
    let Some(array) = value.as_array() else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            Some(MarkedHandle {
                kind: entry
                    .get("kind")
                    .and_then(|k| k.as_str())
                    .unwrap_or("object")
                    .to_string(),
                id: id.to_string(),
                label: entry
                    .get("label")
                    .and_then(|l| l.as_str())
                    .unwrap_or(id)
                    .to_string(),
            })
        })
        .collect()
}

/// The preamble block for this turn, or nothing when nothing is marked.
///
/// Says what the block IS, because the model has no other way to know: these
/// are the user's own pointings, the ids are resolvable, and the list is
/// complete as of this turn.
pub fn marked_block() -> Option<String> {
    let handles = marked();
    if handles.is_empty() {
        return None;
    }
    let mut block = String::from(
        "MARKED BY THE USER\n\
         The user pointed at these objects in the interface and asked you to work \
         with them. Each `id` is resolvable with the tools you already have (a \
         cache:// key names a cached structure, a DOI names a paper). This list is \
         rebuilt every turn from what is marked right now — anything the user has \
         unmarked is simply absent, so treat it as the complete current set.\n",
    );
    for handle in handles {
        block.push_str(&format!(
            "- {} id={} label={}\n",
            handle.kind, handle.id, handle.label
        ));
    }
    Some(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(kind: &str, id: &str, label: &str) -> MarkedHandle {
        MarkedHandle {
            kind: kind.to_string(),
            id: id.to_string(),
            label: label.to_string(),
        }
    }

    /// The whole point of a slot: what the model sees is what is marked NOW.
    /// Mark A, take two turns, unmark A and mark B — the third turn must say
    /// B and must not still be carrying A.
    #[test]
    fn the_block_is_replaced_each_turn_never_accumulated() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_marked(vec![handle("structure", "cache://a", "TiAl")]);
        let first = marked_block().expect("A is marked");
        assert!(first.contains("cache://a"), "{first}");
        // A second turn with the same marks says the same thing once.
        let second = marked_block().expect("still marked");
        assert_eq!(first, second);
        assert_eq!(second.matches("cache://a").count(), 1, "{second}");
        // The reader unmarks A and marks B.
        set_marked(vec![handle("paper", "10.1038/ncomms10602", "CrCoNi")]);
        let third = marked_block().expect("B is marked");
        assert!(
            !third.contains("cache://a"),
            "an unmarked object must be gone, not retracted: {third}"
        );
        assert!(third.contains("10.1038/ncomms10602"), "{third}");
        // And an empty set is no block at all, not an empty heading.
        set_marked(Vec::new());
        assert_eq!(marked_block(), None);
    }

    #[test]
    fn the_block_says_what_it_is_and_that_the_ids_resolve() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_marked(vec![handle("structure", "cache://abc", "TiAl")]);
        let block = marked_block().unwrap();
        assert!(block.starts_with("MARKED BY THE USER"), "{block}");
        assert!(block.contains("rebuilt every turn"), "{block}");
        assert!(
            block.contains("- structure id=cache://abc label=TiAl"),
            "{block}"
        );
        set_marked(Vec::new());
    }

    #[test]
    fn wire_entries_without_an_identity_are_dropped() {
        let parsed = from_json(&serde_json::json!([
            {"kind": "structure", "id": "cache://a", "label": "TiAl"},
            {"kind": "object", "id": "", "label": "nothing"},
            {"kind": "object", "label": "no id at all"},
        ]));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "cache://a");
    }
}
