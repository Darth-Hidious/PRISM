#!/usr/bin/env bash
# smoke-engine-tools.sh — prove the agent loop routes to real tools end-to-end.
#
# Pipes one prompt at a time through prism (non-interactive path in
# crates/cli/src/forge_chat.rs:131), captures stdout/stderr, and checks for:
#   1. clean exit (no panic, no crossterm fail, no premature exit)
#   2. domain-relevant keywords in the response (per-case grep pattern)
#   3. tool routing telemetry from the platform bridge ("semantic top-K: ...")
#
# Cases cover the customer surface: knowledge graph, literature, compute,
# mesh, marketplace, models, capabilities, code execution, materials
# discovery. Re-runnable. Each case = one prism subprocess, one shot.
#
# Usage:
#   scripts/smoke-engine-tools.sh                    # run all cases
#   scripts/smoke-engine-tools.sh -p knowledge_      # run cases whose
#                                                    # name matches glob
#   scripts/smoke-engine-tools.sh -t 60              # per-case timeout
#
# Output:
#   /tmp/prism-smoke/<timestamp>/<case>.{out,err,meta}
#   final pass/fail matrix to stdout

set -uo pipefail

BIN="${PRISM_BIN:-./target/release/prism}"
CASE_PATTERN="*"
TIMEOUT_S=120
RUN_ID="$(date '+%Y%m%d_%H%M%S')"
RUN_DIR="/tmp/prism-smoke/${RUN_ID}"
GTIMEOUT="$(command -v gtimeout || command -v timeout || true)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    -p) CASE_PATTERN="$2"; shift 2 ;;
    -t) TIMEOUT_S="$2"; shift 2 ;;
    --bin) BIN="$2"; shift 2 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

if [[ ! -x "$BIN" ]]; then
  echo "prism binary not found at $BIN — set PRISM_BIN or pass --bin" >&2
  exit 1
fi

mkdir -p "$RUN_DIR"
echo "smoke run: $RUN_ID" > "$RUN_DIR/manifest"
echo "binary:    $BIN" >> "$RUN_DIR/manifest"
echo "version:   $($BIN --version 2>&1)" >> "$RUN_DIR/manifest"
echo "timeout:   ${TIMEOUT_S}s/case" >> "$RUN_DIR/manifest"
echo "filter:    $CASE_PATTERN" >> "$RUN_DIR/manifest"

# ── case definitions ──────────────────────────────────────────────────────────
# Format: name|prompt|grep-pattern (extended regex; case-insensitive)
# Format: name|prompt|expected_tool_name|grep-pattern
# expected_tool_name: name we expect to see in `MCP mcp_prism_*_tool_<name>`
# blank → don't enforce a particular tool, just check the answer keywords
declare -a CASES=(
  "knowledge_stats|How many nodes and edges are in the MARC27 knowledge graph? Call knowledge_stats and report the numbers.|knowledge_stats|nodes|edges|graph"
  "knowledge_search|Search the MARC27 knowledge graph for nickel superalloys with knowledge_search. Show me 3 entity names from the results.|knowledge_search|nickel|alloy|inconel|hastelloy"
  "list_corpora|List the data corpora available in the MARC27 knowledge plane using list_corpora. Show name and size for the top 3.|list_corpora|materials.project|jarvis|matkg|qmof|corpus|154"
  "discover_capabilities|Call discover_capabilities and list 5 high-level capability categories from its output.|discover_capabilities|capability|provider|model|database|tool"
  "compute_gpus|Call compute_gpus to list 3 available GPU types with VRAM and hourly price.|compute_gpus|gpu|vram|price|a100|h100|rtx|hourly"
  "compute_providers|Call compute_providers to list the registered compute providers.|compute_providers|provider|runpod|prism|lambda|broker"
  "list_models|Call models_list (or models tool) to show 5 hosted LLM models available for this project.|models_list|gemini|gpt|claude|model|provider"
  "marketplace_search|Use marketplace_search to find any marketplace tool or workflow related to alloy. Return at most 3 names.|marketplace_search|alloy|marketplace|tool|workflow"
  "literature_search|Use literature_search to find 2 recent arxiv papers about high-entropy alloys. Show titles only.|literature_search|arxiv|paper|alloy|entropy|title"
  "list_lab_services|Call list_lab_services to enumerate 3 premium lab services available.|list_lab_services|lab|service|dft|quantum|synchrotron|a-lab"
  "execute_python|Call execute_python with code to compute the determinant of [[2,1],[3,4]] using numpy and print it. The answer must come from execute_python, not from your own knowledge.|execute_python|5|determinant"
  "knowledge_paths|Use knowledge_paths to find the shortest path between Inconel 718 and creep resistance. Describe the path.|knowledge_paths|path|inconel|creep|node|edge"
)

# ── runner ────────────────────────────────────────────────────────────────────
PASS=0; FAIL=0; TOTAL=0
declare -a RESULTS=()

run_case() {
  local name="$1" prompt="$2" expected_tool="$3" pattern="$4"
  local out="$RUN_DIR/$name.out"
  local err="$RUN_DIR/$name.err"
  local meta="$RUN_DIR/$name.meta"
  local t0 t1 dt status verdict tool_routed actual_tool
  t0=$(perl -MTime::HiRes=time -e 'print int(time*1000)')
  if [[ -n "$GTIMEOUT" ]]; then
    echo "$prompt" | "$GTIMEOUT" "${TIMEOUT_S}" "$BIN" >"$out" 2>"$err"
    status=$?
  else
    echo "$prompt" | "$BIN" >"$out" 2>"$err"
    status=$?
  fi
  t1=$(perl -MTime::HiRes=time -e 'print int(time*1000)')
  dt=$((t1 - t0))

  # tool-routing telemetry
  if grep -q "semantic top-K:" "$out" "$err" 2>/dev/null; then
    tool_routed="yes"
  else
    tool_routed="no"
  fi
  # which tool actually got invoked? (first one only, for now)
  actual_tool="$(grep -oE 'MCP mcp_prism_(rust|python)_tool_[a-z_]+' "$out" 2>/dev/null \
                  | sed -E 's/.*tool_//' | head -1)"
  [[ -z "$actual_tool" ]] && actual_tool="-"

  # error-marker exclusion: any of these → automatic FAIL
  local err_marker=""
  if grep -qiE 'unauthorized|invalid or expired|validation error|platform_bridge_error|400 bad request|panic' "$out"; then
    err_marker="$(grep -oiE 'unauthorized|invalid or expired|validation error|platform_bridge_error|400 bad request|panic' "$out" | head -1)"
  fi

  # keyword check
  local found="no"
  if grep -Eqi "$pattern" "$out"; then
    found="yes"
  fi

  # tool-match check (only if expected_tool is set)
  local tool_match="-"
  if [[ -n "$expected_tool" ]]; then
    if [[ "$actual_tool" == "$expected_tool" ]]; then
      tool_match="ok"
    else
      tool_match="wrong"
    fi
  fi

  if [[ -n "$err_marker" ]]; then
    verdict="FAIL($err_marker)"
  elif [[ $status -ne 0 ]]; then
    verdict="FAIL(exit=$status)"
  elif [[ "$tool_match" == "wrong" ]]; then
    verdict="FAIL(routed=$actual_tool)"
  elif [[ $found != "yes" ]]; then
    verdict="FAIL(no-keyword)"
  else
    verdict="PASS"
  fi

  printf 'case=%s\nstatus=%s\nverdict=%s\ndt_ms=%s\nrouted=%s\nactual_tool=%s\nexpected_tool=%s\nerr_marker=%s\npattern=%s\n' \
    "$name" "$status" "$verdict" "$dt" "$tool_routed" "$actual_tool" "$expected_tool" "$err_marker" "$pattern" > "$meta"

  RESULTS+=("$verdict|$name|${dt}ms|tool=$actual_tool")
  TOTAL=$((TOTAL+1))
  if [[ $verdict == PASS ]]; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); fi

  printf '  [%-25s] %-26s %6sms  tool=%s\n' "$verdict" "$name" "$dt" "$actual_tool" >&2
}

echo "═══ PRISM engine smoke run ${RUN_ID} ═══" >&2
echo "binary: $BIN" >&2
echo "logs:   $RUN_DIR" >&2
echo >&2

for raw in "${CASES[@]}"; do
  IFS='|' read -r name prompt expected_tool pattern <<< "$raw"
  if [[ "$name" != $CASE_PATTERN ]]; then continue; fi
  run_case "$name" "$prompt" "$expected_tool" "$pattern"
done

echo >&2
echo "═══ matrix ═══" >&2
printf '%s\n' "${RESULTS[@]}" | column -t -s '|' >&2
echo >&2
echo "TOTAL=$TOTAL  PASS=$PASS  FAIL=$FAIL" >&2
echo "logs: $RUN_DIR" >&2

# also write final matrix to file for later diffing
{
  echo "═══ PRISM engine smoke run ${RUN_ID} ═══"
  echo "binary: $BIN"
  printf '%s\n' "${RESULTS[@]}" | column -t -s '|'
  echo
  echo "TOTAL=$TOTAL  PASS=$PASS  FAIL=$FAIL"
} > "$RUN_DIR/matrix.txt"

[[ $FAIL -eq 0 ]]
