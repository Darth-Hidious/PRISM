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
#[derive(Debug, Clone, Default)]
pub struct HookResult {
    pub abort: bool,
    pub reason: String,
    pub modified_inputs: Option<Value>,
}

/// Result of a post-hook execution.
#[derive(Debug, Clone, Default)]
pub struct PostHookResult {
    pub modified_result: Option<Value>,
    pub log_message: String,
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
            if let Some(ref before) = hook.before
                && hook.matches(tool_name)
            {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    before(tool_name, inputs)
                }));
                match result {
                    Ok(hr) => {
                        if hr.abort {
                            info!("Hook '{}' aborted {}: {}", hook.name, tool_name, hr.reason);
                            return hr;
                        }
                    }
                    Err(_) => {
                        warn!("Pre-hook '{}' panicked", hook.name);
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
            if let Some(ref after) = hook.after
                && hook.matches(tool_name)
            {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    after(tool_name, inputs, &current, elapsed_ms)
                }));
                match outcome {
                    Ok(post_result) => {
                        if !post_result.log_message.is_empty() {
                            info!("Post-hook '{}': {}", hook.name, post_result.log_message);
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
        current
    }
}

// ── Built-in hooks ──────────────────────────────────────────────────

/// Pre-hook on ALL tools. Scans arg values for destructive keywords.
/// If found, aborts with a reason string.
pub fn safety_hook() -> Hook {
    let destructive: HashSet<&str> = ["delete", "drop", "remove", "destroy", "truncate", "reset"]
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
        after: Some(Box::new(|tool_name, _inputs, _result, elapsed_ms| {
            PostHookResult {
                modified_result: None,
                log_message: format!("{}: {:.0}ms", tool_name, elapsed_ms),
            }
        })),
        tool_filter: None,
    }
}

/// Post-hook on ALL tools. Logs `"[AUDIT] {tool_name} {OK|ERROR} ({elapsed_ms}ms)"`.
pub fn audit_hook() -> Hook {
    Hook {
        name: "audit_log".into(),
        before: None,
        after: Some(Box::new(|tool_name, _inputs, result, elapsed_ms| {
            let status = if result.get("error").is_some() {
                "ERROR"
            } else {
                "OK"
            };
            PostHookResult {
                modified_result: None,
                log_message: format!("[AUDIT] {} {} ({:.0}ms)", tool_name, status, elapsed_ms),
            }
        })),
        tool_filter: None,
    }
}

/// Live provenance context — the REAL session id and model name behind
/// every ledger row. protocol.rs updates this on session init, resume,
/// and model switch; the provenance hook, the meta-tool recall scope,
/// and the LLM-turn recorder all read it.
///
/// Pre-fix every record carried the literal session_id "session" and a
/// NULL llm_model — six days of history blended into one bucket that
/// `recall` couldn't scope. A process-wide RwLock is the boring fix:
/// the hook closure has a fixed signature and one process serves one
/// session at a time.
pub static PROVENANCE_CTX: std::sync::RwLock<ProvenanceCtx> =
    std::sync::RwLock::new(ProvenanceCtx::empty());

#[derive(Clone, Debug)]
pub struct ProvenanceCtx {
    pub session_id: String,
    pub llm_model: String,
}

impl ProvenanceCtx {
    const fn empty() -> Self {
        Self {
            session_id: String::new(),
            llm_model: String::new(),
        }
    }
}

/// Update the live provenance context. Call whenever the session id or
/// model changes. Empty strings leave the existing value untouched.
pub fn set_provenance_ctx(session_id: &str, llm_model: &str) {
    if let Ok(mut ctx) = PROVENANCE_CTX.write() {
        if !session_id.is_empty() {
            ctx.session_id = session_id.to_string();
        }
        if !llm_model.is_empty() {
            ctx.llm_model = llm_model.to_string();
        }
    }
}

/// Current session id for provenance rows ("unknown" until init).
pub fn provenance_session_id() -> String {
    PROVENANCE_CTX
        .read()
        .ok()
        .map(|c| c.session_id.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn provenance_model() -> Option<String> {
    PROVENANCE_CTX
        .read()
        .ok()
        .map(|c| c.llm_model.clone())
        .filter(|m| !m.is_empty())
}

/// Build the default hook registry with safety + cost + audit + provenance hooks.
pub fn build_default_hooks() -> HookRegistry {
    let mut registry = HookRegistry::new();
    registry.register(safety_hook());
    registry.register(cost_hook());
    registry.register(audit_hook());
    registry.register(provenance_hook());
    registry
}

/// Derive the (status, exit_code) pair for a provenance record from the tool
/// result. Extracted as a pure helper so it can be unit-tested WITHOUT firing
/// the hook (which spawns a real DB write — out of bounds for unit tests).
/// The status string is the SAME signal the F1 is_error gate uses
/// ([`crate::tool_result::tool_result_is_error`]); keeping it inline in the
/// closure would let the record drift from the gate.
fn classify_for_provenance(result: &Value) -> (Option<String>, Option<i64>) {
    let status = if crate::tool_result::tool_result_is_error(result) {
        "error"
    } else {
        "ok"
    };
    (
        Some(status.to_string()),
        crate::tool_result::tool_exit_code(result),
    )
}

/// Provenance hook — records every tool call to Turso via a spawned
/// async task. Non-blocking: the hook returns immediately, the write
/// happens in the background.
fn provenance_hook() -> Hook {
    use prism_provenance::{ActionType, Actor, new_record};

    Hook {
        name: "provenance".to_string(),
        before: None,
        after: Some(Box::new(move |tool_name, inputs, result, _elapsed_ms| {
            // Spawn an async task to write the provenance record.
            // This requires being inside a tokio runtime — the agent
            // loop runs inside one, so this works.
            let session_id = provenance_session_id();
            let model = provenance_model();
            let mut record = new_record(
                &session_id,
                ActionType::ToolCall,
                Actor::Agent,
                Some(tool_name),
                model.as_deref(),
                inputs.clone(),
            );
            // Record the output too so `recall` can pull the full result
            // back later (by id or keyword), not just the tool's inputs.
            record.output_json = Some(result.clone());
            // VS1/F5: structured outcome flag, derived from the SAME signal
            // as the F1 is_error gate (crates/agent/src/tool_result.rs) so the
            // flag, the gate, and the summary can never disagree. "which runs
            // failed" is now a real query against the provenance store.
            let (status, exit_code) = classify_for_provenance(result);
            record.status = status;
            record.exit_code = exit_code;

            // Try to spawn a background write task.
            // VS1/F5: a write failure must NOT be silent. There is no shared
            // metrics counter in the agent crate today, so each failure path
            // emits a warn! under a distinct, grep-able target
            // ("provenance_drop") naming the record + cause. Building a real
            // counter/metric is deferred (see report). The Handle::try_current
            // Err branch — previously completely silent — now warns too.
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        let db_path = dirs::home_dir()
                            .map(|h| h.join(".prism/provenance.db"))
                            .unwrap_or_else(|| std::path::PathBuf::from("provenance.db"));
                        match prism_provenance::ProvenanceStore::open(&db_path).await {
                            Ok(store) => {
                                if let Err(e) = store.record(&record).await {
                                    warn!(
                                        target: "provenance_drop",
                                        tool = %record.tool_name.as_deref().unwrap_or("?"),
                                        session = %record.session_id,
                                        "provenance write failed (record dropped): {e}"
                                    );
                                } else {
                                    // Semantic memory: embed the record so `recall`
                                    // can find it by meaning, not just keyword.
                                    crate::embeddings::embed_record(&store, &record).await;
                                }
                            }
                            Err(e) => {
                                warn!(
                                    target: "provenance_drop",
                                    tool = %record.tool_name.as_deref().unwrap_or("?"),
                                    session = %record.session_id,
                                    "provenance store open failed (record dropped): {e}"
                                );
                            }
                        }
                    });
                }
                Err(e) => {
                    warn!(
                        target: "provenance_drop",
                        tool = %record.tool_name.as_deref().unwrap_or("?"),
                        session = %record.session_id,
                        "provenance write skipped — no tokio runtime (record dropped): {e}"
                    );
                }
            }

            PostHookResult {
                log_message: String::new(),
                modified_result: None,
            }
        })),
        tool_filter: None, // matches all tools
    }
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

    // ── VS1 / F5: provenance status derivation ─────────────────────────
    //
    // classify_for_provenance is a pure helper so it can be tested without
    // firing the hook (which spawns a real DB write). The contract: the
    // status string MUST agree with the F1 is_error gate.

    #[test]
    fn f5_classify_wrapped_python_failure_is_error() {
        let (status, exit) = classify_for_provenance(&json!({
            "success": false, "exit_code": 1, "stderr": "ValueError: boom"
        }));
        assert_eq!(status.as_deref(), Some("error"));
        assert_eq!(exit, Some(1));
    }

    #[test]
    fn f5_classify_success_is_ok() {
        let (status, exit) = classify_for_provenance(&json!({ "success": true, "exit_code": 0 }));
        assert_eq!(status.as_deref(), Some("ok"));
        assert_eq!(exit, Some(0));
    }

    #[test]
    fn f5_classify_grep_no_match_is_ok_not_error() {
        // Regression guard: grep exit-1 with success:true must record as ok,
        // not error — same rule as the F1 gate.
        let (status, exit) = classify_for_provenance(&json!({
            "success": true,
            "exit_code": 1,
            "return_code_interpretation": "No matches found"
        }));
        assert_eq!(status.as_deref(), Some("ok"));
        assert_eq!(exit, Some(1));
    }

    #[test]
    fn f5_classify_top_level_error_is_error() {
        let (status, exit) = classify_for_provenance(&json!({ "error": "unknown tool: frob" }));
        assert_eq!(status.as_deref(), Some("error"));
        assert_eq!(exit, None);
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
