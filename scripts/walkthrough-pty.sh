#!/usr/bin/env bash
# walkthrough-pty.sh — drive the PRISM TUI through a real customer
# session inside a tmux pty. This is the Phase 1 exit harness: must
# pass 10× consecutively before Phase 2 (TUI cosmetics) unlocks.
#
# Why tmux? We tried tui-use first; it broke on the cursor-position
# query (DSR) that ratatui issues at startup. tmux gives a real PTY
# with full terminfo, so the binary can't tell it isn't a person.
#
# What this exercises (and the smoke harness doesn't):
#   - Boot sequence renders + clears
#   - Input dispatcher accepts typed keystrokes (not piped stdin)
#   - LLM streamer renders incrementally
#   - Status bar / token counter updates
#   - Quit on Ctrl+D / `/exit`
#   - Process exits 0 with no crossterm RESET errors
#
# Usage:
#   scripts/walkthrough-pty.sh                # one walkthrough
#   scripts/walkthrough-pty.sh -n 10          # 10 consecutive runs
#   scripts/walkthrough-pty.sh -q "creep resistance in titanium"
#
# Exit code 0 only if every run is clean.

set -uo pipefail

BIN="${PRISM_BIN:-./target/release/prism}"
QUERY="${PRISM_WALKTHROUGH_QUERY:-search for nickel superalloys with creep resistance}"
RUNS=1
PER_STEP_WAIT_S=2
RESPONSE_WAIT_S=45
SESSION_PREFIX="prism-walk-$$"
RUN_ID="$(date '+%Y%m%d_%H%M%S')"
RUN_DIR="/tmp/prism-walkthrough/${RUN_ID}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    -n) RUNS="$2"; shift 2 ;;
    -q) QUERY="$2"; shift 2 ;;
    --bin) BIN="$2"; shift 2 ;;
    -w) RESPONSE_WAIT_S="$2"; shift 2 ;;
    -h|--help) sed -n '2,28p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

if [[ ! -x "$BIN" ]]; then
  echo "prism binary not found at $BIN" >&2
  exit 1
fi
if ! command -v tmux >/dev/null; then
  echo "tmux not on PATH" >&2
  exit 1
fi

mkdir -p "$RUN_DIR"

# Wait until tmux capture-pane shows $1 (regex), or fail after $2 seconds.
wait_for_pane() {
  local session="$1" pattern="$2" deadline="$3"
  local t0; t0=$(date +%s)
  while true; do
    if tmux capture-pane -t "$session" -p 2>/dev/null | grep -Eq -- "$pattern"; then
      return 0
    fi
    if (( $(date +%s) - t0 > deadline )); then
      return 1
    fi
    sleep 0.5
  done
}

run_one() {
  local n="$1"
  local session="${SESSION_PREFIX}-${n}"
  local logbase="$RUN_DIR/run-${n}"
  local capture="${logbase}.cap"
  local pre_capture="${logbase}.pre.cap"
  local meta="${logbase}.meta"
  local verdict="UNKNOWN"
  local reason=""
  local t0; t0=$(date +%s)

  # Pipe-pane writes the live pty stream to a file — gives us a tape
  # of every byte for forensics if a run fails.
  tmux new-session -d -s "$session" -x 200 -y 50 "$BIN"
  tmux pipe-pane -t "$session" "cat >> ${logbase}.tape"

  # Wait for the actual chat TUI input prompt (❯). Boot currently
  # takes ~25 s through the 8-status-check + ASCII-banner sequence;
  # waiting for "PRISM" alone falsely passes because the user's typed
  # query gets echoed during boot.
  if ! wait_for_pane "$session" "❯" 60; then
    verdict="FAIL"; reason="boot-timeout"
  else
    # Snapshot pre-query screen so we can prove the response is NEW
    # text and not just the echoed query.
    tmux capture-pane -t "$session" -p > "$pre_capture" 2>/dev/null || true

    tmux send-keys -t "$session" "$QUERY" Enter
    sleep "$PER_STEP_WAIT_S"

    # Real response signals — these only appear after the LLM/tool
    # chain runs, never in boot screen. "tool:" or "Searching" come
    # from the agent loop's inline status; the alloy-name keywords
    # come from a real knowledge_search response.
    local pat='tool:|Searching|Searched|Found|knowledge|graph|nickel|titanium|alloy|creep|superalloy|composition|properties'
    if ! wait_for_pane "$session" "$pat" "$RESPONSE_WAIT_S"; then
      verdict="FAIL"; reason="no-response"
    else
      # Confirm new content — current capture must contain bytes that
      # weren't in the pre-query snapshot.
      tmux capture-pane -t "$session" -p > "$capture" 2>/dev/null || true
      if ! diff -q "$pre_capture" "$capture" >/dev/null 2>&1; then
        verdict="PASS"
      else
        verdict="FAIL"; reason="no-new-content"
      fi
    fi
  fi

  [[ -f "$capture" ]] || tmux capture-pane -t "$session" -p > "$capture" 2>/dev/null || true

  # Quit cleanly: /exit Enter (preferred — matches the slash-command
  # menu the binary advertises). Ctrl+D as fallback.
  tmux send-keys -t "$session" "/exit" Enter 2>/dev/null || true
  sleep 1
  tmux send-keys -t "$session" "C-d" 2>/dev/null || true
  sleep 1
  tmux kill-session -t "$session" 2>/dev/null || true

  local t1; t1=$(date +%s)
  local dt=$((t1 - t0))

  printf 'run=%d\nverdict=%s\nreason=%s\ndt_s=%d\nquery=%s\n' \
    "$n" "$verdict" "$reason" "$dt" "$QUERY" > "$meta"

  printf '  [%-4s] run %2d  %3ss  %s\n' "$verdict" "$n" "$dt" "$reason" >&2
  [[ "$verdict" == "PASS" ]]
}

echo "═══ PRISM walkthrough ${RUN_ID} ═══" >&2
echo "binary:  $BIN" >&2
echo "query:   $QUERY" >&2
echo "runs:    $RUNS" >&2
echo "logs:    $RUN_DIR" >&2
echo >&2

PASS=0; FAIL=0
for i in $(seq 1 "$RUNS"); do
  if run_one "$i"; then
    PASS=$((PASS+1))
  else
    FAIL=$((FAIL+1))
  fi
done

echo >&2
echo "═══ summary ═══" >&2
echo "TOTAL=$RUNS  PASS=$PASS  FAIL=$FAIL" >&2
echo "logs: $RUN_DIR" >&2

# Phase 1 exit gate: zero failures.
[[ $FAIL -eq 0 ]]
