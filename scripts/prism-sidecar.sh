#!/usr/bin/env bash
# PRISM TUI sidecar — run the TUI in a detached tmux session you can watch.
#
# WHY a sidecar: the TUI owns the terminal, so an agent driving it and a human
# watching it cannot share one shell. tmux gives the TUI its own pty; the
# session is the shared surface — you attach to watch, tooling captures panes
# to verify, and neither side steals the other's input.
#
#   ./scripts/prism-sidecar.sh start [scenario]   launch (default: demo mode)
#   ./scripts/prism-sidecar.sh watch              attach and drive it yourself
#   ./scripts/prism-sidecar.sh peek               print current screen, no attach
#   ./scripts/prism-sidecar.sh keys <keys...>     send keystrokes
#   ./scripts/prism-sidecar.sh real               run against the REAL backend
#   ./scripts/prism-sidecar.sh stop               kill the session
#
# Demo mode (`--fake-backend`) is the default on purpose: no login, no network,
# no LLM spend, deterministic frames. `real` is opt-in.
set -euo pipefail

SESSION="${PRISM_SIDECAR_SESSION:-prism}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${PRISM_BIN:-$ROOT/target/release/prism}"
# 120x36 is a deliberate floor: the workspace sidebar collapses below ~100
# columns, so a smaller pane hides the layout the TUI is judged on.
COLS="${PRISM_SIDECAR_COLS:-120}"
ROWS="${PRISM_SIDECAR_ROWS:-36}"

need_binary() {
  if [ ! -x "$BIN" ]; then
    echo "prism binary not found at: $BIN" >&2
    echo "build it first:  cargo build --release -p prism-cli" >&2
    echo "or point PRISM_BIN at one." >&2
    exit 1
  fi
}

running() { tmux has-session -t "$SESSION" 2>/dev/null; }

start() {
  need_binary
  local scenario="${1:-basic_chat}"
  if running; then
    echo "session '$SESSION' already running — 'watch' to attach, 'stop' to kill."
    return 0
  fi
  tmux new-session -d -s "$SESSION" -x "$COLS" -y "$ROWS" \
    "$BIN" tui --fake-backend --scenario "$scenario"
  # Let the first frame render before anyone captures it.
  sleep 2
  echo "PRISM TUI running in tmux session '$SESSION' (${COLS}x${ROWS}, scenario: $scenario)"
  echo
  echo "  watch it:   tmux attach -t $SESSION     (detach with Ctrl-b then d)"
  echo "  peek:       $0 peek"
  echo "  stop:       $0 stop"
}

real() {
  need_binary
  if running; then
    echo "session '$SESSION' already running — 'stop' first." >&2
    exit 1
  fi
  # Real mode spawns `prism backend` and talks to a live LLM. It is opt-in
  # because it costs money and needs credentials the demo path never touches.
  tmux new-session -d -s "$SESSION" -x "$COLS" -y "$ROWS" \
    "$BIN" tui --project-root "$ROOT"
  sleep 3
  echo "PRISM TUI (REAL backend) in tmux session '$SESSION'"
  echo "  watch it:   tmux attach -t $SESSION"
}

watch_it() { running || { echo "no session '$SESSION' — run '$0 start'" >&2; exit 1; }; tmux attach -t "$SESSION"; }

peek() {
  running || { echo "no session '$SESSION' — run '$0 start'" >&2; exit 1; }
  # -p prints to stdout; -e keeps colour escapes so the capture looks like the
  # screen rather than a de-styled approximation.
  tmux capture-pane -t "$SESSION" -p -e
}

keys() {
  running || { echo "no session '$SESSION' — run '$0 start'" >&2; exit 1; }
  tmux send-keys -t "$SESSION" "$@"
  sleep 1
}

stop() {
  running || { echo "no session '$SESSION' to stop."; return 0; }
  tmux kill-session -t "$SESSION"
  echo "stopped '$SESSION'."
}

case "${1:-start}" in
  start) shift || true; start "${1:-basic_chat}" ;;
  real)  real ;;
  watch|attach) watch_it ;;
  peek)  peek ;;
  keys)  shift; keys "$@" ;;
  stop)  stop ;;
  *) echo "usage: $0 {start [scenario]|real|watch|peek|keys <keys>|stop}" >&2; exit 2 ;;
esac
