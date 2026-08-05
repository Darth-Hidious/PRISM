# Embedded GGUF inference feasibility

Phase 1 was completed before implementation on 2026-08-05. This record separates measured facts from design decisions.

## Existing port/adapter shape

`prism-embed` is the pattern to mirror:

1. A small domain port, `EmbedBackend`, owns the operations and capability metadata.
2. `NativeOnnx` and `OpenAiCompat` are adapters behind that port.
3. Selection is explicit (`native`, `openai`, or `off`) and construction returns one backend to callers.
4. Native CPU work leaves Tokio worker threads through `spawn_blocking`.
5. Native weights live under `~/.prism/models/embed`; adapter failures are actionable and never panic.

Generation differs in one important policy: an unavailable embedding backend may degrade to keyword search, but an explicitly selected local generation backend must return an error. It must never fall back to HTTP.

## Runtime choice

Chosen runtime: [`llama-cpp-2` 0.1.154](https://crates.io/crates/llama-cpp-2/0.1.154), behind an opt-in Cargo feature.

| Runtime | Build/architecture assessment | GGUF and hardware | Licence | Decision |
|---|---|---|---|---|
| `llama-cpp-2` | Thin Rust bindings around llama.cpp. It requires clang/bindgen and compiles C/C++, but adds no serving process or scheduler. | GGUF is llama.cpp's native format. The crate enables Metal on Apple Silicon and exposes a CUDA feature; CPU remains available on Linux. | Wrapper: MIT OR Apache-2.0; bundled llama.cpp/ggml: MIT. | **Chosen.** Widest GGUF/model compatibility and the smallest architecture that meets the requirement. |
| Candle | `candle-core` is a lower-level tensor runtime. PRISM would own model-specific graph support, chat templates, tokenization, sampling, and cache policy. | The ecosystem has a GGUF parser and Metal/CUDA features, but GGUF generation remains model-implementation-specific rather than llama.cpp-compatible by construction. | MIT OR Apache-2.0. | Rejected for this first adapter: lower dependency-level build weight would be offset by substantially more PRISM inference code and narrower model coverage. |
| `mistralrs` | A full inference engine with model builders, scheduling, constraints, adapters, an agent layer, and tool APIs. Those are useful features, but duplicate more of PRISM's harness. | Its public API advertises local GGUF builders, streaming, tools, Metal, and CUDA. | MIT. | Rejected for this first adapter: materially broader stack than the required single-model port. Revisit if native batching or runtime-level constrained tool calls become requirements. |

Candidate metadata was inspected with `cargo info`; Candle and mistral.rs were not built, so their build time and disk cost are **UNVERIFIED**.

## Measured build cost

The exact selected dependency configuration was built alone on Apple Silicon macOS with Rust 1.92.0:

```toml
[dependencies]
llama-cpp-2 = { version = "0.1.154", default-features = false }
```

Command:

```bash
/usr/bin/time -lp cargo build < /dev/null
```

Observed result:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 34.53s
real 34.54
user 184.55
sys 23.37
maximum resident set size 541409280
build_exit_code=0
```

Disk evidence:

```text
before: / available 8.2 GiB; isolated crate 8.0 KiB; Cargo registry 2.0 GiB
after:  / available 7.9 GiB; isolated crate 256 MiB; Cargo registry 2.1 GiB
```

Conclusion: approximately 0.3 GiB reported filesystem delta and 34.54 seconds is acceptable for an opt-in feature, but inappropriate for every default build. Default builds must not compile llama.cpp.

## Hard parts and phase-2 boundary

- **KV cache:** each request needs a correctly sized context and cache. The smallest correct implementation creates a fresh context per request while sharing loaded immutable weights. Cross-turn cache reuse is an optimization, not required for correctness, and is unsafe without exact prompt-prefix accounting.
- **Tokenizer and chat template:** tokenization and the default chat template must come from GGUF metadata through llama.cpp. A separately downloaded Hugging Face tokenizer must not be guessed or required.
- **Streaming:** native generation is synchronous and CPU/GPU-bound. It needs a blocking worker plus a channel so async callers receive decoded pieces as they are produced.
- **Cancellation:** dropping the async request does not stop `spawn_blocking`. A shared cancellation flag must be set by a drop guard and checked between prompt batches and generated tokens.
- **Context reporting:** GGUF metadata exposes the trained context length. It should populate `LlmConfig.context_window` when the configured file is present; runtime context allocation must still cap this to a conservative/operator-configurable amount to avoid an excessive KV cache.
- **Tool calling:** llama.cpp can run models trained for tool calling, but the selected Rust wrapper exposes the low-level model chat-template API, not llama.cpp server's higher-level tool-template/parser pipeline. The first adapter must therefore reject non-empty tool sets explicitly rather than silently omit them or fabricate compatibility. Native tool calling remains **unsupported and UNVERIFIED** until PRISM can render and parse each GGUF's tool template with equivalent semantics.
- **Cancellation during model load:** token-by-token generation can be cooperatively cancelled. A model mmap/load already inside llama.cpp cannot be interrupted safely through this wrapper; cancellation takes effect immediately after that call returns.
