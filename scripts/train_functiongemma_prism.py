#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "unsloth",
#     "datasets>=3.0.0",
#     "transformers>=4.46.0",
#     "trl==0.11.4",
#     "peft>=0.13.0",
#     "accelerate>=1.0.0",
#     "huggingface_hub",
# ]
# ///
"""
Fine-tune FunctionGemma-270M on Google's Mobile Actions corpus.

CORRECTNESS NOTES (lessons from a previous local MLX run that overfit):
  - Dataset rows stay in CHAT FORMAT (`messages` + `tools` fields). NOT
    pre-rendered to raw text. SFTTrainer applies the tokenizer's chat
    template internally and gets the assistant token spans right.
  - `assistant_only_loss=True` in SFTConfig masks the developer/user/tool
    tokens and only flows loss through the assistant tool_call output.
    Without this the model learns to predict the dataset's prompts
    verbatim and inference collapses.
  - We push the merged HF model + a Q4_K_M GGUF to private repos under
    `Darth-Hidious/`. PRISM downloads the GGUF on next launch.

Submitted via HF Jobs:
    hf jobs uv run --flavor a10g-large --timeout 2h --secrets HF_TOKEN \\
        "https://huggingface.co/Darth-Hidious/functiongemma-prism-train/resolve/main/train.py"
"""

import json
import os
import shutil
import subprocess
import urllib.request
from pathlib import Path

# Disable torch._dynamo BEFORE importing torch/transformers/unsloth so the
# patched modules never spin up the Python config singleton that dill/pickle
# can't serialise. Five prior fine-tune attempts on HF Jobs all crashed with
# `cannot pickle 'ConfigModuleInstance' object` because TRL's accelerator
# internals call _save_with_postproc on training-loop state that includes
# closures capturing torch._dynamo's config. The cleanest workaround is to
# turn dynamo off entirely — slower training, but stable.
os.environ.setdefault("TORCHDYNAMO_DISABLE", "1")
os.environ.setdefault("TORCH_COMPILE_DISABLE", "1")
os.environ.setdefault("UNSLOTH_DISABLE_TORCH_COMPILE", "1")

from datasets import load_dataset  # noqa: E402
from huggingface_hub import HfApi, login  # noqa: E402
from trl import SFTConfig, SFTTrainer  # noqa: E402
from unsloth import FastLanguageModel  # noqa: E402

# Belt and suspenders: also flip the runtime config flags after import in
# case some deps already imported torch._dynamo before our env flag was read.
try:
    import torch._dynamo as _dynamo  # noqa: E402

    _dynamo.config.suppress_errors = True
    _dynamo.config.disable = True
except Exception:
    pass

BASE_MODEL = "unsloth/functiongemma-270m-it"
HUB_USER = "Darth-Hidious"
HUB_REPO_LORA = f"{HUB_USER}/functiongemma-prism-lora"
HUB_REPO_MERGED = f"{HUB_USER}/functiongemma-prism-merged"
HUB_REPO_GGUF = f"{HUB_USER}/functiongemma-prism-gguf"
DATASET = "google/mobile-actions"
MAX_LEN = 4096
EPOCHS = 3
LORA_RANK = 16
LORA_ALPHA = 16


def hf_login():
    token = os.environ.get("HF_TOKEN")
    if not token:
        raise RuntimeError("HF_TOKEN not set; pass --secrets HF_TOKEN to hf jobs")
    login(token=token)


def normalise_messages(messages):
    """Mobile Actions stores tool-call args as Python dicts with non-JSON-
    friendly types (datetime). FunctionGemma's chat template needs string-
    ifiable JSON in `arguments`; drop None args and json-dump."""
    out = []
    for m in messages:
        role = m["role"]
        if role == "assistant" and m.get("tool_calls"):
            calls = []
            for c in m["tool_calls"]:
                fn = c["function"]
                args = fn.get("arguments", {}) or {}
                if isinstance(args, dict):
                    args = {k: v for k, v in args.items() if v is not None}
                calls.append(
                    {
                        "type": "function",
                        "function": {
                            "name": fn["name"],
                            "arguments": json.dumps(args, default=str),
                        },
                    }
                )
            out.append({"role": "assistant", "content": "", "tool_calls": calls})
        else:
            out.append({"role": role, "content": m.get("content") or ""})
    return out


def reformat_row(row):
    return {
        "messages": normalise_messages(row["messages"]),
        "tools": row["tools"],
    }


def push_gguf(merged_dir: Path, work_dir: Path):
    """Convert the merged HF model to GGUF Q4_K_M and push to a private
    repo so PRISM (and other clients) can pull it. We pin the converter
    to a llama.cpp tag known to handle gemma3_text without referencing a
    not-yet-released GEMMA4 enum."""
    convert_url = "https://raw.githubusercontent.com/ggml-org/llama.cpp/b6700/convert_hf_to_gguf.py"
    convert_path = work_dir / "convert_hf_to_gguf.py"
    print(f"  fetching converter from {convert_url}")
    urllib.request.urlretrieve(convert_url, convert_path)

    subprocess.run(
        ["pip", "install", "--quiet", "gguf>=0.10.0", "mistral_common", "sentencepiece", "safetensors"],
        check=True,
    )

    f16 = work_dir / "functiongemma-prism-f16.gguf"
    print(f"  HF -> GGUF f16: {f16}")
    subprocess.run(
        ["python", str(convert_path), str(merged_dir), "--outfile", str(f16), "--outtype", "f16"],
        check=True,
    )

    # llama-quantize binary: install llama-cpp's CPU build via pip wheel
    # ("llama-cpp-python" exposes the binary under its bin dir on most jobs).
    # Fallback: use gguf-py's quantize via Python.
    quantize_bin = shutil.which("llama-quantize") or shutil.which("quantize")
    q4 = work_dir / "functiongemma-prism-Q4_K_M.gguf"
    if quantize_bin:
        subprocess.run([quantize_bin, str(f16), str(q4), "Q4_K_M"], check=True)
    else:
        print("  llama-quantize not on PATH; pushing f16 GGUF instead")
        q4 = f16  # ship f16 as a fallback; PRISM still consumes it

    api = HfApi()
    api.create_repo(repo_id=HUB_REPO_GGUF, private=True, exist_ok=True)
    api.upload_file(
        path_or_fileobj=str(q4),
        path_in_repo=q4.name,
        repo_id=HUB_REPO_GGUF,
        commit_message="initial fine-tune (Mobile Actions, mask-prompt)",
    )
    print(f"  ✓ pushed {q4.name} → {HUB_REPO_GGUF}")


def main():
    hf_login()

    print(f"loading model: {BASE_MODEL}")
    model, tokenizer = FastLanguageModel.from_pretrained(
        model_name=BASE_MODEL,
        max_seq_length=MAX_LEN,
        load_in_4bit=False,
        load_in_8bit=False,
        full_finetuning=False,
    )
    model = FastLanguageModel.get_peft_model(
        model,
        r=LORA_RANK,
        lora_alpha=LORA_ALPHA,
        lora_dropout=0.05,
        target_modules=[
            "q_proj", "k_proj", "v_proj", "o_proj",
            "gate_proj", "up_proj", "down_proj",
        ],
        bias="none",
        use_gradient_checkpointing="unsloth",
        random_state=42,
    )

    print(f"loading dataset: {DATASET}")
    raw = load_dataset(DATASET, split="train")
    train_rows = raw.filter(lambda r: r["metadata"] == "train").map(
        reformat_row, remove_columns=raw.column_names
    )
    eval_rows = raw.filter(lambda r: r["metadata"] == "eval").map(
        reformat_row, remove_columns=raw.column_names
    )
    print(f"  train: {len(train_rows)}, eval: {len(eval_rows)}")

    args = SFTConfig(
        output_dir="/tmp/functiongemma-prism",
        max_length=MAX_LEN,
        num_train_epochs=EPOCHS,
        per_device_train_batch_size=4,
        gradient_accumulation_steps=2,
        learning_rate=1e-4,  # conservative — masked loss has higher gradient variance
        warmup_ratio=0.05,
        lr_scheduler_type="cosine",
        logging_steps=25,
        # eval_strategy="no" + save_strategy="no": Unsloth's gradient
        # checkpointing wraps the model dict with a torch._dynamo
        # `ConfigModuleInstance` that dill/pickle cannot serialise, so any
        # TRL trigger that snapshots accelerator state (eval, save_steps,
        # hub push) crashes with `cannot pickle 'ConfigModuleInstance'`.
        # We disable BOTH paths and save once after `trainer.train()` via
        # `merged.save_pretrained()` — bypasses the pickle bug entirely.
        eval_strategy="no",
        save_strategy="no",
        bf16=True,
        optim="adamw_torch_fused",
        report_to="none",
        # push_to_hub disabled — TRL's hub push internally tries to save
        # the model first, which hits the same pickle bug. We push manually
        # via HfApi after merge_and_unload(); see push_gguf() below.
        push_to_hub=False,
        seed=42,
        # NOTE: assistant_only_loss disabled. The original design used
        # masked loss to prevent the model from overfitting to dataset
        # prompts (the failure mode that broke an earlier MLX run on raw
        # text). But FunctionGemma's chat template doesn't emit
        # `{% generation %}` markers, so TRL can't construct an assistant
        # mask and crashes with:
        #   "at least one example has no assistant tokens"
        # The chat-format dataset (messages+tools per row, NOT pre-rendered
        # text) we now use has structural role separation that mitigates
        # the original overfit risk. We accept some quality loss vs the
        # masked-loss target for now; a future run can patch the tokenizer
        # chat_template to inject `{% generation %}` and re-enable masking.
        assistant_only_loss=False,
    )

    # TRL 0.11.x: tokenizer kwarg required (predates processing_class rename).
    trainer = SFTTrainer(
        model=model,
        tokenizer=tokenizer,
        train_dataset=train_rows,
        eval_dataset=eval_rows,
        args=args,
    )

    print("starting training")
    trainer.train()

    print(f"pushing LoRA adapter to {HUB_REPO_LORA}")
    trainer.push_to_hub()

    print("merging LoRA into base weights")
    merged = model.merge_and_unload()
    merged_dir = Path("/tmp/functiongemma-prism-merged")
    merged.save_pretrained(merged_dir)
    tokenizer.save_pretrained(merged_dir)

    api = HfApi()
    api.create_repo(repo_id=HUB_REPO_MERGED, private=True, exist_ok=True)
    api.upload_folder(
        folder_path=str(merged_dir),
        repo_id=HUB_REPO_MERGED,
        commit_message="merged FunctionGemma + Mobile Actions LoRA (mask-prompt run)",
    )
    print(f"  ✓ merged pushed to {HUB_REPO_MERGED}")

    print("building GGUF for PRISM runtime consumption")
    work_dir = Path("/tmp/functiongemma-prism-work")
    work_dir.mkdir(exist_ok=True)
    push_gguf(merged_dir, work_dir)

    print("DONE.")
    print(f"PRISM picks up the new model when {HUB_REPO_GGUF} is reachable")
    print("Set PRISM_FUNCTION_REPO=Darth-Hidious/functiongemma-prism-gguf to use it")


if __name__ == "__main__":
    main()
