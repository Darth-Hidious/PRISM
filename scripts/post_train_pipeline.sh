#!/usr/bin/env bash
# Post-training pipeline:
#   1. mlx_lm fuse — merge LoRA adapter into base FunctionGemma weights
#   2. convert merged HF model to GGUF Q4_K_M via llama.cpp's convert script
#   3. push merged model + GGUF to private HF repos
#   4. drop the new GGUF into ~/.prism/models/ so PRISM picks it up
#
# Run after the MLX training finishes:
#   bash scripts/post_train_pipeline.sh

set -euo pipefail

PRISM_HOME="$HOME/.prism"
TRAIN_DIR="$PRISM_HOME/training"
ADAPTER="$TRAIN_DIR/adapter"
MERGED="$TRAIN_DIR/merged"
GGUF="$TRAIN_DIR/functiongemma-prism-Q4_K_M.gguf"
PYTHON="/Users/siddharthakovid/Downloads/PRISM/.venv-mlx-gemma4/bin/python3"
HF_USER="Darth-Hidious"
REPO_MERGED="${HF_USER}/functiongemma-prism-merged"
REPO_GGUF="${HF_USER}/functiongemma-prism-gguf"

echo "[1/4] fusing LoRA adapter into base weights"
"$PYTHON" -m mlx_lm fuse \
    --model unsloth/functiongemma-270m-it \
    --adapter-path "$ADAPTER" \
    --save-path "$MERGED" \
    --upload-repo "$REPO_MERGED" \
    --hf-path unsloth/functiongemma-270m-it 2>&1 | tail -10 || {
  # Fall back to local fuse without HF upload if --upload-repo isn't supported
  "$PYTHON" -m mlx_lm fuse \
    --model unsloth/functiongemma-270m-it \
    --adapter-path "$ADAPTER" \
    --save-path "$MERGED"
}
echo "  ✓ merged at $MERGED"

echo "[2/4] downloading llama.cpp convert script if needed"
CONVERT="$TRAIN_DIR/convert_hf_to_gguf.py"
if [ ! -f "$CONVERT" ]; then
  curl -sL -o "$CONVERT" \
    "https://raw.githubusercontent.com/ggml-org/llama.cpp/master/convert_hf_to_gguf.py"
fi

echo "[3/4] converting merged → GGUF (f16) → quantize Q4_K_M"
F16="$TRAIN_DIR/functiongemma-prism-f16.gguf"
"$PYTHON" -m pip install --quiet gguf safetensors numpy sentencepiece transformers >/dev/null 2>&1 || true
"$PYTHON" "$CONVERT" "$MERGED" --outfile "$F16" --outtype f16
/opt/homebrew/bin/llama-quantize "$F16" "$GGUF" Q4_K_M
ls -la "$GGUF"

echo "[4/4] pushing to HF + dropping into ~/.prism/models/"
hf repo create "$REPO_MERGED"  --private --exist-ok 2>&1 | tail -1 || true
hf repo create "$REPO_GGUF"    --private --exist-ok 2>&1 | tail -1 || true
hf upload "$REPO_MERGED" "$MERGED" . --commit-message "merged FunctionGemma + Mobile Actions LoRA" 2>&1 | tail -3
hf upload "$REPO_GGUF"   "$GGUF" "$(basename "$GGUF")" --commit-message "Q4_K_M GGUF" 2>&1 | tail -3

# Swap into PRISM's expected location so the next `prism tui` picks it up.
cp "$GGUF" "$PRISM_HOME/models/functiongemma-270m.gguf"
echo "  ✓ PRISM will use the fine-tuned weights on next launch"

echo "DONE."
echo "Set PRISM_FUNCTION_REPO=$REPO_GGUF to make new installs auto-download the fine-tuned weights."
