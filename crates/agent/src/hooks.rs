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

/// Resolve whether a tool can WRITE, from what the harness already declares
/// about it — never from the words in its arguments.
///
/// Resolution order mirrors how the catalog itself is assembled:
/// 1. Command-tool specs (`COMMAND_TOOLS`) declare a `permission_mode` per
///    tool (aliases included).
/// 2. Native meta-tools carry an exhaustive effect classification
///    ([`crate::meta_tools::MetaTool::effect`]).
/// 3. Python/platform tools fall through to the global permission map, where
///    an UNKNOWN tool defaults to `WorkspaceWrite` — so an unregistered tool
///    is treated as write-capable (the scan stays fail-closed for tools the
///    harness knows nothing about).
fn tool_is_write_capable(tool_name: &str) -> bool {
    use crate::permissions::PermissionMode;
    if let Some(mode) = crate::command_tools::command_tool_permission_mode(tool_name) {
        return mode != PermissionMode::ReadOnly;
    }
    if let Some(meta) = crate::meta_tools::MetaTool::from_name(tool_name) {
        return meta.effect() != crate::meta_tools::MetaToolEffect::ReadOnly;
    }
    crate::permissions::get_tool_permission(tool_name) != PermissionMode::ReadOnly
}

/// Pre-hook on ALL tools: a destructive-keyword tripwire for WRITE-CAPABLE
/// tools only.
///
/// The gate keys on the tool's registered write capability — a fact the
/// harness already knows — never on English words in a read-only tool's
/// arguments. A search string is data: "droplet spreading in LPBF" and
/// "hydrogen removal from Ti melts" are this product's own domain language,
/// and the previous unanchored `contains` scan over EVERY tool's string
/// arguments aborted exactly those searches ("drop" in "droplet", "remove"
/// in "removal") ahead of every real gate in the system.
///
/// For write-capable tools the keywords match as WHOLE WORDS (alphanumeric/
/// underscore token boundaries). Honest scope: this catches the model
/// spelling out an explicit destructive verb ("DROP TABLE", "git reset
/// --hard", "delete the store") to a tool that can write. It cannot catch
/// destructive intent phrased without these words, hidden inside code, or
/// fused into an identifier ("drop_table") — it is a tripwire, not a
/// security boundary. The real gates (permission map, OPA policy, approval)
/// still run after it.
pub fn safety_hook() -> Hook {
    let destructive: HashSet<&str> = ["delete", "drop", "remove", "destroy", "truncate", "reset"]
        .into_iter()
        .collect();

    Hook {
        name: "safety_guard".into(),
        before: Some(Box::new(move |tool_name, inputs| {
            // Read-only tools take queries, not commands. Their arguments
            // are never scanned.
            if !tool_is_write_capable(tool_name) {
                return HookResult::default();
            }
            if let Value::Object(map) = inputs {
                for (key, val) in map {
                    if let Value::String(s) = val {
                        let lowered = s.to_lowercase();
                        if let Some(word) = lowered
                            .split(|c: char| !c.is_alphanumeric() && c != '_')
                            .find(|token| destructive.contains(token))
                        {
                            // The caller receiving this abort is the MODEL,
                            // which cannot invoke slash commands — so the
                            // recourse it is given is one it can actually
                            // take: ask the human. A human-typed /bash or
                            // /python runs the same policy, permission,
                            // skill and provenance gates but treats THIS
                            // scan as advisory, because a human asked.
                            return HookResult {
                                abort: true,
                                reason: format!(
                                    "Blocked: the word '{}' appears in {}.{} \
                                     and '{}' can write. If the user asked \
                                     for exactly this, ask them to run it \
                                     themselves with /bash or /python (the \
                                     human-typed path treats this check as \
                                     advisory). Otherwise rephrase without \
                                     the destructive wording or use a \
                                     read-only tool.",
                                    word, tool_name, key, tool_name
                                ),
                                modified_inputs: None,
                            };
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
            // VS1 fix-round: classify via the SHARED helper, not a naive
            // `get("error").is_some()`. The old check keyed on error-key
            // PRESENCE, so (a) `{"error": null}` on a *successful* run_skill
            // logged ERROR (false positive), and (b) a run_skill failure that
            // carried no `error` key logged OK (the mask, the other direction).
            // Using tool_result_is_error keeps this audit line consistent with
            // the is_error gate and the provenance status.
            let status = if crate::tool_result::tool_result_is_error(result) {
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
    /// Id of the tool call currently executing, minted by [`begin_action`]
    /// before the tool runs and consumed by [`end_action`] after it returns.
    ///
    /// Process-global for the same reason `session_id` is: the tool that
    /// writes facts is a CHILD PROCESS, so the id has to reach an environment
    /// variable rather than a call argument. `fire_before` -> execute ->
    /// `fire_after` is sequential within a turn, so at most one action is
    /// live at a time. Two agent loops sharing one process would interleave
    /// here — the same limitation the session id already has, and the reason
    /// background research runs out-of-process.
    pub action_id: Option<String>,
}

impl ProvenanceCtx {
    const fn empty() -> Self {
        Self {
            session_id: String::new(),
            llm_model: String::new(),
            action_id: None,
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

/// Mint the id for the tool call about to run, and make it current.
///
/// Returned so the caller can correlate; the usual consumers read it back
/// with [`current_action_id`]. Called from the provenance hook's BEFORE
/// callback, so the id exists while the tool executes — the after hook is
/// too late, because by then the tool has already written its facts.
///
/// Overwrites any previous value unconditionally: a tool that was denied or
/// panicked never reaches [`end_action`], and the next call must not inherit
/// its id.
pub fn begin_action() -> String {
    let id = prism_provenance::new_action_id();
    if let Ok(mut ctx) = PROVENANCE_CTX.write() {
        ctx.action_id = Some(id.clone());
    }
    id
}

/// The tool call currently executing, if one is.
///
/// `None` outside a tool call — attribution is never invented, and callers
/// treat it as "not launched by an agent action".
#[must_use]
pub fn current_action_id() -> Option<String> {
    PROVENANCE_CTX.read().ok().and_then(|c| c.action_id.clone())
}

/// Clear the current action and return what it was.
///
/// Clearing matters: without it, provenance written between turns (or by a
/// tool whose own hooks do not fire) would be attributed to whichever call
/// happened to run last — a wrong attribution, which is worse than none.
pub fn end_action() -> Option<String> {
    PROVENANCE_CTX
        .write()
        .ok()
        .and_then(|mut c| c.action_id.take())
}

fn provenance_model() -> Option<String> {
    PROVENANCE_CTX
        .read()
        .ok()
        .map(|c| c.llm_model.clone())
        .filter(|m| !m.is_empty())
}

/// Resolve the path of the durable provenance store.
///
/// Order: `$PRISM_PROVENANCE_DB` — the platform's documented override for
/// this database (the workflow provenance step honors the same variable),
/// and the injection point tests use to aim writes at a scratch store —
/// then the production default `~/.prism/provenance.db`, unchanged.
///
/// Every open of the durable store in this crate MUST resolve its path
/// here; open-coding the default is how the test suite ended up writing
/// into the user's live database.
#[track_caller]
pub fn provenance_db_path() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("PRISM_PROVENANCE_DB")
        && !p.is_empty()
    {
        return std::path::PathBuf::from(p);
    }
    default_store_path()
}

/// Delegates to the ONE default resolver, so this crate and `prism_provenance`
/// can never name two different files.
///
/// They previously could: this copy read `dirs::home_dir()` (which falls back
/// to getpwuid when HOME is unset) and, failing that, a bare relative
/// `provenance.db`; `prism_provenance::default_store_path` reads `$HOME` and
/// falls back to `.prism/provenance.db`. With HOME unset the agent wrote one
/// file and every reader opened another — two live stores, no error, and the
/// symptom is an empty answer rather than a failure.
///
/// The wrapper survives only to keep the `test-guard` arm below, which is why
/// resolution still routes through this crate at all.
#[cfg(not(feature = "test-guard"))]
fn default_store_path() -> std::path::PathBuf {
    prism_provenance::default_store_path()
}

// `test-guard` is an ordinary PUBLIC cargo feature, so nothing about the
// self dev-dependency stops `cargo build --release --all-features` from
// switching the abort path on. Refuse to compile instead: a build that would
// ship `process::abort()` into a user's `prism` must fail loudly at build
// time, not surprise someone at runtime. `cargo test --release` is caught by
// the same rule, which is the intended trade — the guard exists to protect a
// developer's live store, and debug is where the suite runs.
#[cfg(all(feature = "test-guard", not(debug_assertions)))]
compile_error!(
    "prism-agent: the `test-guard` feature aborts the process on a default \
     store-path resolution and must never be compiled into a release build. \
     It is armed automatically for test targets by the self dev-dependency; \
     do not enable it by hand, and do not use --all-features on a release \
     build."
);

/// `test-guard` build (every test target of this crate, via the self
/// dev-dependency; never a production build): resolving the default path
/// means test code was about to open the user's LIVE provenance store.
/// Abort — deliberately not a panic, because two call sites resolve inside
/// detached `tokio::spawn` tasks, where a panic is silently swallowed and
/// the offending test stays green.
#[cfg(feature = "test-guard")]
#[track_caller]
fn default_store_path() -> std::path::PathBuf {
    eprintln!(
        "FATAL (prism-agent test-guard): {} resolved the DEFAULT provenance \
         store path — the user's live provenance store (~/.prism/provenance.db). \
         Isolate the test by setting PRISM_PROVENANCE_DB to a scratch path: \
         integration binaries declare `mod common;`, the lib test binary \
         injects it pre-main. Aborting instead of panicking because a panic \
         inside a detached tokio task is swallowed.",
        std::panic::Location::caller(),
    );
    std::process::abort();
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
///
/// VS3: `pub(crate)` so the single-tool executor (`crate::service`) records the
/// SAME outcome the provenance hook does, rather than re-deriving the
/// success/error match inline (which would drift from this single source of
/// truth). A hard dispatch `Err` is recorded as an error by the caller before
/// calling this (this fn only classifies a produced `Value`).
pub(crate) fn classify_for_provenance(result: &Value) -> (Option<String>, Option<i64>) {
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

// ── VS2-P1c: PROV-O chaining for the verify-by-execution repair loop ──────
//
// Records the last code-exec tool call so the NEXT one (if it's a repair of
// the same tool after a failure) can point at it via `parent_id` and tag
// itself `repair_attempt`. Walking the `parent_id` chain then reconstructs the
// whole repair sequence. Mirrors the PROVENANCE_CTX static pattern.

/// Code-execution tools whose consecutive failures form a repair chain.
const PROV_CODE_EXEC_TOOLS: &[&str] = &["execute_python", "execute_bash", "notebook_exec"];

#[derive(Clone, Debug)]
pub struct LastCodeRun {
    // The canonical tool name is the HashMap KEY, so it's not stored here too.
    record_id: String,
    failed: bool,
}

/// One-process memory of recent code-exec calls, keyed by CANONICAL tool name
/// (FIX-6: was a single Option<LastCodeRun> slot, which an interleaving call
/// of a DIFFERENT code-exec tool overwrote — so python-fail -> execute_bash ->
/// python-retry severed the chain because bash clobbered the slot. Per-tool
/// slots survive the interleaving: bash writes its own slot, the python slot
/// is preserved for the retry to chain against). Mutable through a Mutex.
static LAST_CODE_RUN: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, LastCodeRun>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Clear the repair-chain memory (FIX-6). Called at turn start so a new turn's
/// first code run is not tagged repair_attempt pointing at last turn's failure.
pub fn reset_code_run_chain() {
    // F4: degrade-safe on lock poison (no-op), but LOG it so a permanently
    // disabled repair chain isn't silent-forever. Very low likelihood.
    match LAST_CODE_RUN.lock() {
        Ok(mut guard) => guard.clear(),
        Err(_) => warn!(
            target: "provenance_drop",
            "LAST_CODE_RUN poisoned; repair-chain reset skipped (chaining degraded)"
        ),
    }
}

/// G2: snapshot/restore the repair-chain memory around a nested subagent turn.
///
/// `reset_code_run_chain` at `run_turn` entry was meant to isolate subagents
/// but it WIPES the parent's in-flight chain (parent loses its parent_id), and
/// the subagent's leftover record is never cleared on return -> the parent's
/// next code call chains against the subagent's record (wrong parent_id +
/// repair_attempt tag). Snapshot BEFORE the nested turn and RESTORE AFTER so
/// the parent's chain survives intact and the subagent's writes are discarded.
pub fn snapshot_code_run_chain() -> std::collections::HashMap<String, LastCodeRun> {
    LAST_CODE_RUN
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default()
}

pub fn restore_code_run_chain(map: std::collections::HashMap<String, LastCodeRun>) {
    // F4: degrade-safe on lock poison (no-op), but LOG it — a silently dropped
    // restore permanently+invisibly disables repair-chain provenance.
    match LAST_CODE_RUN.lock() {
        Ok(mut guard) => *guard = map,
        Err(_) => warn!(
            target: "provenance_drop",
            "LAST_CODE_RUN poisoned; repair-chain restore skipped (chaining degraded)"
        ),
    }
}

/// H3: RAII guard that snapshots the repair-chain memory on construct and
/// restores it on DROP — covering the normal return, an `Err`, AND a panic
/// unwinding through the nested subagent turn.
///
/// The previous manual snapshot + restore-before-`?` in subagent.rs restored on
/// Ok/Err but a panic unwinding through `run_turn` SKIPPED the restore, leaving
/// `LAST_CODE_RUN` reset/subagent-populated for whatever ran next. `Drop` runs
/// on unwind too, so the parent's chain is always put back. Lock-poison degrades
/// safe (restore is a no-op if the mutex is poisoned).
#[must_use = "hold the guard for the duration of the nested turn; dropping it early restores the chain prematurely"]
pub struct CodeRunChainGuard {
    snapshot: std::collections::HashMap<String, LastCodeRun>,
}

impl CodeRunChainGuard {
    /// Snapshot the current repair-chain memory; restored when the guard drops.
    pub fn new() -> Self {
        Self {
            snapshot: snapshot_code_run_chain(),
        }
    }
}

impl Default for CodeRunChainGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CodeRunChainGuard {
    fn drop(&mut self) {
        restore_code_run_chain(std::mem::take(&mut self.snapshot));
    }
}

/// Pure helper: given the current tool + its status, and the remembered last
/// code runs (per-tool), decide (parent_id, tags) for the new record.
///
/// - Code-exec tools always get the `code_exec` tag (so "all code runs" is
///   queryable).
/// - If the PREVIOUS run of the SAME (canonical) tool failed, this run is a
///   repair attempt: set `parent_id` to the previous record and tag
///   `repair_attempt`.
/// - Chains NEVER cross tools (an execute_python failure does not make a later
///   execute_bash run a "repair").
/// - Non-code-exec tools: empty tags, no parent.
///
/// Extracted as a pure fn so it's unit-testable without the static / the hook.
fn chain_code_run(
    tool_name: &str,
    status: &str,
    last: &std::collections::HashMap<String, LastCodeRun>,
) -> (Option<String>, Vec<String>) {
    // FIX-5: normalize the tool name so alias-invoked notebook cells
    // (notebook_run/run_python_notebook/notebook -> notebook_exec) get the
    // code_exec tag and chain correctly.
    let canonical = crate::command_tools::canonical_code_exec_tool(tool_name);
    if !PROV_CODE_EXEC_TOOLS.contains(&canonical) {
        return (None, Vec::new());
    }
    let mut tags = vec!["code_exec".to_string()];
    let mut parent_id = None;
    // FIX-6: look up the PREVIOUS run of THIS tool (per-tool slots). A different
    // tool's run no longer severs the chain.
    if let Some(prev) = last.get(canonical)
        && prev.failed
    {
        parent_id = Some(prev.record_id.clone());
        tags.push("repair_attempt".to_string());
    }
    // status is "error" or "ok" (from classify_for_provenance); unused beyond
    // the call site updating LAST_CODE_RUN, but kept in the signature so the
    // helper is self-contained and testable for the "reset on success" path.
    let _ = status;
    (parent_id, tags)
}

/// Provenance hook — records every tool call to Turso via a spawned
/// async task. Non-blocking: the hook returns immediately, the write
/// happens in the background.
fn provenance_hook() -> Hook {
    use prism_provenance::{ActionType, Actor, new_record};

    Hook {
        name: "provenance".to_string(),
        // Mint the action id BEFORE the tool runs. It has to exist while the
        // tool executes: the tools that write facts are child processes that
        // read the id from the environment, and by the after-hook they have
        // already written. Minting here is also what makes the id and the
        // record id the same string, which IS the join.
        before: Some(Box::new(move |_tool_name, _inputs| {
            begin_action();
            HookResult::default()
        })),
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
            record.status = status.clone();
            record.exit_code = exit_code;

            // Adopt the id minted before the tool ran, replacing the fresh one
            // `new_record` generated. Any activity this tool caused stored the
            // SAME string in `prov_activity.origin_action_id`, so
            // `assertions_from_action(record.id)` now answers "what did this
            // call buy". `end_action` also clears it, so provenance written
            // between turns is not misattributed to the last call. Falling
            // back to the generated id keeps the record writable when no
            // action was current — an unattributed record, never a lost one.
            if let Some(action_id) = end_action() {
                record.id = action_id;
            }

            // VS2-P1c: PROV-O chaining for the verify-by-execution repair loop.
            // For code-exec tools, if the previous code-exec run was the SAME
            // tool and FAILED, this run is a repair attempt — point at the
            // previous record via parent_id and tag repair_attempt. Always tag
            // code_exec. Then remember THIS run as the new "last" for the next
            // call. Walking parent_id reconstructs the whole repair chain.
            let status_str = status.as_deref().unwrap_or("ok");
            let last = LAST_CODE_RUN
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default();
            let (parent_id, chain_tags) = chain_code_run(tool_name, status_str, &last);
            if let Some(pid) = parent_id {
                record.parent_id = Some(pid);
            }
            record.tags.extend(chain_tags);
            // Update LAST_CODE_RUN only for code-exec tools (non-code tools
            // never participate in a repair chain). `failed` drives whether the
            // NEXT same-tool call is tagged repair_attempt. FIX-5: store the
            // CANONICAL name so an alias-invoked cell updates the same slot.
            let canonical_for_chain = crate::command_tools::canonical_code_exec_tool(tool_name);
            if PROV_CODE_EXEC_TOOLS.contains(&canonical_for_chain) {
                let this_run = LastCodeRun {
                    record_id: record.id.clone(),
                    failed: status_str == "error",
                };
                // FIX-6: per-tool slot — insert/replace only THIS tool's entry,
                // leaving other tools' slots intact (so python-fail -> bash ->
                // python-retry keeps the python slot for the retry to chain).
                if let Ok(mut guard) = LAST_CODE_RUN.lock() {
                    guard.insert(canonical_for_chain.to_string(), this_run);
                }
            }

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
                        let db_path = provenance_db_path();
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

    /// F1: serialize the tests that mutate the PROCESS-GLOBAL `LAST_CODE_RUN`
    /// static. `g2_snapshot_restore_roundtrip_preserves_state`,
    /// `h3_chain_guard_restores_on_normal_drop`, and
    /// `h3_chain_guard_restores_on_panic_unwind` all reset/populate/assert on
    /// that one static with NO synchronization, so at DEFAULT parallelism they
    /// race (one test's `reset_code_run_chain` wipes another's mid-assert →
    /// ~10% flaky; `--test-threads=1` was always green). This mirrors the
    /// `SERIAL_TEST_LOCK` already used in tests/subagent_nested_turn.rs. A plain
    /// std Mutex suffices (these tests are sync, not async); we recover from a
    /// poisoned lock (`into_inner`) since the guard only serializes — it holds no
    /// invariant of its own.
    static SERIAL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Guard self-test: a test that resolves the DEFAULT store path (no
    /// `PRISM_PROVENANCE_DB` override) must be killed, not allowed to open
    /// the user's live `~/.prism/provenance.db`. Runs the probe below in a
    /// subprocess because the guard aborts the whole process — deliberately,
    /// since a panic inside a detached tokio task is swallowed and the
    /// offending test would stay green.
    ///
    /// `PRISM_TEST_NO_STORE_ISOLATION=1` keeps the probe process's pre-main
    /// ctor from re-injecting a scratch path — i.e. the probe IS "one test
    /// with its isolation removed".
    #[test]
    fn guard_kills_a_test_that_resolves_the_default_store_path() {
        let exe = std::env::current_exe().expect("test binary path");
        let out = std::process::Command::new(exe)
            .args([
                "--exact",
                "hooks::tests::guard_probe_resolves_default_store_path",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env_remove("PRISM_PROVENANCE_DB")
            .env("PRISM_TEST_NO_STORE_ISOLATION", "1")
            .output()
            .expect("spawn guard probe subprocess");
        assert!(
            !out.status.success(),
            "guard did NOT fire: the probe resolved the default (live) store \
             path and exited cleanly.\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("test-guard"),
            "probe died, but without the guard's abort message — it must \
             fail FOR THE RIGHT REASON.\nstderr: {stderr}"
        );
    }

    #[test]
    #[ignore = "probe: run only as the subprocess of \
                guard_kills_a_test_that_resolves_the_default_store_path"]
    fn guard_probe_resolves_default_store_path() {
        let _ = super::provenance_db_path();
    }

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

    // ── M1: the gate keys on write capability, never on words in a
    // read-only tool's query ─────────────────────────────────────────

    #[test]
    fn m1_read_only_search_accepts_domain_language() {
        // `papers` is declared ReadOnly in COMMAND_TOOLS. Melt-pool
        // literature is made of the word "droplet"; these queries previously
        // aborted on the unanchored substrings "drop"/"remove".
        let registry = build_default_hooks();
        for query in [
            "droplet spreading in LPBF",
            "hydrogen removal from Ti melts",
            "support removal after additive manufacturing",
            "drop tower microgravity solidification",
        ] {
            let result = registry.fire_before("papers", &json!({ "query": query }));
            assert!(
                !result.abort,
                "read-only search must accept {query:?}: {}",
                result.reason
            );
        }
    }

    #[test]
    fn m1_read_only_tool_is_never_scanned_even_for_exact_words() {
        // Stronger than anchoring: even an EXACT destructive word in a
        // read-only tool's argument passes, because a query is data, not a
        // command. Covers all three capability-resolution branches:
        // command-tool spec (`papers`), meta-tool effect (`recall`), and the
        // global permission map (`materials_search`).
        let registry = build_default_hooks();
        for tool in ["papers", "recall", "materials_search"] {
            let result = registry.fire_before(tool, &json!({ "query": "how to delete and reset" }));
            assert!(
                !result.abort,
                "read-only tool '{tool}' must never be scanned: {}",
                result.reason
            );
        }
    }

    #[test]
    fn m1_write_capable_tool_is_still_gated() {
        // `execute_bash` is FullAccess; `knowledge_ingest` is WorkspaceWrite.
        // A whole destructive word in their arguments still aborts.
        let registry = build_default_hooks();
        let bash = registry.fire_before("execute_bash", &json!({ "command": "git reset --hard" }));
        assert!(bash.abort, "write-capable tool must stay gated");
        assert!(bash.reason.contains("reset"));
        let ingest = registry.fire_before(
            "knowledge_ingest",
            &json!({ "content": "DROP TABLE users" }),
        );
        assert!(ingest.abort, "workspace-write tool must stay gated");
    }

    #[test]
    fn m1_write_capable_gate_matches_whole_words_not_substrings() {
        // "Dropbox" contains "drop" and "droplet_data" contains "drop";
        // neither is the word. This goes RED if the scan reverts to
        // `contains`.
        let registry = build_default_hooks();
        for command in ["ls ~/Dropbox", "cat droplet_data.csv", "echo removalists"] {
            let result = registry.fire_before("execute_bash", &json!({ "command": command }));
            assert!(
                !result.abort,
                "substring must not trip the whole-word gate for {command:?}: {}",
                result.reason
            );
        }
    }

    #[test]
    fn m1_abort_reason_names_recourse_the_model_can_take() {
        // The abort is delivered to the MODEL, which cannot invoke slash
        // commands. The recourse must be one the caller can act on: asking
        // the human — not "run it yourself".
        let registry = build_default_hooks();
        let result =
            registry.fire_before("execute_bash", &json!({ "command": "rm -rf x; drop it" }));
        assert!(result.abort);
        assert!(
            result.reason.contains("ask them to run it"),
            "reason must direct the model to ask the human: {}",
            result.reason
        );
        assert!(
            !result.reason.contains("Run it yourself"),
            "reason must not tell the model to invoke human-only slash commands: {}",
            result.reason
        );
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
    fn audit_hook_null_error_on_success_is_ok_not_error() {
        // VS1 fix-round regression guard: a successful run_skill now carries a
        // PRESENT-but-null `error` key. The old `get("error").is_some()` check
        // logged that as ERROR (false positive). The shared classifier reads
        // null via .as_str() -> not an error, so this must log OK.
        let hook = audit_hook();
        let after = hook.after.as_ref().unwrap();
        let ok_skill = json!({ "name": "s", "ok": true, "success": true, "error": null });
        let result = after("run_skill", &json!({}), &ok_skill, 10.0);
        assert!(
            result.log_message.contains("[AUDIT] run_skill OK (10ms)"),
            "null error on a successful skill must log OK: {}",
            result.log_message
        );
    }

    #[test]
    fn audit_hook_failed_skill_logs_error() {
        // The other direction the old check got wrong: a failed run_skill that
        // (pre-fix) carried no `error` key logged OK. It now carries the
        // success/error contract and must log ERROR via the shared classifier.
        let hook = audit_hook();
        let after = hook.after.as_ref().unwrap();
        let bad_skill = json!({
            "name": "s", "ok": false, "success": false,
            "error": "skill 's' exited non-zero (exit 7); see stderr"
        });
        let result = after("run_skill", &json!({}), &bad_skill, 20.0);
        assert!(
            result
                .log_message
                .contains("[AUDIT] run_skill ERROR (20ms)"),
            "failed skill must log ERROR: {}",
            result.log_message
        );
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

    // ── VS3: the single-tool executor (`crate::service::invoke_tool`) now
    // records status/exit_code by calling this same classifier on the record's
    // output_json. These pin the EXACT shapes that path feeds in — a guard
    // against drift, since the executor previously re-derived the match inline.

    #[test]
    fn f5_classify_service_wrapped_failure_is_error() {
        // service.rs Ok arm when a Python tool failed: output_json is the
        // tool_server-wrapped {"result": {"success":false, ...}}.
        let (status, exit) = classify_for_provenance(&json!({
            "result": { "success": false, "exit_code": -11, "stderr": "SIGSEGV" }
        }));
        assert_eq!(status.as_deref(), Some("error"));
        assert_eq!(exit, Some(-11));
    }

    #[test]
    fn f5_classify_service_wrapped_success_is_ok() {
        let (status, exit) = classify_for_provenance(&json!({
            "result": { "success": true, "exit_code": 0, "stdout": "ok" }
        }));
        assert_eq!(status.as_deref(), Some("ok"));
        assert_eq!(exit, Some(0));
    }

    #[test]
    fn f5_classify_service_dispatch_err_is_error() {
        // service.rs Err arm: output_json is {"error": "<formatted err>"}.
        // Must classify as error with no exit code (there was no process).
        let (status, exit) =
            classify_for_provenance(&json!({ "error": "tool dispatch failed: timeout" }));
        assert_eq!(status.as_deref(), Some("error"));
        assert_eq!(exit, None);
    }

    // ── VS2-P1c: chain_code_run (PROV-O repair chaining) ───────────────

    use std::collections::HashMap;

    /// Build a per-tool LAST_CODE_RUN map with one entry.
    fn last_run(tool: &str, failed: bool) -> HashMap<String, LastCodeRun> {
        let mut m = HashMap::new();
        m.insert(
            tool.to_string(),
            LastCodeRun {
                record_id: format!("rec-{tool}-1"),
                failed,
            },
        );
        m
    }

    /// Build a map with TWO entries (the interleaving case FIX-6 fixes).
    fn last_run_two(
        tool_a: &str,
        failed_a: bool,
        tool_b: &str,
        failed_b: bool,
    ) -> HashMap<String, LastCodeRun> {
        let mut m = HashMap::new();
        m.insert(
            tool_a.to_string(),
            LastCodeRun {
                record_id: format!("rec-{tool_a}-1"),
                failed: failed_a,
            },
        );
        m.insert(
            tool_b.to_string(),
            LastCodeRun {
                record_id: format!("rec-{tool_b}-1"),
                failed: failed_b,
            },
        );
        m
    }

    #[test]
    fn p1c_chains_fail_to_attempt_same_tool() {
        // Previous execute_python failed → this execute_python is a repair.
        let (parent, tags) =
            chain_code_run("execute_python", "error", &last_run("execute_python", true));
        assert_eq!(parent.as_deref(), Some("rec-execute_python-1"));
        assert!(tags.contains(&"code_exec".to_string()));
        assert!(tags.contains(&"repair_attempt".to_string()));
    }

    #[test]
    fn p1c_does_not_chain_across_different_tools() {
        // A failed execute_python does NOT make a later execute_bash a "repair".
        let (parent, tags) =
            chain_code_run("execute_bash", "error", &last_run("execute_python", true));
        assert_eq!(parent, None, "no cross-tool chaining");
        assert!(tags.contains(&"code_exec".to_string()));
        assert!(
            !tags.contains(&"repair_attempt".to_string()),
            "different tool is not a repair attempt"
        );
    }

    #[test]
    fn p1c_resets_on_success_no_repair_tag() {
        // Previous execute_python SUCCEEDED → this one is not a repair, even
        // though it's the same tool.
        let (parent, tags) = chain_code_run(
            "execute_python",
            "error",
            &last_run("execute_python", false),
        );
        assert_eq!(parent, None, "success resets the chain");
        assert!(tags.contains(&"code_exec".to_string()));
        assert!(!tags.contains(&"repair_attempt".to_string()));
    }

    #[test]
    fn p1c_first_call_has_no_parent() {
        // No previous run at all → no parent, just the code_exec tag.
        let empty: HashMap<String, LastCodeRun> = HashMap::new();
        let (parent, tags) = chain_code_run("execute_python", "error", &empty);
        assert_eq!(parent, None);
        assert_eq!(tags, vec!["code_exec".to_string()]);
    }

    #[test]
    fn p1c_non_code_tools_get_no_tags() {
        let (parent, tags) = chain_code_run("search", "error", &last_run("search", true));
        assert_eq!(parent, None);
        assert!(tags.is_empty(), "non-code tools don't participate");
    }

    #[test]
    fn p1c_code_exec_tag_always_present_for_code_tools() {
        // Even on a successful first call, code-exec tools get the code_exec tag
        // so "all code runs" is queryable.
        let empty: HashMap<String, LastCodeRun> = HashMap::new();
        let (_, tags) = chain_code_run("notebook_exec", "ok", &empty);
        assert!(tags.contains(&"code_exec".to_string()));
    }

    #[test]
    fn fix6_interleaving_different_tool_does_not_sever_chain() {
        // FIX-6: python-fail -> execute_bash -> python-retry. The bash run must
        // NOT clobber the python slot (the old single-slot design did, so the
        // retry got no parent_id). With per-tool slots, the python slot survives
        // the bash call and the retry chains correctly.
        let last = last_run_two("execute_python", true, "execute_bash", false);
        // Now the python retry runs: it should chain to the python failure
        // EVEN THOUGH execute_bash ran in between and is in the map.
        let (parent, tags) = chain_code_run("execute_python", "error", &last);
        assert_eq!(
            parent.as_deref(),
            Some("rec-execute_python-1"),
            "interleaving bash must not sever the python repair chain"
        );
        assert!(tags.contains(&"repair_attempt".to_string()));
    }

    #[test]
    fn fix6_reset_code_run_chain_clears_memory() {
        // reset_code_run_chain (called at turn start) clears the per-tool map.
        // We verify via chain_code_run: after a reset, a run has no parent.
        // (We can't easily call the static reset from a unit test without
        // affecting global state, so we verify the empty-map semantics that
        // reset produces: chain_code_run on an empty map yields no parent.)
        let empty: HashMap<String, LastCodeRun> = HashMap::new();
        let (parent, tags) = chain_code_run("execute_python", "error", &empty);
        assert_eq!(parent, None);
        assert!(!tags.contains(&"repair_attempt".to_string()));
        assert!(tags.contains(&"code_exec".to_string()));
    }

    #[test]
    fn g2_snapshot_restore_roundtrip_preserves_state() {
        // G2: snapshot/restore the global LAST_CODE_RUN map. A snapshot taken
        // before a (simulated) subagent reset, then restored, recovers the
        // parent's chain intact. This is the unit-testable core of the fix;
        // the subagent.rs wiring calls these around the nested run_turn.
        let _serial = SERIAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_code_run_chain();
        // Populate the global via the public restore (insert a parent record).
        let mut parent_map: HashMap<String, LastCodeRun> = HashMap::new();
        parent_map.insert(
            "execute_python".to_string(),
            LastCodeRun {
                record_id: "PARENT-REC-1".to_string(),
                failed: true,
            },
        );
        restore_code_run_chain(parent_map.clone());

        // Subagent entry: snapshot, then the nested run_turn resets the global.
        let snap = snapshot_code_run_chain();
        assert_eq!(
            snap.get("execute_python").map(|r| r.record_id.as_str()),
            Some("PARENT-REC-1")
        );
        reset_code_run_chain(); // nested turn wipes it
        // During the nested turn, the global is empty (subagent starts clean).
        let empty_snap = snapshot_code_run_chain();
        assert!(empty_snap.is_empty());

        // Subagent exit: restore the parent's snapshot.
        restore_code_run_chain(snap);
        // The parent's chain is intact: chain_code_run finds the parent record.
        let (parent_id, tags) =
            chain_code_run("execute_python", "error", &snapshot_code_run_chain());
        assert_eq!(
            parent_id.as_deref(),
            Some("PARENT-REC-1"),
            "parent's chain must survive the nested subagent turn"
        );
        assert!(tags.contains(&"repair_attempt".to_string()));

        // Clean up global state so this test doesn't leak into others.
        reset_code_run_chain();
    }

    #[test]
    fn h3_chain_guard_restores_on_normal_drop() {
        // H3: the RAII guard restores the parent's chain when it drops at end of
        // scope — the Ok path. A subagent-populated slot is discarded.
        let _serial = SERIAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_code_run_chain();
        restore_code_run_chain(last_run("execute_python", true)); // parent failed
        {
            let _g = CodeRunChainGuard::new(); // snapshots the parent chain
            reset_code_run_chain(); // nested run_turn entry wipes it
            restore_code_run_chain(last_run("execute_bash", false)); // subagent slot
            assert!(snapshot_code_run_chain().contains_key("execute_bash"));
            assert!(!snapshot_code_run_chain().contains_key("execute_python"));
        } // guard drops here -> parent snapshot restored
        let after = snapshot_code_run_chain();
        assert_eq!(
            after.get("execute_python").map(|r| r.record_id.as_str()),
            Some("rec-execute_python-1"),
            "parent chain restored on drop"
        );
        assert!(
            !after.contains_key("execute_bash"),
            "subagent slot discarded on drop"
        );
        reset_code_run_chain();
    }

    #[test]
    fn h3_chain_guard_restores_on_panic_unwind() {
        // H3 (the core of the fix): a PANIC unwinding through the nested turn
        // must still restore the parent's chain. The old manual restore ran
        // before `?` and was SKIPPED on unwind; `Drop` runs on unwind too.
        //
        // The panic is contained by catch_unwind below, so it never poisons
        // SERIAL_TEST_LOCK — but the other serialized tests recover from poison
        // regardless.
        let _serial = SERIAL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_code_run_chain();
        restore_code_run_chain(last_run("execute_python", true));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = CodeRunChainGuard::new();
            reset_code_run_chain();
            restore_code_run_chain(last_run("execute_bash", false));
            panic!("simulate a panic unwinding through the nested run_turn");
        }));
        assert!(result.is_err(), "the closure panicked");
        let after = snapshot_code_run_chain();
        assert_eq!(
            after.get("execute_python").map(|r| r.record_id.as_str()),
            Some("rec-execute_python-1"),
            "parent chain restored even on panic unwind"
        );
        assert!(
            !after.contains_key("execute_bash"),
            "subagent slot discarded on panic unwind"
        );
        reset_code_run_chain();
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

    // ── Action attribution lifecycle ─────────────────────────────────────

    /// `begin_action` makes an id current; `end_action` hands it back AND
    /// clears it. The clear is the point: provenance written after a tool
    /// returns (between turns, or by a path whose hooks do not fire) must not
    /// inherit the last call's id and be credited to work it did not do.
    #[test]
    fn an_action_id_is_current_only_while_its_tool_runs() {
        let _lock = crate::skills::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        end_action();

        assert_eq!(current_action_id(), None, "no action before one begins");
        let id = begin_action();
        assert_eq!(
            current_action_id(),
            Some(id.clone()),
            "current while running"
        );
        assert_eq!(end_action(), Some(id), "end hands back what began");
        assert_eq!(current_action_id(), None, "cleared once the tool returned");
        assert_eq!(end_action(), None, "ending twice invents nothing");
    }

    /// A tool that was denied or panicked never reaches `end_action`, so the
    /// next `begin_action` must overwrite rather than preserve. Otherwise the
    /// next call's facts would be credited to the call that failed.
    #[test]
    fn a_new_action_replaces_one_that_never_finished() {
        let _lock = crate::skills::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        end_action();

        let abandoned = begin_action();
        let fresh = begin_action(); // previous call never ended
        assert_ne!(abandoned, fresh, "each call gets its own id");
        assert_eq!(
            current_action_id(),
            Some(fresh),
            "the live call owns attribution, not the abandoned one"
        );
        end_action();
    }
}
