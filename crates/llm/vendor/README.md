# Vendored Minja snapshot

`minja/` and `nlohmann/json.hpp` are the template engine sources shipped by
`llama-cpp-sys-2` 0.1.154 (crate source commit
`bed81ad4ab1a6c904b11d425608e50f976d8ea62`), the exact llama.cpp dependency
used by this crate.
They are vendored because llama.cpp's public `llama_chat_apply_template` C API
only recognizes a fixed list of templates; it does not execute arbitrary
Jinja. PRISM needs the real Minja path for the chat template embedded in a
GGUF and for weight-free golden tests.

The build namespaces these sources as `prism_minja` so they do not collide
with the same symbols already present in llama.cpp's `libcommon`.
The two `string.cpp` include paths are flattened to match this vendor layout;
the implementation is otherwise unchanged.

- llama.cpp Minja: MIT, see `LLAMA_CPP_LICENSE`.
- nlohmann/json: MIT, see `NLOHMANN_JSON_LICENSE`.
