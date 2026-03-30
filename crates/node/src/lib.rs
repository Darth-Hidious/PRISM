//! PRISM node runtime — the `prism-node` daemon binary.
//!
//! Turns any machine into a MARC27 compute node. Capabilities:
//!
//! - **Hardware probing** ([`detect`]): CPU, RAM, GPU, disk, container runtimes, services.
//! - **Job execution** ([`executor`]): Docker/Podman container lifecycle with timeouts and output capture.
//! - **E2EE** ([`crypto`]): X25519 key agreement + ChaCha20-Poly1305 AEAD + Ed25519 signing.
//! - **Platform registration** ([`daemon`]): WebSocket heartbeat, job dispatch, and reconnect.
//! - **Crash-safe state** ([`state`]): Atomic file writes for active job tracking and shutdown coordination.

pub mod crypto;
pub mod daemon;
pub mod detect;
pub mod executor;
pub mod state;
