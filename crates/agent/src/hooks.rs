//! Pre/post tool execution hooks — extensibility without modifying the core loop.
//!
//! - Hooks fire before and after every tool call
//! - Pre-hooks can block execution (abort = true)
//! - Post-hooks are observational (never block)
//! - Hooks are registered per-session, immutable after build

use std::collections::HashSet;

use serde_json::Value;
use tracing::{info, warn};

// ── Result types ────────────────────────────────────────────────────

/// Result of a pre-hook execution.
#[derive(Debug, Clone)]
pub struct HookResult {
    pub abort: bool,
    pub reason: String,
    pub modified_inputs: Option<Value>,
}

impl Default for HookResult {
    fn default() -> Self {
        Self {
            abort: false,
            reason: String::new(),
            modified_inputs: None,
        }
    }
}

/// Result of a post-hook execution.
#[derive(Debug, Clone)]
pub struct PostHookResult {
    pub modified_result: Option<Value>,
    pub log_message: String,
}

impl Default for PostHookResult {
    fn default() -> Self {
        Self {
            modified_result: None,
            log_message: String::new(),
        }
    }
}

// ── Hook and callback types ─────────────────────────────────────────

/// Pre-tool callback: `(tool_name, inputs) -> HookResult`.
pub type BeforeFn = Box<dyn Fn(&str, &Value) -> HookResult + Send + Sync>;

/// Post-tool callback: `(tool_name, inputs, result, elapsed_ms) -> PostHookResult`.
pub type AfterFn = Box<dyn Fn(&str, &Value, &Value, f64) -> PostHookResult + Send + Sync>;

/// A named hook with optional before/after callbacks.
pub struct Hook {
    pub name: String,
    pub before: Option<BeforeFn>,
    pub after: Option<AfterFn>,
    pub tool_filter: Option<HashSet<String>>,
}

impl Hook {
    /// Returns true if this hook should fire for the given tool.
    /// `None` filter matches all tools.
    pub fn matches(&self, tool_name: &str) -> bool {
        match &self.tool_filter {
            None => true,
            Some(set) => set.contains(tool_name),
        }
    }
}

// ── Registry ────────────────────────────────────────────────────────

/// Ordered registry of pre/post tool hooks.
///
/// Hooks fire in registration order. Pre-hooks can abort execution.
/// Post-hooks are observational (fire-and-forget).
pub struct HookRegistry {
    hooks: Vec<Hook>,
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HookRegistry {
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Append a hook to the registry.
    pub fn register(&mut self, hook: Hook) {
        info!("Registered hook: {}", hook.name);
        self.hooks.push(hook);
    }

    /// Fire all matching pre-hooks. First abort wins.
    /// Panics in individual hooks are caught and logged.
    pub fn fire_before(&self, tool_name: &str, inputs: &Value) -> HookResult {
        for hook in &self.hooks {
            if let Some(ref before) = hook.before {
                if hook.matches(tool_name) {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        before(tool_name, inputs)
                    }));
                    match result {
                        Ok(hr) => {
                            if hr.abort {
                                info!(
                                    "Hook '{}' aborted {}: {}",
                                    hook.name, tool_name, hr.reason
                                );
                                return hr;
                            }
                        }
                        Err(_) => {
                            warn!("Pre-hook '{}' panicked", hook.name);
                        }
                    }
                }
            }
        }
        HookResult::default()
    }

    /// Fire all matching post-hooks. Never aborts. Panics are caught.
    /// Returns the (potentially modified) result.
    pub fn fire_after(
        &self,
        tool_name: &str,
        inputs: &Value,
        result: &Value,
        elapsed_ms: f64,
    ) -> Value {
        let mut current = result.clone();
        for hook in &self.hooks {
            if let Some(ref after) = hook.after {
                if hook.matches(tool_name) {
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        after(tool_name, inputs, &current, elapsed_ms)
                    }));
                    match outcome {
                        Ok(post_result) => {
                            if !post_result.log_message.is_empty() {
                                info!(
                                    "Post-hook '{}': {}",
                                    hook.name, post_result.log_message
                                );
                            }
                            if let Some(modified) = post_result.modified_result {
                                current = modified;
                            }
                        }
                        Err(_) => {
                            warn!("Post-hook '{}' panicked", hook.name);
                        }
                    }
                }
            }
        }
        current
    }
}

// ── Built-in hooks ──────────────────────────────────────────────────

/// Pre-hook on ALL tools. Scans arg values for destructive keywords.
/// If found, aborts with a reason string.
pub fn safety_hook() -> Hook {
    let destructive: HashSet<&str> =
        ["delete", "drop", "remove", "destroy", "truncate", "reset"]
            .into_iter()
            .collect();

    Hook {
        name: "safety_guard".into(),
        before: Some(Box::new(move |tool_name, inputs| {
            if let Value::Object(map) = inputs {
                for (key, val) in map {
                    if let Value::String(s) = val {
                        let lowered = s.to_lowercase();
                        for &pattern in &destructive {
                            if lowered.contains(pattern) {
                                return HookResult {
                                    abort: true,
                                    reason: format!(
                                        "Blocked: '{}' detected in {}.{}. \
                                         Use --dangerously-accept-all to override.",
                                        pattern, tool_name, key
                                    ),
                                    modified_inputs: None,
                                };
                            }
                        }
                    }
                }
            }
            HookResult::default()
        })),
        after: None,
        tool_filter: None,
    }
}

/// Post-hook on ALL tools. Logs `"{tool_name}: {elapsed_ms}ms"`.
pub fn cost_hook() -> Hook {
    Hook {
        name: "cost_tracker".into(),
        before: None,
        after: Some(Box::new(
            |tool_name, _inputs, _result, elapsed_ms| PostHookResult {
                modified_result: None,
                log_message: format!("{}: {:.0}ms", tool_name, elapsed_ms),
            },
        )),
        tool_filter: None,
    }
}

/// Post-hook on ALL tools. Logs `"[AUDIT] {tool_name} {OK|ERROR} ({elapsed_ms}ms)"`.
pub fn audit_hook() -> Hook {
    Hook {
        name: "audit_log".into(),
        before: None,
        after: Some(Box::new(
            |tool_name, _inputs, result, elapsed_ms| {
                let status = if result.get("error").is_some() {
                    "ERROR"
                } else {
                    "OK"
                };
                PostHookResult {
                    modified_result: None,
                    log_message: format!(
                        "[AUDIT] {} {} ({:.0}ms)",
                        tool_name, status, elapsed_ms
                    ),
                }
            },
        )),
        tool_filter: None,
    }
}

/// Build the default hook registry with safety + cost + audit hooks.
pub fn build_default_hooks() -> HookRegistry {
    let mut registry = HookRegistry::new();
    registry.register(safety_hook());
    registry.register(cost_hook());
    registry.register(audit_hook());
    registry
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn safety_hook_blocks_destructive_keywords() {
        let registry = build_default_hooks();
        let inputs = json!({"query": "DROP TABLE users"});
        let result = registry.fire_before("sql_exec", &inputs);
        assert!(result.abort);
        assert!(result.reason.contains("drop"));
    }

    #[test]
    fn safety_hook_allows_safe_inputs() {
        let registry = build_default_hooks();
        let inputs = json!({"query": "SELECT * FROM users"});
        let result = registry.fire_before("sql_exec", &inputs);
        assert!(!result.abort);
    }

    #[test]
    fn cost_hook_logs_timing() {
        let hook = cost_hook();
        let after = hook.after.as_ref().unwrap();
        let result = after("my_tool", &json!({}), &json!({}), 42.0);
        assert_eq!(result.log_message, "my_tool: 42ms");
        assert!(result.modified_result.is_none());
    }

    #[test]
    fn audit_hook_logs_ok_status() {
        let hook = audit_hook();
        let after = hook.after.as_ref().unwrap();
        let result = after("search", &json!({}), &json!({"data": 1}), 100.0);
        assert!(result.log_message.contains("[AUDIT] search OK (100ms)"));
    }

    #[test]
    fn audit_hook_logs_error_status() {
        let hook = audit_hook();
        let after = hook.after.as_ref().unwrap();
        let result = after("search", &json!({}), &json!({"error": "fail"}), 50.0);
        assert!(result.log_message.contains("[AUDIT] search ERROR (50ms)"));
    }

    #[test]
    fn hook_filter_restricts_matching() {
        let mut filter = HashSet::new();
        filter.insert("allowed_tool".into());
        let hook = Hook {
            name: "filtered".into(),
            before: None,
            after: None,
            tool_filter: Some(filter),
        };
        assert!(hook.matches("allowed_tool"));
        assert!(!hook.matches("other_tool"));
    }

    #[test]
    fn hook_no_filter_matches_all() {
        let hook = Hook {
            name: "unfiltered".into(),
            before: None,
            after: None,
            tool_filter: None,
        };
        assert!(hook.matches("any_tool"));
    }

    #[test]
    fn fire_before_first_abort_wins() {
        let mut registry = HookRegistry::new();
        registry.register(Hook {
            name: "aborter".into(),
            before: Some(Box::new(|_name, _inputs| HookResult {
                abort: true,
                reason: "first".into(),
                modified_inputs: None,
            })),
            after: None,
            tool_filter: None,
        });
        registry.register(Hook {
            name: "never_reached".into(),
            before: Some(Box::new(|_name, _inputs| HookResult {
                abort: true,
                reason: "second".into(),
                modified_inputs: None,
            })),
            after: None,
            tool_filter: None,
        });
        let result = registry.fire_before("tool", &json!({}));
        assert!(result.abort);
        assert_eq!(result.reason, "first");
    }

    #[test]
    fn fire_after_chains_modifications() {
        let mut registry = HookRegistry::new();
        registry.register(Hook {
            name: "modifier".into(),
            before: None,
            after: Some(Box::new(|_name, _inputs, _result, _ms| PostHookResult {
                modified_result: Some(json!({"modified": true})),
                log_message: String::new(),
            })),
            tool_filter: None,
        });
        let result = registry.fire_after("tool", &json!({}), &json!({}), 0.0);
        assert_eq!(result, json!({"modified": true}));
    }

    #[test]
    fn fire_after_never_aborts() {
        let mut registry = HookRegistry::new();
        // Even if a post-hook panics, fire_after should not propagate it
        registry.register(Hook {
            name: "panicker".into(),
            before: None,
            after: Some(Box::new(|_name, _inputs, _result, _ms| {
                panic!("intentional panic in post-hook");
            })),
            tool_filter: None,
        });
        registry.register(Hook {
            name: "normal".into(),
            before: None,
            after: Some(Box::new(|_name, _inputs, _result, _ms| PostHookResult {
                modified_result: Some(json!({"survived": true})),
                log_message: "after panic".into(),
            })),
            tool_filter: None,
        });
        let result = registry.fire_after("tool", &json!({}), &json!({}), 0.0);
        assert_eq!(result, json!({"survived": true}));
    }

    #[test]
    fn safety_hook_checks_multiple_keywords() {
        let registry = build_default_hooks();
        for keyword in &["delete", "remove", "destroy", "truncate", "reset"] {
            let inputs = json!({"cmd": format!("please {} it", keyword)});
            let result = registry.fire_before("tool", &inputs);
            assert!(result.abort, "should block '{}'", keyword);
        }
    }
}
