//! Toast notifications — transient, non-blocking feedback.
//!
//! Modeled on opencode's `ui/toast.tsx`. Unlike system chat lines, toasts
//! float over the interface and auto-dismiss after a TTL, so confirmations
//! ("theme applied", "thinking hidden", "goal set") don't clutter the
//! transcript. Errors live a little longer so they're actually readable.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Ok,
    Warn,
    Err,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub message: String,
    pub kind: ToastKind,
    pub created_at: Instant,
    pub ttl: Duration,
}

impl Toast {
    pub const DEFAULT_TTL: Duration = Duration::from_secs(4);
    pub const ERROR_TTL: Duration = Duration::from_secs(6);

    pub fn new(message: impl Into<String>, kind: ToastKind) -> Self {
        let ttl = if matches!(kind, ToastKind::Err) {
            Self::ERROR_TTL
        } else {
            Self::DEFAULT_TTL
        };
        Self {
            message: message.into(),
            kind,
            created_at: Instant::now(),
            ttl,
        }
    }

    /// True once the TTL has elapsed.
    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() >= self.ttl
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_toasts_outlive_info_toasts() {
        assert!(Toast::new("x", ToastKind::Err).ttl > Toast::new("x", ToastKind::Ok).ttl);
        assert_eq!(Toast::new("x", ToastKind::Info).ttl, Toast::DEFAULT_TTL);
    }

    #[test]
    fn fresh_toast_is_not_expired() {
        assert!(!Toast::new("hi", ToastKind::Info).is_expired());
    }

    #[test]
    fn zero_ttl_toast_is_expired() {
        let mut t = Toast::new("bye", ToastKind::Warn);
        t.ttl = Duration::ZERO;
        assert!(t.is_expired());
    }
}
