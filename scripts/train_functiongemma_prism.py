#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.10"
# dependencies = [
#     # PIN to the exact stack from Unsloth's official FunctionGemma notebook
#     # https://github.com/unslothai/notebooks/blob/main/nb/FunctionGemma_(270M)-Mobile-Actions.ipynb
#     #
#     # Smoke runs 69fca0f3 + 69fca240 proved the unpinned path doesn't work:
#     # left to its own devices uv pulled transformers 5.5.0 + latest TRL,
#     # which broke at SFTTrainer construction (tokenizer→processing_class
#     # rename) and then again at trainer.train() (entropy_from_logits hit
#     # outputs.logits as a function instead of a tensor — TRL 0.23+ lazy
#     # logits change).
#     #
#     # The notebook pins solve both — transformers 4.56.2 still has tensor
#     # logits, TRL 0.22.2 still accepts the new processing_class kwarg.
#     "transformers==4.56.2",
#     "trl==0.22.2",
#     "unsloth",
#     "datasets==4.3.0",
#     "huggingface_hub>=0.34.0",
#     "hf_transfer",
#     "peft",
#     "sentencepiece",
#     "protobuf",
#     "accelerate",
#     "bitsandbytes",
# ]
# ///
"""
Fine-tune FunctionGemma-270M on Google's Mobile Actions corpus, then push
the merged model + a Q4_K_M GGUF to Darth-Hidious/functiongemma-prism-gguf
so PRISM picks it up on next launch.

This rewrite tracks the official Unsloth notebook as closely as possible:
  https://colab.research.google.com/github/unslothai/notebooks/blob/main/nb/FunctionGemma_(270M)-Mobile-Actions.ipynb

Key correctness commitments (lessons from 10 failed prior runs):
  - load_in_16bit=True (Unsloth's recommended FunctionGemma path; was False)
  - Pre-rendering with tokenizer.apply_chat_template + dataset_text_field="text"
    (was passing raw messages+tools, which only worked under TRL's
    deprecated assistant_only_loss path)
  - train_on_responses_only WRAPS the trainer post-construction with
    explicit instruction_part / response_part markers — this is Unsloth's
    blessed loss-masking path and replaces TRL's broken
    assistant_only_loss=True for chat templates without {% generation %}
  - optim="adamw_8bit" (matches notebook; bitsandbytes optimizer)
  - learning_rate=2e-4 (matches notebook)
  - No TRL/transformers/peft pin — let Unsloth bring in versions it tested

Submitted via HF Jobs (one shot, after local --smoke validation):
    hf jobs uv run --flavor a10g-large --timeout 2h --secrets HF_TOKEN \\
        scripts/train_functiongemma_prism.py
"""

import argparse
import os
import shutil
import subprocess
import sys
import urllib.request
from pathlib import Path

# Disable torch._dynamo BEFORE importing torch/transformers/unsloth so the
# Unsloth gradient-checkpointing wrapper never captures torch._dynamo's
# config singleton in a closure (multiple early runs failed with
# "cannot pickle 'ConfigModuleInstance' object" until this).
os.environ.setdefault("TORCHDYNAMO_DISABLE", "1")
os.environ.setdefault("TORCH_COMPILE_DISABLE", "1")
os.environ.setdefault("UNSLOTH_DISABLE_TORCH_COMPILE", "1")
# Smoke v3 (69fca477) failed in flex_attention's _validate_sdpa_input
# with q/k=fp32 vs v=bf16. The Unsloth gemma flex_attention path on
# torch 2.10 + a10g doesn't reliably keep dtypes in lockstep. SDPA
# (torch's standard scaled-dot-product) is bf16-clean on the same
# hardware and Unsloth supports it as the fallback path.
os.environ.setdefault("UNSLOTH_DISABLE_FLEX_ATTENTION", "1")
# Smoke v4 (69fca55f) failed at trainer.train() with:
#   NotImplementedError: Unsloth: Logits are empty from 2024.11 onwards.
# Unsloth 2026.5.2 deliberately drops logits to save VRAM during the
# forward pass, but TRL 0.22.2's compute_loss still references them
# for per-token entropy. The Unsloth error message itself documents
# the fix: re-enable logit return BEFORE trainer.train(). MUST be set
# before any unsloth import for the patcher to honour it.
os.environ.setdefault("UNSLOTH_RETURN_LOGITS", "1")

from datasets import load_dataset  # noqa: E402
from huggingface_hub import HfApi, login  # noqa: E402

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


def hf_login() -> None:
    token = os.environ.get("HF_TOKEN")
    if not token:
        raise RuntimeError("HF_TOKEN not set; pass --secrets HF_TOKEN to hf jobs")
    login(token=token)


def build_dataset(tokenizer, smoke: bool = False):
    """Render Mobile Actions chat-format rows to a flat `text` field via
    `tokenizer.apply_chat_template`. Matches the Unsloth notebook's
    `process_dataset` exactly so the assistant-token spans line up with
    `train_on_responses_only`'s `<start_of_turn>model\\n` marker."""
    raw = load_dataset(DATASET, split="train")

    def _process(row):
        text = tokenizer.apply_chat_template(
            row["messages"],
            tools=row["tools"],
            tokenize=False,
            add_generation_prompt=False,
        )
        return {"text": text}

    train_split = raw.filter(lambda r: r["metadata"] == "train")
    eval_split = raw.filter(lambda r: r["metadata"] == "eval")
    if smoke:
        # Local smoke: take 5 rows from each split to validate end-to-end
        # without spending real compute.
        train_split = train_split.select(range(min(5, len(train_split))))
        eval_split = eval_split.select(range(min(5, len(eval_split))))

    train_rows = train_split.map(
        _process, remove_columns=raw.column_names, num_proc=1
    )
    eval_rows = eval_split.map(
        _process, remove_columns=raw.column_names, num_proc=1
    )
    return train_rows, eval_rows


def push_gguf(merged_dir: Path, work_dir: Path) -> None:
    """Convert the merged HF model to GGUF Q4_K_M and push to a private
    repo so PRISM (and other clients) can pull it. Pinned converter at
    llama.cpp b6700 — known to handle gemma3_text without referencing a
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
        commit_message="initial fine-tune (Mobile Actions, train_on_responses_only)",
    )
    print(f"  ✓ pushed {q4.name} → {HUB_REPO_GGUF}")


def main(smoke: bool = False, dry_run: bool = False) -> None:
    """Train. With `smoke=True`, take only 5 train + 5 eval rows so the
    whole pipeline can be exercised on cheap hardware. With `dry_run=True`,
    skip the actual `trainer.train()` call — useful to confirm the script
    imports / loads / tokenizes / wraps the trainer cleanly without
    spending GPU time."""

    if not smoke:
        hf_login()

    # Imports here (not module-level) so a CPU-only smoke that doesn't
    # have GPU/CUDA can at least exercise the dataset-prep half of the
    # pipeline before hitting the unsloth.FastLanguageModel import.
    print("importing unsloth (requires CUDA)...")
    from trl import SFTConfig, SFTTrainer  # noqa: E402
    from unsloth import FastLanguageModel  # noqa: E402
    from unsloth.chat_templates import train_on_responses_only  # noqa: E402

    # Belt and suspenders against torch._dynamo pickle bug.
    try:
        import torch._dynamo as _dynamo  # noqa: E402
        _dynamo.config.suppress_errors = True
        _dynamo.config.disable = True
    except Exception:
        pass

    print(f"loading model: {BASE_MODEL}")
    model, tokenizer = FastLanguageModel.from_pretrained(
        model_name=BASE_MODEL,
        max_seq_length=MAX_LEN,
        load_in_4bit=False,
        load_in_8bit=False,
        load_in_16bit=True,         # ← MATCHES NOTEBOOK
        full_finetuning=False,
        # Force SDPA over flex_attention. Smoke v3 hit a dtype mismatch
        # in flex_attention; SDPA is the bf16-stable fallback that
        # Unsloth still has fast paths for on Gemma3.
        attn_implementation="sdpa",
    )
    model = FastLanguageModel.get_peft_model(
        model,
        r=LORA_RANK,
        target_modules=[
            "q_proj", "k_proj", "v_proj", "o_proj",
            "gate_proj", "up_proj", "down_proj",
        ],
        lora_alpha=LORA_ALPHA,
        lora_dropout=0,             # notebook uses 0
        bias="none",
        use_gradient_checkpointing="unsloth",
        random_state=3407,          # match notebook
        use_rslora=False,
        loftq_config=None,
    )

    print(f"loading dataset: {DATASET} (smoke={smoke})")
    train_rows, eval_rows = build_dataset(tokenizer, smoke=smoke)
    print(f"  train: {len(train_rows)}, eval: {len(eval_rows)}")
    print(f"  example text (first 200 chars): {train_rows[0]['text'][:200]!r}")

    args = SFTConfig(
        output_dir="/tmp/functiongemma-prism",
        dataset_text_field="text",
        max_length=MAX_LEN,
        num_train_epochs=EPOCHS if not smoke else 1,
        max_steps=10 if smoke else -1,
        per_device_train_batch_size=4,
        gradient_accumulation_steps=2,
        warmup_steps=5,
        learning_rate=2e-4,         # ← MATCHES NOTEBOOK
        logging_steps=1 if smoke else 10,
        eval_strategy="no",         # eval triggers accelerator.save_state which pickles dynamo state
        save_strategy="no",         # same; we save manually after train
        push_to_hub=False,          # same; we push manually after merge
        bf16=True,
        optim="adamw_8bit",         # ← MATCHES NOTEBOOK
        weight_decay=0.001,
        lr_scheduler_type="linear",
        seed=3407,
        report_to="none",
    )

    # TRL ≥0.18 renamed `tokenizer=` → `processing_class=`. The Unsloth
    # notebook screenshot still shows the old kwarg, but the unpinned
    # latest TRL that Unsloth pulls in rejects it with TypeError. Smoke
    # run 69fca0f3 caught this at the SFTTrainer construction step.
    trainer = SFTTrainer(
        model=model,
        processing_class=tokenizer,
        train_dataset=train_rows,
        eval_dataset=eval_rows,
        args=args,
    )

    # Unsloth's blessed loss-masking path. Replaces TRL's
    # assistant_only_loss=True (which crashed on chat templates without
    # `{% generation %}` markers — FunctionGemma's template).
    trainer = train_on_responses_only(
        trainer,
        instruction_part="<start_of_turn>user\n",
        response_part="<start_of_turn>model\n",
    )

    if dry_run:
        print("DRY RUN: trainer constructed cleanly, skipping .train()")
        return

    print("starting training")
    trainer.train()

    if smoke:
        # Smoke runs validate the full training pipeline (imports, model
        # load, dataset, trainer construct, train_on_responses_only wrap,
        # 10 actual training steps) without polluting the target repo
        # with a 10-step undertrained model.
        print("SMOKE OK: trained 10 steps without errors, skipping merge/push")
        return

    print(f"merging LoRA into base weights")
    merged = model.merge_and_unload()
    merged_dir = Path("/tmp/functiongemma-prism-merged")
    merged.save_pretrained(merged_dir)
    tokenizer.save_pretrained(merged_dir)

    api = HfApi()
    api.create_repo(repo_id=HUB_REPO_MERGED, private=True, exist_ok=True)
    api.upload_folder(
        folder_path=str(merged_dir),
        repo_id=HUB_REPO_MERGED,
        commit_message="merged FunctionGemma + Mobile Actions (train_on_responses_only)",
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
    parser = argparse.ArgumentParser()
    parser.add_argument("--smoke", action="store_true",
                        help="Tiny dataset (5 rows) + 10 max_steps. For local validation.")
    parser.add_argument("--dry-run", action="store_true",
                        help="Build dataset + trainer; skip .train(). Validates imports/data only.")
    ns = parser.parse_args()
    main(smoke=ns.smoke, dry_run=ns.dry_run)
