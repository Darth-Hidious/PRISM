// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
// Thin C ABI around the vendored llama.cpp Minja implementation.

#include "../vendor/minja/lexer.h"
#include "../vendor/minja/parser.h"
#include "../vendor/minja/runtime.h"
#include "../vendor/minja/value.h"

#include <cstdlib>
#include <cstring>
#include <exception>
#include <nlohmann/json.hpp>
#include <string>
#include <string_view>

namespace {

char * duplicate_string(const std::string_view value, std::size_t * length) {
    auto * result = static_cast<char *>(std::malloc(value.size() + 1));
    if (result == nullptr) {
        return nullptr;
    }
    std::memcpy(result, value.data(), value.size());
    result[value.size()] = '\0';
    *length = value.size();
    return result;
}

} // namespace

extern "C" int prism_minja_render(
    const char * template_source,
    const char * context_json,
    char ** output,
    std::size_t * output_length,
    char ** error,
    std::size_t * error_length) {
    if (output == nullptr || output_length == nullptr || error == nullptr || error_length == nullptr) {
        return 1;
    }
    *output = nullptr;
    *output_length = 0;
    *error = nullptr;
    *error_length = 0;
    if (template_source == nullptr || context_json == nullptr) {
        *error = duplicate_string("template source and context JSON are required", error_length);
        return 1;
    }

    try {
        const std::string source(template_source);
        prism_minja::lexer lexer;
        auto lexed = lexer.tokenize(source);
        auto program = prism_minja::parse_from_tokens(lexed);
        prism_minja::context context(lexed.source);
        const auto values = nlohmann::ordered_json::parse(context_json);
        prism_minja::global_from_json(context, values, true);
        prism_minja::runtime runtime(context);
        const auto rendered = runtime.execute(program);
        const auto parts = prism_minja::runtime::gather_string_parts(rendered);
        *output = duplicate_string(parts->as_string().str(), output_length);
        if (*output == nullptr) {
            *error = duplicate_string("failed to allocate rendered template output", error_length);
            return 3;
        }
        return 0;
    } catch (const std::exception & exception) {
        *error = duplicate_string(exception.what(), error_length);
        return 2;
    } catch (...) {
        *error = duplicate_string("unknown Minja rendering failure", error_length);
        return 2;
    }
}

extern "C" void prism_minja_string_free(char * value) {
    std::free(value);
}
