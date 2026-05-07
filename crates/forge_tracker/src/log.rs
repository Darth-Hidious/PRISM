// File-based tracing. PostHog writer removed.

use std::path::PathBuf;

use tracing::debug;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{self, EnvFilter, Layer, filter};

use crate::Tracker;

pub fn init_tracing(log_path: PathBuf, _tracker: Tracker) -> anyhow::Result<Guard> {
    debug!(path = %log_path.display(), "Initializing logging system in JSON format");

    let appender = tracing_appender::rolling::daily(log_path, "forge.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);

    let filter = filter::filter_fn(|metadata| metadata.target().starts_with("forge_"));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .with_thread_ids(false)
        .with_target(false)
        .with_file(true)
        .with_line_number(true)
        .with_writer(non_blocking)
        .with_filter(filter);

    // Use try_init so we don't panic when a global subscriber is already
    // installed (e.g. when forge_tracker is called from inside a host CLI
    // like prism that sets up its own tracing-subscriber first).
    let _ = tracing_subscriber::registry()
        .with(EnvFilter::try_from_env("FORGE_LOG").unwrap_or_else(|_| EnvFilter::new("forge=debug")))
        .with(fmt_layer)
        .try_init();

    Ok(Guard(guard))
}

pub struct Guard(#[allow(dead_code)] WorkerGuard);
