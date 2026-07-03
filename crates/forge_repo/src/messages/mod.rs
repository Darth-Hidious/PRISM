mod message_record;
mod outbox_drain;
mod sync_outbox_record;

pub(crate) use message_record::{MessageRecord, MessageRole};
pub(crate) use sync_outbox_record::NewSyncOutboxRecord;
#[allow(unused_imports)]
pub(crate) use sync_outbox_record::SyncOutboxRecord;

// Re-exported pub so the chat backend / CLI can construct a drain
// context and call `drain_once` per turn (or on demand). Will be
// consumed by external crates in the chat-loop wiring PR; until then
// the names are unused in-crate but still part of the public API.
#[allow(unused_imports)]
pub use outbox_drain::{DrainSummary, MAX_OUTBOX_ATTEMPTS, OutboxDrain};
