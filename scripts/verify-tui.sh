#!/usr/bin/env bash
# PRISM TUI verification harness — one-command local verification.
#
# Runs all known-good checks for the TUI crate without requiring
# external services, network, LLMs, or real backend.
#
# Usage:
#   bash scripts/verify-tui.sh
#
# Exits nonzero on first failure (set -euo pipefail).

set -euo pipefail

# ── Configuration ────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

PYTHON="${PYTHON:-python}"
PYTEST_ARGS="-q"

# ── Helpers ─────────────────────────────────────────────────────────

header() {
    echo ""
    echo "════════════════════════════════════════════════════════════"
    echo "  $1"
    echo "════════════════════════════════════════════════════════════"
}

pass() {
    echo "  [PASS] $1"
}

fail() {
    echo "  [FAIL] $1" >&2
    exit 1
}

# ── Checks ──────────────────────────────────────────────────────────

header "PRISM TUI Verification Harness"
echo "  Project: $PROJECT_ROOT"
echo "  Date: $(date -u '+%Y-%m-%d %H:%M:%S UTC')"
echo "  Git: $(git rev-parse --short HEAD 2>/dev/null || echo 'unknown')"

# 1. Formatting
header "1/6: Formatting (cargo fmt --check)"
if cargo fmt --check -p prism-tui -p prism-cli 2>&1; then
    pass "fmt clean"
else
    fail "fmt check found issues — run: cargo fmt -p prism-tui -p prism-cli"
fi

# 2. Build
header "2/6: Build (cargo build)"
if cargo build -p prism-tui -p prism-cli 2>&1; then
    pass "build clean"
else
    fail "build failed"
fi

# 3. Unit tests
header "3/6: Unit tests (cargo test -p prism-tui)"
if cargo test -p prism-tui 2>&1; then
    pass "unit tests passed"
else
    fail "unit tests failed"
fi

# 4. Clippy
header "4/6: Clippy (cargo clippy -D warnings)"
if cargo clippy -p prism-tui -- -D warnings 2>&1; then
    pass "clippy clean"
else
    fail "clippy found warnings"
fi

# 5. PTY e2e tests
header "5/6: PTY e2e tests (pytest tests/test_tui_e2e.py)"
if "$PYTHON" -m pytest tests/test_tui_e2e.py $PYTEST_ARGS 2>&1; then
    pass "PTY tests passed"
else
    # Check if pytest is available
    if ! "$PYTHON" -m pytest --version >/dev/null 2>&1; then
        echo "  [SKIP] pytest not available — skipping PTY tests"
    else
        fail "PTY tests failed"
    fi
fi

# 6. Language drift checker
header "6/6: Language drift (check_no_cjk_in_agent_artifacts.py)"
if "$PYTHON" scripts/check_no_cjk_in_agent_artifacts.py 2>&1; then
    pass "no CJK language drift detected"
else
    fail "CJK language drift detected in agent artifacts"
fi

# ── Summary ─────────────────────────────────────────────────────────
header "Verification Complete"
echo "  All checks passed."
echo ""
echo "  Fake backend scenarios available:"
echo "    prism tui --fake-backend --scenario basic_chat"
echo "    prism tui --fake-backend --scenario streaming_answer"
echo "    prism tui --fake-backend --scenario thinking_stream"
echo "    prism tui --fake-backend --scenario tool_success"
echo "    prism tui --fake-backend --scenario tool_error"
echo "    prism tui --fake-backend --scenario approval_required"
echo "    prism tui --fake-backend --scenario cost_metrics"
echo "    prism tui --fake-backend --scenario backend_warning_error"
echo "    prism tui --fake-backend --scenario ansi_injection"
echo ""
echo "  TUI MCP verification guide: docs/tui-mcp-verification.md"
echo "  Verification contract: PRISM_TUI_VERIFY.md"
echo "  Report template: docs/templates/tui_mcp_verification_report.md"