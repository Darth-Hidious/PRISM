#!/usr/bin/env bash
# stress-test-engine.sh — exercise every customer-facing PRISM surface,
# not just the chat-LLM path. Reports pass/fail/skip per command with
# timing.
#
# This complements the chat-driven smoke harness (smoke-engine-tools.sh)
# and the direct-MCP harness (smoke-mcp-direct.py). Those test the LLM
# routing and tool implementations; this one tests the broader product
# surface — node lifecycle, ingest, query, mesh discovery, models,
# workflow listing, marketplace, doctor, etc. — that customers hit.
#
# Read-only by default. Pass --include-side-effects to also exercise
# node up/down, ingest, deploy create/stop. Don't run side-effects
# against production credentials without thinking.

set -uo pipefail

BIN="${PRISM_BIN:-./target/release/prism}"
RUN_ID="$(date '+%Y%m%d_%H%M%S')"
RUN_DIR="/tmp/prism-stress/${RUN_ID}"
TIMEOUT_S=30
INCLUDE_SIDE_EFFECTS=0
GTIMEOUT="$(command -v gtimeout || command -v timeout || true)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    -t) TIMEOUT_S="$2"; shift 2 ;;
    --bin) BIN="$2"; shift 2 ;;
    --include-side-effects) INCLUDE_SIDE_EFFECTS=1; shift ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

if [[ ! -x "$BIN" ]]; then
  echo "prism binary not found at $BIN" >&2
  exit 1
fi

mkdir -p "$RUN_DIR"

# Format: name | description | command | grep-pattern | side-effect?
# Each test runs $BIN with the given args under a timeout. PASS if exit 0
# and the grep pattern matches in stdout. FAIL otherwise.
declare -a TESTS=(
  "version|prism --version|--version|^prism|0"
  "help|prism --help shows command groupings|--help|setup|0"
  "status|prism status returns runtime paths + auth state|status|credentials_present|0"
  "doctor|prism doctor diagnostic snapshot|doctor|llama-server|0"
  "tools_list|prism tools enumerates Python tools|tools|knowledge_search|0"
  "agent_help|prism agent shows pipe-friendly commands|agent|KNOWLEDGE GRAPH|0"
  "workflow_list|prism workflow list shows YAML workflows|workflow list|workflow|0"
  "models_list|prism models list shows hosted MARC27 models|models list|gemini|0"
  "models_search|prism models search 'gemini'|models search gemini|gemini|0"
  "marketplace_search|prism marketplace search alloys|marketplace search alloys|marketplace|0"
  "node_status|prism node status (no node up — should report offline)|node status|node|0"
  "mesh_discover|prism mesh discover (3s mDNS scan)|mesh discover --timeout 3|peers|0"
  "query_platform|prism query --platform 'titanium' --json (real KG hit)|query --platform titanium --json|.|0"
  "query_semantic|prism query --platform --semantic 'creep resistance'|query --platform --semantic 'creep resistance'|.|0"
  "billing|prism billing shows credit balance|billing|credit|0"
)

if [[ $INCLUDE_SIDE_EFFECTS -eq 1 ]]; then
  TESTS+=(
    "node_up|prism node up registers local node|node up|registered|1"
    "node_down|prism node down deregisters|node down|deregistered|1"
  )
fi

PASS=0; FAIL=0; SKIP=0; TOTAL=0
declare -a RESULTS=()

run_one() {
  local name="$1" desc="$2" cmd="$3" pattern="$4" side="$5"
  local out="$RUN_DIR/$name.out"
  local err="$RUN_DIR/$name.err"
  local meta="$RUN_DIR/$name.meta"
  local t0 t1 dt status verdict

  if [[ "$side" == "1" && $INCLUDE_SIDE_EFFECTS -eq 0 ]]; then
    SKIP=$((SKIP+1))
    RESULTS+=("SKIP|$name|--|side-effect")
    printf '  [%-22s] %-26s SKIP (side-effect)\n' "SKIP" "$name" >&2
    return
  fi

  t0=$(perl -MTime::HiRes=time -e 'print int(time*1000)')
  if [[ -n "$GTIMEOUT" ]]; then
    eval "$GTIMEOUT $TIMEOUT_S \"$BIN\" $cmd >\"$out\" 2>\"$err\""
  else
    eval "\"$BIN\" $cmd >\"$out\" 2>\"$err\""
  fi
  status=$?
  t1=$(perl -MTime::HiRes=time -e 'print int(time*1000)')
  dt=$((t1 - t0))

  local found=no
  if grep -Eqi -- "$pattern" "$out" "$err" 2>/dev/null; then
    found=yes
  fi
  if [[ $status -eq 0 && $found == yes ]]; then
    verdict=PASS
    PASS=$((PASS+1))
  elif [[ $status -ne 0 ]]; then
    verdict="FAIL(exit=$status)"
    FAIL=$((FAIL+1))
  else
    verdict="FAIL(no-match)"
    FAIL=$((FAIL+1))
  fi

  printf 'name=%s\nstatus=%s\nverdict=%s\ndt_ms=%s\ncmd=%s\npattern=%s\n' \
    "$name" "$status" "$verdict" "$dt" "$cmd" "$pattern" > "$meta"

  RESULTS+=("$verdict|$name|${dt}ms|$desc")
  TOTAL=$((TOTAL+1))
  printf '  [%-22s] %-26s %6sms\n' "$verdict" "$name" "$dt" >&2
}

echo "═══ PRISM stress-test ${RUN_ID} ═══" >&2
echo "binary: $BIN" >&2
echo "logs:   $RUN_DIR" >&2
echo "side-effects: $([[ $INCLUDE_SIDE_EFFECTS -eq 1 ]] && echo enabled || echo skipped)" >&2
echo >&2

for raw in "${TESTS[@]}"; do
  IFS='|' read -r name desc cmd pattern side <<< "$raw"
  run_one "$name" "$desc" "$cmd" "$pattern" "$side"
done

echo >&2
echo "═══ matrix ═══" >&2
printf '%s\n' "${RESULTS[@]}" | column -t -s '|' >&2
echo >&2
echo "TOTAL=$TOTAL  PASS=$PASS  FAIL=$FAIL  SKIP=$SKIP" >&2
echo "logs: $RUN_DIR" >&2

{
  echo "═══ PRISM stress-test ${RUN_ID} ═══"
  echo "binary: $BIN"
  printf '%s\n' "${RESULTS[@]}" | column -t -s '|'
  echo
  echo "TOTAL=$TOTAL PASS=$PASS FAIL=$FAIL SKIP=$SKIP"
} > "$RUN_DIR/matrix.txt"

[[ $FAIL -eq 0 ]]
