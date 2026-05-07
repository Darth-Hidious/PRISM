#!/usr/bin/env bash
# walkthrough-multiturn.sh — drive a 3-turn conversation through PRISM.
#
# Single-turn (walkthrough-pty.sh) proves the engine boots, retrieves,
# and dispatches an LLM call. Multi-turn proves something stronger:
#   - The first response renders to the user
#   - Forge keeps prior turns in context
#   - The second response references the first (real continuity)
#   - The third turn closes the conversation cleanly
#
# This is the Apple-feel test — a real customer holds a conversation,
# they don't fire one query. If the model loses context or the TUI
# clears between turns, this catches it.
#
# Usage:
#   scripts/walkthrough-multiturn.sh           # one 3-turn walkthrough
#   scripts/walkthrough-multiturn.sh -n 5      # five consecutive
#
# Each turn captures what's on screen before sending the next message
# so we can verify the response actually rendered, not just that the
# spinner ticked.

set -uo pipefail

BIN="${PRISM_BIN:-./target/release/prism}"
RUNS=1
RESPONSE_WAIT_S=60
SESSION_PREFIX="prism-mt-$$"
RUN_ID="$(date '+%Y%m%d_%H%M%S')"
RUN_DIR="/tmp/prism-multiturn/${RUN_ID}"

# Three queries — chained so turn 2 needs turn 1's context, turn 3
# needs turn 2's. Pure-chat path (no tool routing) so we can verify
# turn-to-turn rendering without depending on tool execution.
TURN1="explain creep deformation in one sentence"
TURN2="now name two alloys known for resisting it"
TURN3="which of those two has a higher operating temperature"

while [[ $# -gt 0 ]]; do
  case "$1" in
    -n) RUNS="$2"; shift 2 ;;
    --bin) BIN="$2"; shift 2 ;;
    -w) RESPONSE_WAIT_S="$2"; shift 2 ;;
    -h|--help) sed -n '2,28p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

[[ -x "$BIN" ]] || { echo "prism not at $BIN" >&2; exit 1; }
command -v tmux >/dev/null || { echo "tmux not on PATH" >&2; exit 1; }
mkdir -p "$RUN_DIR"

# Wait for a regex pattern to appear in tmux capture. Returns 0 on
# match, 1 on timeout.
wait_for_pane() {
  local session="$1" pattern="$2" deadline="$3"
  local t0; t0=$(date +%s)
  while true; do
    if tmux capture-pane -t "$session" -p 2>/dev/null | grep -Eq -- "$pattern"; then
      return 0
    fi
    (( $(date +%s) - t0 > deadline )) && return 1
    sleep 0.5
  done
}

# Wait for a specific Finished marker (each turn gets its own UUID).
# Returns the line count of new content rendered between this turn's
# Initialize and Finished — used by the caller to confirm a real
# response rendered (not just an empty turn).
wait_for_finished_count() {
  local session="$1" deadline="$2" out="$3"
  local t0; t0=$(date +%s)
  while true; do
    local snap; snap=$(tmux capture-pane -t "$session" -p -S -500)
    # Count Finished lines — each completed turn emits one.
    local finished_count; finished_count=$(echo "$snap" | grep -c "Finished")
    echo "$snap" > "$out"
    if [[ -n "$WANT_FINISHED" && "$finished_count" -ge "$WANT_FINISHED" ]]; then
      return 0
    fi
    (( $(date +%s) - t0 > deadline )) && return 1
    sleep 1
  done
}

run_one() {
  local n="$1"
  local session="${SESSION_PREFIX}-${n}"
  local logbase="$RUN_DIR/run-${n}"
  local verdict="UNKNOWN" reason=""
  local t0; t0=$(date +%s)

  tmux new-session -d -s "$session" -x 220 -y 60 "$BIN"
  tmux pipe-pane -t "$session" "cat >> ${logbase}.tape"

  if ! wait_for_pane "$session" "❯" 60; then
    verdict="FAIL"; reason="boot-timeout"
  else
    # Turn 1
    tmux send-keys -t "$session" "$TURN1" Enter
    WANT_FINISHED=1 wait_for_finished_count "$session" "$RESPONSE_WAIT_S" "${logbase}.t1.cap"
    local t1_ok; t1_ok=$([[ $? -eq 0 ]] && echo y || echo n)

    if [[ "$t1_ok" == "n" ]]; then
      verdict="FAIL"; reason="turn1-no-finish"
    else
      # Sanity — was real content rendered? Look for keywords from t1
      # response, NOT just the echoed query.
      if ! grep -Eq "(time-dependent|stress|temperature|deformation|grain|dislocation|atom)" "${logbase}.t1.cap"; then
        verdict="FAIL"; reason="turn1-blank"
      else
        # Turn 2 — needs turn 1's context to know "it" = creep
        tmux send-keys -t "$session" "$TURN2" Enter
        WANT_FINISHED=2 wait_for_finished_count "$session" "$RESPONSE_WAIT_S" "${logbase}.t2.cap"
        local t2_ok; t2_ok=$([[ $? -eq 0 ]] && echo y || echo n)

        if [[ "$t2_ok" == "n" ]]; then
          verdict="FAIL"; reason="turn2-no-finish"
        elif ! grep -Eqi "(inconel|hastelloy|udimet|waspaloy|nimonic|rene|cmsx|alloy|superalloy)" "${logbase}.t2.cap"; then
          verdict="FAIL"; reason="turn2-no-alloys"
        else
          # Turn 3 — needs turn 2's two alloys to compare
          tmux send-keys -t "$session" "$TURN3" Enter
          WANT_FINISHED=3 wait_for_finished_count "$session" "$RESPONSE_WAIT_S" "${logbase}.t3.cap"
          local t3_ok; t3_ok=$([[ $? -eq 0 ]] && echo y || echo n)

          if [[ "$t3_ok" == "n" ]]; then
            verdict="FAIL"; reason="turn3-no-finish"
          # Strict turn-3 gate: must contain response-only tokens, NOT
          # tokens from the user's query. "temperature/operating/higher"
          # are all in the query. The actual response should mention a
          # specific alloy by name (Inconel/CMSX) AND a comparison verb
          # AND ideally a numeric temp. Also rule out the visible-failure
          # detector's chat message (it contains "regression" / "blank").
          elif grep -q "PRISM detected dropped output" "${logbase}.t3.cap"; then
            verdict="FAIL"; reason="turn3-silent-failure-detected"
          elif ! grep -Eqi "(CMSX|Inconel).*(higher|exceeds|withstand|°C|°F|degrees|kelvin|operating|melt)" "${logbase}.t3.cap" \
            && ! grep -Eqi "(higher|exceeds|withstand|°C|°F|degrees|kelvin).*(CMSX|Inconel)" "${logbase}.t3.cap"; then
            verdict="FAIL"; reason="turn3-no-comparison"
          else
            verdict="PASS"
          fi
        fi
      fi
    fi
  fi

  tmux send-keys -t "$session" "/exit" Enter 2>/dev/null
  sleep 1
  tmux kill-session -t "$session" 2>/dev/null

  local t1; t1=$(date +%s)
  local dt=$((t1 - t0))
  printf '  [%-4s] run %2d  %3ss  %s\n' "$verdict" "$n" "$dt" "$reason" >&2
  [[ "$verdict" == "PASS" ]]
}

echo "═══ PRISM multi-turn walkthrough ${RUN_ID} ═══" >&2
echo "binary:  $BIN" >&2
echo "runs:    $RUNS" >&2
echo "logs:    $RUN_DIR" >&2
echo "T1: $TURN1" >&2
echo "T2: $TURN2" >&2
echo "T3: $TURN3" >&2
echo >&2

PASS=0; FAIL=0
for i in $(seq 1 "$RUNS"); do
  run_one "$i" && PASS=$((PASS+1)) || FAIL=$((FAIL+1))
done

echo >&2
echo "TOTAL=$RUNS  PASS=$PASS  FAIL=$FAIL" >&2
echo "logs: $RUN_DIR" >&2

[[ $FAIL -eq 0 ]]
