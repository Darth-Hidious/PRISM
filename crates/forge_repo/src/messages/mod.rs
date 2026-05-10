mod message_record;
mod sync_outbox_record;

pub(crate) use message_record::{MessageRecord, MessageRole};
pub(crate) use sync_outbox_record::NewSyncOutboxRecord;
// SyncOutboxRecord is the read shape — wired up in the drain-worker PR
// that follows. Not re-exported yet.
#[allow(unused_imports)]
pub(crate) use sync_outbox_record::SyncOutboxRecord;
