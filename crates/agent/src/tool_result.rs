//! Shared classification of tool results.
//!
//! Three call sites need to answer the same question — "did this tool call
//! fail?" — and must never drift apart: the two agent-loop `is_error` gates
//! ([`crate::agent_loop`] and [`crate::protocol`]) and the provenance hook
//! ([`crate::hooks`]). Historically each gate only checked for a top-level
//! `"error"` string, which meant every Python-tool failure arrived wrapped as
//! `{"result": {"success": false, ...}}` and was flagged as success. That is
//! the VS1 crux: the agent was told a failed code run succeeded.
//!
//! The source of truth here is the **tool's own `success` boolean**, NOT the
//! exit code. `app/tools/bash.py::_interpret_exit_code` deliberately treats
//! `grep`/`find`/`diff`/`test` exit-1 as "no match" with `success: true`; a
//! naive `exit_code != 0` rule would over-flag those as errors. So we key on
//! `success` when present, and fall back to a string `error` field.
//!
//! The helper also handles two shapes:
//! - **Wrapped** (Python tools via `tool_server.py`): `{"result": { ... }}`.
//! - **Unwrapped** (Rust notebook/CLI tools): the fields live at the top level.
//!
//! `notebook_exec` is the trap: its top-level `result` field is the
//! last-expression *value* (a string or null), NOT a wrapped payload. We only
//! descend into a `result` sub-object when it is actually an object.

use serde_json::Value;

/// Inspect a tool result and decide whether it represents a failure.
///
/// Order of checks (first match wins):
/// 1. A top-level `"error"` that is a **string** → error. Covers Rust tools'
///    bare error and `tool_server`'s outer wrap when it leaks.
/// 2. A `"result"` sub-**object** (never a string/null — guards `notebook_exec`
///    whose `result` is the last-expr value): error if its `error` is a string
///    OR its `success` is a bool and `== false`.
/// 3. The object itself has `success: bool` → error iff `false` (unwrapped
///    Rust tools: notebook/CLI).
/// 4. Otherwise → not an error. Legacy tools without the field are not flagged.
///
/// We deliberately do NOT key on `exit_code != 0` — `grep`/`find`/`test`
/// exit-1 with `success: true` must remain a non-error.
pub fn tool_result_is_error(value: &Value) -> bool {
    let obj = match value.as_object() {
        Some(o) => o,
        None => return false,
    };

    // (1) Top-level string error — the legacy/outer-wrap signal.
    if obj.get("error").and_then(Value::as_str).is_some() {
        return true;
    }

    // (2) Wrapped payload under "result", but ONLY when it is an object.
    //     notebook_exec's "result" is a string (the last-expression value);
    //     descending into it would misread the payload.
    if let Some(inner) = obj.get("result").and_then(Value::as_object) {
        if inner.get("error").and_then(Value::as_str).is_some() {
            return true;
        }
        if let Some(false) = inner.get("success").and_then(Value::as_bool) {
            return true;
        }
    }

    // (3) Unwrapped Rust tools carry success/error directly at the top level.
    if let Some(false) = obj.get("success").and_then(Value::as_bool) {
        return true;
    }

    // (4) Legacy or partial payload without a signal — do not over-flag.
    false
}

/// Best-effort extraction of the tool's exit code.
///
/// Checks both the wrapped (`result.exit_code`) and unwrapped (`exit_code`)
/// shapes. Returns `None` when absent or non-numeric.
pub fn tool_exit_code(value: &Value) -> Option<i64> {
    let obj = value.as_object()?;
    if let Some(code) = obj
        .get("result")
        .and_then(Value::as_object)
        .and_then(|inner| inner.get("exit_code"))
        .and_then(Value::as_i64)
    {
        return Some(code);
    }
    obj.get("exit_code").and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── tool_result_is_error ──────────────────────────────────────────

    #[test]
    fn wrapped_python_raise_is_error() {
        // execute_python raising -> tool_server wraps {"success":false,...} under "result".
        let v = json!({
            "result": {
                "exit_code": 1,
                "stdout": "",
                "stderr": "Traceback (most recent call last):\nValueError: boom",
                "success": false
            }
        });
        assert!(tool_result_is_error(&v));
    }

    #[test]
    fn wrapped_bash_grep_no_match_is_not_error() {
        // THE LOAD-BEARING REGRESSION GUARD: grep exits 1 but the tool sets
        // success:true (no match is not a failure). Must stay is_error=false.
        let v = json!({
            "result": {
                "success": true,
                "exit_code": 1,
                "stdout": "",
                "stderr": "",
                "return_code_interpretation": "No matches found"
            }
        });
        assert!(!tool_result_is_error(&v));
    }

    #[test]
    fn wrapped_timeout_is_error() {
        let v = json!({
            "result": {
                "success": false,
                "timed_out": true,
                "exit_code": 124,
                "error": "Timed out after 60s"
            }
        });
        assert!(tool_result_is_error(&v));
    }

    #[test]
    fn wrapped_error_string_without_success_is_error() {
        // code.py's timeout/exception branches set `error` but omit `success`;
        // the error-string rule must still catch them.
        let v = json!({
            "result": { "error": "Timed out after 60s", "exit_code": 124 }
        });
        assert!(tool_result_is_error(&v));
    }

    #[test]
    fn wrapped_run_skill_failure_is_error() {
        // VS1 fix-round #1: a stored skill that now crashes returns
        // {"result": {"ok":false, "success":false, "error":..., ...}}. Before
        // the fix run_skill emitted only `ok` (no success/error) and every rule
        // missed it -> green "completed" card + provenance status:ok. With the
        // success/error contract it must trip rule 2 (success:false OR error).
        let v = json!({
            "result": {
                "name": "broken_skill",
                "ok": false,
                "success": false,
                "error": "skill 'broken_skill' exited non-zero (exit 1); see stderr",
                "exit_code": 1,
                "stdout": "",
                "stderr": "Traceback ...\nValueError: boom"
            }
        });
        assert!(tool_result_is_error(&v));
    }

    #[test]
    fn wrapped_run_skill_success_is_not_error() {
        // A clean skill run: ok/success true, error is null -> must NOT flag
        // (the null `error` must not be read as a string-error).
        let v = json!({
            "result": {
                "name": "good_skill",
                "ok": true,
                "success": true,
                "error": null,
                "exit_code": 0,
                "stdout": "42\n",
                "stderr": ""
            }
        });
        assert!(!tool_result_is_error(&v));
    }

    #[test]
    fn unwrapped_notebook_failure_is_error_despite_string_result() {
        // notebook_exec has a top-level success bool AND a top-level "result"
        // field that is the last-expression VALUE (string), not a wrapped
        // payload. The gate must not be fooled by the string "result".
        let v = json!({
            "root": "notebook",
            "invocation": "cell[0]",
            "success": false,
            "exit_code": 1,
            "stderr": "NameError: x",
            "result": "some last-expression string",
            "error": "NameError: x"
        });
        assert!(tool_result_is_error(&v));
    }

    #[test]
    fn unwrapped_notebook_failure_with_null_result_is_error() {
        // Same trap, but result is null (no last expression) and no top-level
        // error string — only success:false. Still an error via branch (3).
        let v = json!({
            "success": false,
            "exit_code": 1,
            "result": null
        });
        assert!(tool_result_is_error(&v));
    }

    #[test]
    fn unwrapped_cli_failure_is_error() {
        let v = json!({ "success": false, "exit_code": 1, "stdout": "", "stderr": "" });
        assert!(tool_result_is_error(&v));
    }

    #[test]
    fn unwrapped_cli_success_is_not_error_even_with_nonzero_exit() {
        // CLI structured_success always emits exit_code 0, but defend against
        // a future caller that sets success:true with a nonzero code: success
        // wins, matching the bash.py contract.
        let v = json!({ "success": true, "exit_code": 0 });
        assert!(!tool_result_is_error(&v));
    }

    #[test]
    fn top_level_error_string_is_error() {
        // Legacy/outer-wrap: tool_server's own error or a Rust tool's bare error.
        let v = json!({ "error": "unknown tool: frobnicate" });
        assert!(tool_result_is_error(&v));
    }

    #[test]
    fn success_payload_without_signal_is_not_error() {
        // Plain result data with no success/error/exit_code — must not flag.
        let v = json!({ "result": { "models": ["a", "b"] } });
        assert!(!tool_result_is_error(&v));
    }

    #[test]
    fn success_true_at_top_level_is_not_error() {
        let v = json!({ "success": true, "exit_code": 0, "stdout": "ok" });
        assert!(!tool_result_is_error(&v));
    }

    #[test]
    fn non_object_is_not_error() {
        assert!(!tool_result_is_error(&json!("just a string")));
        assert!(!tool_result_is_error(&json!(42)));
        assert!(!tool_result_is_error(&Value::Null));
    }

    #[test]
    fn wrapped_success_true_with_string_result_is_not_error() {
        // A wrapped result whose inner payload is fine but happens to be a
        // string (defensive — should not occur for code tools but must not
        // crash or misflag).
        let v = json!({ "result": "ok" });
        assert!(!tool_result_is_error(&v));
    }

    // ── tool_exit_code ────────────────────────────────────────────────

    #[test]
    fn exit_code_wrapped() {
        let v = json!({ "result": { "exit_code": 137, "success": false } });
        assert_eq!(tool_exit_code(&v), Some(137));
    }

    #[test]
    fn exit_code_unwrapped() {
        let v = json!({ "exit_code": 0, "success": true });
        assert_eq!(tool_exit_code(&v), Some(0));
    }

    #[test]
    fn exit_code_absent_returns_none() {
        assert_eq!(
            tool_exit_code(&json!({ "result": { "success": true } })),
            None
        );
        assert_eq!(tool_exit_code(&json!({ "models": [] })), None);
    }

    #[test]
    fn exit_code_notebook_string_result_does_not_confuse() {
        // notebook_exec: top-level result is a string; exit_code is at top level.
        let v = json!({ "exit_code": 1, "result": "last-expr" });
        assert_eq!(tool_exit_code(&v), Some(1));
    }
}
