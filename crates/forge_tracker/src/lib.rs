// PRISM fork: PostHog telemetry stripped. Public API preserved so existing
// forge_main call sites keep compiling. Every dispatch path is a no-op.
// Logging still goes to a rolling daily file under the forge log path.

mod dispatch;
mod event;
mod log;

pub use dispatch::Tracker;
pub use event::{Event, EventKind, Identity, ToolCallPayload};
pub use log::{Guard, init_tracing};

pub const VERSION: &str = match option_env!("APP_VERSION") {
    Some(val) => val,
    None => env!("CARGO_PKG_VERSION"),
};
