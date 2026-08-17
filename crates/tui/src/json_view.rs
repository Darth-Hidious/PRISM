//! Human-readable rendering of tool-result JSON for the transcript.
//!
//! Tool results are the substance of the product — the numbers, the sources,
//! the citations — but they arrive as machine JSON and used to be pasted into
//! the transcript verbatim. One `materials_search` produced a wall of escaped
//! strings, 200-character URLs, nine-decimal timings and nested arrays, and
//! the two facts a reader actually wanted were somewhere inside it.
//!
//! This renders the same value as an indented outline: one field per line,
//! long strings elided, big arrays summarised by count, and the whole thing
//! capped so a single tool call can never flood the view.
//!
//! It is deliberately LOSSY, and that is safe for one reason only: the exact
//! payload is still one keypress away in the Activity detail modal
//! (`chatline_detail_json`), which shows the unmodified event. Summarising
//! here without that escape hatch would be hiding data, not presenting it.

use serde_json::Value;

/// Longest string value shown before eliding. Comfortably fits a DOI, a
/// short title or an error sentence; cuts a tracking-parameter URL.
const MAX_STRING: usize = 96;
/// Array elements shown before collapsing to a count.
const MAX_ITEMS: usize = 4;
/// Nesting depth rendered before collapsing to a shape summary.
const MAX_DEPTH: usize = 4;
/// Hard line cap for one tool result.
const MAX_LINES: usize = 32;

/// Render `raw` as a readable outline.
///
/// Anything that is not JSON is returned unchanged — plenty of tools answer
/// with plain prose, and reformatting that would be worse than leaving it.
#[must_use]
pub fn summarize_tool_json(raw: &str) -> String {
    let trimmed = raw.trim();
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return raw.to_string();
    };
    // A bare scalar gains nothing from outlining.
    if !value.is_object() && !value.is_array() {
        return raw.to_string();
    }

    let mut out = Vec::new();
    render(&value, 0, &mut out);
    let truncated = out.len() > MAX_LINES;
    if truncated {
        let hidden = out.len() - MAX_LINES;
        out.truncate(MAX_LINES);
        out.push(format!(
            "… {hidden} more line(s) — open the Activity detail for the full payload"
        ));
    }
    out.join("\n")
}

fn render(value: &Value, depth: usize, out: &mut Vec<String>) {
    if out.len() > MAX_LINES {
        return;
    }
    let pad = "  ".repeat(depth);
    match value {
        Value::Object(map) => {
            if depth >= MAX_DEPTH {
                out.push(format!("{pad}{{{} field(s)}}", map.len()));
                return;
            }
            for (key, child) in map {
                match child {
                    // Scalars share the key's line — the common case, and
                    // what makes the outline scannable.
                    Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                        out.push(format!("{pad}{key}: {}", scalar(child)));
                    }
                    Value::Array(items) if items.is_empty() => {
                        out.push(format!("{pad}{key}: []"));
                    }
                    Value::Object(inner) if inner.is_empty() => {
                        out.push(format!("{pad}{key}: {{}}"));
                    }
                    Value::Array(items) => {
                        out.push(format!("{pad}{key}: [{}]", items.len()));
                        render(child, depth + 1, out);
                    }
                    Value::Object(_) => {
                        out.push(format!("{pad}{key}:"));
                        render(child, depth + 1, out);
                    }
                }
                if out.len() > MAX_LINES {
                    return;
                }
            }
        }
        Value::Array(items) => {
            if depth >= MAX_DEPTH {
                out.push(format!("{pad}[{} item(s)]", items.len()));
                return;
            }
            for item in items.iter().take(MAX_ITEMS) {
                match item {
                    Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                        out.push(format!("{pad}- {}", scalar(item)));
                    }
                    _ => {
                        out.push(format!("{pad}-"));
                        render(item, depth + 1, out);
                    }
                }
                if out.len() > MAX_LINES {
                    return;
                }
            }
            if items.len() > MAX_ITEMS {
                out.push(format!("{pad}… {} more", items.len() - MAX_ITEMS));
            }
        }
        other => out.push(format!("{pad}{}", scalar(other))),
    }
}

/// One scalar, elided if long. Floats lose their noise tail: a latency of
/// `16317.195292` is `16317.2` to a reader and identical in meaning.
fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.as_i64().is_none() && n.as_u64().is_none() => format!("{f:.1}"),
            _ => n.to_string(),
        },
        Value::String(s) => {
            let one_line = s.replace('\n', " ");
            elide(&one_line)
        }
        _ => String::new(),
    }
}

fn elide(s: &str) -> String {
    if s.chars().count() <= MAX_STRING {
        return s.to_string();
    }
    // Char-boundary safe: a byte slice here would panic on any multi-byte
    // character, and tool output carries plenty (µm, °C, — ).
    let head: String = s.chars().take(MAX_STRING).collect();
    format!("{head}… (+{} chars)", s.chars().count() - MAX_STRING)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Non-JSON is returned untouched: many tools answer in prose.
    #[test]
    fn plain_text_is_left_alone() {
        assert_eq!(summarize_tool_json("no results found"), "no results found");
        assert_eq!(summarize_tool_json("42"), "42");
    }

    /// The shape that motivated this: nested objects, a long URL, a
    /// nine-decimal float, and an array longer than the cap.
    #[test]
    fn a_search_result_becomes_scannable() {
        let raw = serde_json::json!({
            "elapsed_ms": 16317.195292_f64,
            "cache_hit": false,
            "results": [
                {"source": "doaj", "title": "Ti-6Al-4V fatigue"},
                {"source": "ntrs", "title": "Alloy review"},
                {"source": "a", "title": "t"},
                {"source": "b", "title": "t"},
                {"source": "c", "title": "t"},
                {"source": "d", "title": "t"}
            ],
            "url": "https://www.ebi.ac.uk/europepmc/webservices/rest/search?format=json&pageSize=5&cursorMark=%2A&query=%28%22yield%20strength%20of%20Ti-6Al-4V%22%29%20AND%20SRC%3APPR"
        })
        .to_string();
        let out = summarize_tool_json(&raw);

        assert!(
            out.contains("elapsed_ms: 16317.2"),
            "float noise trimmed: {out}"
        );
        assert!(out.contains("cache_hit: false"), "{out}");
        assert!(
            out.contains("results: [6]"),
            "arrays announce their size: {out}"
        );
        assert!(out.contains("… 2 more"), "over-cap items collapse: {out}");
        assert!(out.contains("… (+"), "the long URL is elided: {out}");
        assert!(
            !out.contains("SRC%3APPR"),
            "the URL tail must not survive: {out}"
        );
        // Scannable: one field per line, no JSON punctuation noise.
        assert!(!out.contains("\":"), "no raw JSON quoting: {out}");
    }

    /// A pathological payload cannot flood the transcript. There are two
    /// independent brakes and both must hold: a long ARRAY collapses to a
    /// count, and a wide OBJECT hits the line cap.
    #[test]
    fn output_is_capped_by_both_brakes() {
        // Brake 1 — a 500-element array never renders 500 rows.
        let big: Vec<_> = (0..500).map(|i| serde_json::json!({"i": i})).collect();
        let out = summarize_tool_json(&serde_json::json!({ "items": big }).to_string());
        assert!(
            out.lines().count() <= MAX_LINES + 1,
            "capped, got {} lines",
            out.lines().count()
        );
        assert!(
            out.contains("… 496 more"),
            "array collapses to a count: {out}"
        );

        // Brake 2 — an object with more keys than the line cap is truncated
        // AND says where the rest is, so nothing is silently dropped.
        let mut wide = serde_json::Map::new();
        for i in 0..200 {
            wide.insert(format!("field_{i}"), serde_json::json!(i));
        }
        let out = summarize_tool_json(&Value::Object(wide).to_string());
        assert!(
            out.lines().count() <= MAX_LINES + 1,
            "capped, got {} lines",
            out.lines().count()
        );
        assert!(
            out.contains("Activity detail"),
            "truncation must point at the full payload: {out}"
        );
    }

    /// Multi-byte characters are everywhere in tool output (µm, °C, —).
    /// Eliding on a byte index would panic; this pins the char-safe path.
    #[test]
    fn eliding_never_splits_a_multibyte_character() {
        let raw = serde_json::json!({ "note": "µ".repeat(400) }).to_string();
        let out = summarize_tool_json(&raw);
        assert!(out.contains("… (+"), "{out}");
    }
}
