// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.

fn main() {
    let sources = [
        "vendor/minja/lexer.cpp",
        "vendor/minja/parser.cpp",
        "vendor/minja/runtime.cpp",
        "vendor/minja/string.cpp",
        "vendor/minja/value.cpp",
        "vendor/minja/unicode.cpp",
        "src/minja_bridge.cpp",
    ];
    for source in sources {
        println!("cargo:rerun-if-changed={source}");
    }
    println!("cargo:rerun-if-changed=vendor/minja");
    println!("cargo:rerun-if-changed=vendor/nlohmann/json.hpp");

    // llama-cpp-sys already links llama.cpp's libcommon, which contains the
    // same Minja symbols. Namespace every vendored symbol to avoid an ODR
    // collision while retaining the exact parser/runtime implementation.
    cc::Build::new()
        .cpp(true)
        .include("vendor")
        .define("jinja", "prism_minja")
        .define("g_jinja_debug", "prism_minja_debug")
        .define(
            "common_parse_utf8_codepoint",
            "prism_minja_parse_utf8_codepoint",
        )
        .define(
            "common_unicode_cpts_to_utf8",
            "prism_minja_unicode_cpts_to_utf8",
        )
        .define(
            "common_unicode_cpt_to_utf8",
            "prism_minja_unicode_cpt_to_utf8",
        )
        .define(
            "common_utf8_sequence_length",
            "prism_minja_utf8_sequence_length",
        )
        .define("common_utf8_is_complete", "prism_minja_utf8_is_complete")
        .define("utf8_parse_result", "prism_minja_utf8_parse_result")
        .files(sources)
        .std("c++17")
        .flag_if_supported("-Wno-unused-function")
        .warnings(false)
        .compile("prism_minja");
}
