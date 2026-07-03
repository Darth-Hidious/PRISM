// PRISM fork: no-op Tracker. PostHog dispatcher removed.

use forge_domain::Conversation;

use crate::event::EventKind;

#[derive(Clone, Default, Debug)]
pub struct Tracker;

impl Tracker {
    pub async fn dispatch(&self, _event_kind: EventKind) -> anyhow::Result<()> {
        Ok(())
    }

    pub async fn set_model<S: Into<String>>(&'static self, _model: S) {}

    pub async fn login<S: Into<String>>(&'static self, _login: S) {}

    pub async fn set_conversation(&self, _conversation: Conversation) {}
}
