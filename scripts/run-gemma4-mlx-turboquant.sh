#!/bin/sh
set -eu

ROOT="/Users/siddharthakovid/Downloads/PRISM"
PYTHON_BIN="$ROOT/.venv-mlx-gemma4/bin/python"
MODEL_DIR="$ROOT/models/gemma-4-26b-a4b-it-mxfp4"

PROMPT="${1:-Reply with exactly: READY}"
SHIFTED=0
if [ "$#" -gt 0 ]; then
  SHIFTED=1
  shift
fi

exec "$PYTHON_BIN" -m mlx_vlm generate \
  --model "$MODEL_DIR" \
  --prompt "$PROMPT" \
  --max-tokens "${MAX_TOKENS:-128}" \
  --kv-bits "${KV_BITS:-3.5}" \
  --kv-group-size "${KV_GROUP_SIZE:-32}" \
  --kv-quant-scheme turboquant \
  "$@"
