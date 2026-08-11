// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Safe Rust wrapper around the vendored llama.cpp Minja renderer.

use std::ffi::{CString, c_char, c_int};

use anyhow::{Context, Result, bail};

unsafe extern "C" {
    fn prism_minja_render(
        template_source: *const c_char,
        context_json: *const c_char,
        output: *mut *mut c_char,
        output_length: *mut usize,
        error: *mut *mut c_char,
        error_length: *mut usize,
    ) -> c_int;
    fn prism_minja_string_free(value: *mut c_char);
}

struct MinjaString {
    pointer: *mut c_char,
    length: usize,
}

impl MinjaString {
    fn to_string(&self) -> Result<String> {
        if self.pointer.is_null() {
            bail!("Minja returned a null string")
        }
        // SAFETY: the bridge returns an allocation of at least `length`
        // bytes which remains owned by this guard until `drop` calls its
        // paired free API. A length-bearing ABI preserves rendered NUL bytes.
        let bytes = unsafe { std::slice::from_raw_parts(self.pointer.cast::<u8>(), self.length) };
        std::str::from_utf8(bytes)
            .context("Minja returned non-UTF-8 output")
            .map(str::to_owned)
    }
}

impl Drop for MinjaString {
    fn drop(&mut self) {
        if !self.pointer.is_null() {
            // SAFETY: this pointer came from `prism_minja_render` and has not
            // been freed or transferred.
            unsafe { prism_minja_string_free(self.pointer) };
        }
    }
}

/// Render an arbitrary chat-template source with llama.cpp's Minja engine.
/// This is weight-free and performs no I/O or network access.
pub fn render(template_source: &str, context: &serde_json::Value) -> Result<String> {
    let template = CString::new(template_source).context("chat template contains a NUL byte")?;
    let context = CString::new(serde_json::to_vec(context)?)
        .context("chat-template context contains a NUL byte")?;
    let mut output = std::ptr::null_mut();
    let mut output_length = 0;
    let mut error = std::ptr::null_mut();
    let mut error_length = 0;
    // SAFETY: both C strings and both out-pointers remain valid for the call.
    // The returned allocations are immediately placed under RAII guards.
    let status = unsafe {
        prism_minja_render(
            template.as_ptr(),
            context.as_ptr(),
            &mut output,
            &mut output_length,
            &mut error,
            &mut error_length,
        )
    };
    let output = MinjaString {
        pointer: output,
        length: output_length,
    };
    let error = MinjaString {
        pointer: error,
        length: error_length,
    };
    if status != 0 {
        let detail = error
            .to_string()
            .unwrap_or_else(|_| "unknown Minja rendering failure".to_string());
        bail!("Minja rejected the embedded chat template (status {status}): {detail}")
    }
    output.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GEMMA4_TEMPLATE: &str = include_str!("../tests/fixtures/gemma4_chat_template.jinja");

    #[test]
    fn renders_control_flow_filters_and_namespace_state() {
        let template = concat!(
            "{%- set ns = namespace(seen=false) -%}",
            "{{- bos_token -}}",
            "{%- for m in messages -%}",
            "{%- if ns.seen %}|{% endif -%}",
            "{{- m.role | upper }}={{ m.content | trim -}}",
            "{%- set ns.seen=true -%}",
            "{%- endfor -%}",
            "{%- if add_generation_prompt %}|ASSISTANT={% endif -%}",
        );
        let context = serde_json::json!({
            "messages": [
                {"role": "system", "content": " Rules "},
                {"role": "user", "content": "Hello"}
            ],
            "bos_token": "<s>",
            "add_generation_prompt": true
        });
        assert_eq!(
            render(template, &context).unwrap(),
            "<s>SYSTEM=Rules|USER=Hello|ASSISTANT="
        );
    }

    #[test]
    fn reports_template_errors_without_crossing_the_ffi_boundary() {
        let error = render("{{ missing.attribute() }}", &serde_json::json!({}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Minja rejected"), "{error}");
    }

    #[test]
    fn preserves_nul_bytes_rendered_from_context() {
        assert_eq!(
            render("{{ value }}", &serde_json::json!({"value": "a\0b"})).unwrap(),
            "a\0b"
        );
    }

    #[test]
    fn pinned_gemma4_template_matches_conversation_golden() {
        assert_eq!(
            crate::model_artifact::sha256_hex(GEMMA4_TEMPLATE.as_bytes()),
            "ae53464bf3be25802b3a5b37def7fd89667067d7577049b3b2d74c4d8de4c6d4"
        );
        let context = serde_json::json!({
            "messages": [
                {"role": "system", "content": "You are terse."},
                {"role": "user", "content": "Hello"}
            ],
            "tools": [],
            "bos_token": "<bos>",
            "eos_token": "<eos>",
            "enable_thinking": false,
            "preserve_thinking": false,
            "add_generation_prompt": true
        });
        assert_eq!(
            render(GEMMA4_TEMPLATE, &context).unwrap(),
            concat!(
                "<bos><|turn>system\nYou are terse.<turn|>\n",
                "<|turn>user\nHello<turn|>\n",
                "<|turn>model\n<|channel>thought\n<channel|>"
            )
        );
    }

    #[test]
    fn pinned_gemma4_template_renders_full_tool_schema() {
        let context = serde_json::json!({
            "messages": [{"role": "user", "content": "Find IN718"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "description": "Find alloy.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "material": {"type": "string", "description": "Name"}
                        },
                        "required": ["material"]
                    }
                }
            }],
            "bos_token": "<bos>",
            "eos_token": "<eos>",
            "enable_thinking": false,
            "preserve_thinking": false,
            "add_generation_prompt": true
        });
        let actual = render(GEMMA4_TEMPLATE, &context).unwrap();
        assert_eq!(
            actual,
            concat!(
                "<bos><|turn>system\n",
                "<|tool>declaration:lookup{description:<|\"|>Find alloy.<|\"|>,",
                "parameters:{properties:{material:{description:<|\"|>Name<|\"|>,",
                "type:<|\"|>STRING<|\"|>}},required:[<|\"|>material<|\"|>],",
                "type:<|\"|>OBJECT<|\"|>}}<tool|><turn|>\n",
                "<|turn>user\nFind IN718<turn|>\n",
                "<|turn>model\n<|channel>thought\n<channel|>"
            )
        );
    }
}
