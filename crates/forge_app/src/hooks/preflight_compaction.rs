use async_trait::async_trait;
use forge_domain::{Agent, Conversation, Environment, EventData, EventHandle, RequestPayload};
use tracing::{debug, info};

use crate::compact::Compactor;

/// Hook handler that runs context compaction BEFORE each LLM request.
///
/// The sibling [`CompactionHandler`](super::CompactionHandler) runs on
/// `Response` events — i.e. *after* a turn comes back. That's the wrong
/// timing for the failure mode this handler exists to prevent: a turn
/// whose outgoing context already exceeds the model window. By the
/// time the response hook fires, the request has already been sent and
/// either succeeded (no compaction needed yet) or 4xx'd with
/// `context_length_exceeded` (compaction is now too late).
///
/// This handler runs on the `Request` event in `orch.rs::run`'s loop,
/// just before `execute_chat_turn`. If `agent.compact.should_compact`
/// returns true for the *current* context, it compacts in place. The
/// orchestrator re-reads `conversation.context` after the hook chain
/// completes so the compacted version is what hits the wire.
///
/// Logic is otherwise identical to `CompactionHandler` — same compactor,
/// same agent config. They differ only in WHEN they run.
#[derive(Clone)]
pub struct PreflightCompactionHandler {
    agent: Agent,
    environment: Environment,
}

impl PreflightCompactionHandler {
    pub fn new(agent: Agent, environment: Environment) -> Self {
        Self { agent, environment }
    }
}

#[async_trait]
impl EventHandle<EventData<RequestPayload>> for PreflightCompactionHandler {
    async fn handle(
        &self,
        _event: &EventData<RequestPayload>,
        conversation: &mut Conversation,
    ) -> anyhow::Result<()> {
        if let Some(context) = &conversation.context {
            let token_count = context.token_count();
            if self.agent.compact.should_compact(context, *token_count) {
                info!(
                    agent_id = %self.agent.id,
                    token_count = *token_count,
                    "Pre-flight compaction triggered before request"
                );
                let compacted =
                    Compactor::new(self.agent.compact.clone(), self.environment.clone())
                        .compact(context.clone(), false)?;
                conversation.context = Some(compacted);
            } else {
                debug!(agent_id = %self.agent.id, "Pre-flight compaction not needed");
            }
        }
        Ok(())
    }
}
