#!/usr/bin/env bash
# start-gemma-server.sh — launch the local Gemma 4 server PRISM extracts against.
#
# CONTEXT SIZE IS LOAD-BEARING, NOT A PREFERENCE.
#   PRISM decides whether to read a paper whole or cut it into overlapping
#   windows by asking the server for `n_ctx` (llm/src/lib.rs, probe_context_window)
#   and budgeting `input_byte_budget(n_ctx)` bytes of document per call
#   (ingest/src/batching.rs). Under that budget the paper is ONE window:
#
#       if text.len() <= budget { return vec![(0, text.len())]; }
#
#   At -c 32768 a 60,015-char arXiv paper was cut into 2 windows. At 65536 the
#   same paper is read in a single pass. The model is trained for 262,144
#   (gemma4.context_length in the GGUF), so 65536 is well inside its range —
#   no RoPE extrapolation, no quality cost.
#
#   Raise CTX for longer documents; the ceiling is RAM for the KV cache, not
#   the model. Gemma 4 is hybrid-attention (most layers sliding-window, capped
#   at the window), so KV grows far slower than context.
set -euo pipefail

MODEL_DIR="${GEMMA_DIR:-$HOME/Downloads/gemma4-12b}"
CTX="${CTX:-65536}"
PORT="${PORT:-8081}"
LOG="${LOG:-$HOME/Downloads/llama-server-${CTX}.log}"

[ -f "$MODEL_DIR/gemma-4-12b-it-qat-q4_0.gguf" ] || {
  echo "model not found: $MODEL_DIR/gemma-4-12b-it-qat-q4_0.gguf" >&2; exit 1; }

if lsof -iTCP:"$PORT" -sTCP:LISTEN -P >/dev/null 2>&1; then
  echo "port $PORT already serving; stop it first" >&2; exit 1
fi

# --jinja is required: Gemma 4's chat template needs it or the template render
# fails. --reasoning-budget 0 keeps the extractor from spending output tokens
# thinking, which is billed and not wanted for structured extraction.
nohup /opt/homebrew/bin/llama-server \
  -m "$MODEL_DIR/gemma-4-12b-it-qat-q4_0.gguf" \
  --mmproj "$MODEL_DIR/mmproj-gemma-4-12b-it-qat-q4_0.gguf" \
  --host 127.0.0.1 --port "$PORT" \
  -c "$CTX" --parallel 1 -ngl 999 \
  --jinja --reasoning-budget 0 > "$LOG" 2>&1 &

echo "launched pid $! → log $LOG"
for i in $(seq 1 90); do
  # `|| true` is load-bearing: while the model loads, curl exits 7
  # (connection refused). Under `set -e` + `pipefail` a failed poll would
  # abort this script on the FIRST attempt — i.e. always — so the retry loop
  # must be allowed to see a failure and go round again.
  n=$(curl -s -m 3 "http://127.0.0.1:$PORT/props" 2>/dev/null |
      python3 -c 'import sys,json
try: print(json.load(sys.stdin)["default_generation_settings"]["n_ctx"])
except Exception: pass' 2>/dev/null) || true
  [ -n "$n" ] && { echo "serving with n_ctx=$n after ${i}s"; exit 0; }
  sleep 1
done
echo "server did not report a context window within 90s — check $LOG" >&2
exit 1
