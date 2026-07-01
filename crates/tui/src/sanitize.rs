//! Sanitizer for terminal control sequences.
//!
//! Model output, tool stdout/stderr, backend error messages, and pasted
//! user input can contain ANSI escape sequences (CSI color, cursor
//! movement, OSC terminal-title, DCS, hyperlinks) and C0/C1 control
//! characters that are dangerous when rendered by Ratatui/Crossterm.
//!
//! [`sanitize_for_render`] strips ANSI escapes via the
//! `strip-ansi-escapes` crate, then filters remaining C0/C1 control
//! characters, keeping only `\n` (newlines) and replacing `\t` (tabs)
//! with four spaces.  Normal Unicode — CJK, combining marks, math
//! symbols, emoji — is preserved.
//!
//! ## Design
//!
//! Sanitize at TUI state ingress (in `App`'s message helpers) so the
//! render path stays pure and never receives raw terminal control
//! sequences.  This avoids double-sanitization: the helpers are the
//! single chokepoint through which all visible text enters `ChatLine`.

/// Strip ANSI escape sequences and unsafe control characters from text
/// before it enters `App` visible state.
///
/// What is removed:
/// - ANSI CSI sequences (`\x1b[...m`, cursor moves, clears)
/// - ANSI OSC sequences (`\x1b]...BEL` or `\x1b]...ST`)
/// - ANSI DCS and other ESC-initiated sequences
/// - C0 control chars except `\n` (kept) and `\t` (→ 4 spaces)
/// - C1 control chars (`\u{0080}`–`\u{009f}`)
/// - Specifically: BEL `\x07`, backspace `\x08`, CR `\x0d`, DEL `\x7f`
///
/// What is preserved:
/// - All non-control Unicode (CJK, emoji, combining marks, math, etc.)
/// - Newlines `\n`
/// - Tabs converted to 4 spaces (deterministic layout)
pub fn sanitize_for_render(input: &str) -> String {
    // Phase 1: convert tabs to 4 spaces BEFORE strip_str, because
    // strip_str removes \t (treats it as a control character).  We
    // want tabs to become spaces, not disappear.
    let tab_expanded: String = input.replace('\t', "    ");

    // Phase 2: strip ANSI escape sequences (CSI, OSC, DCS, etc.)
    let stripped = strip_ansi_escapes::strip_str(&tab_expanded);

    // Phase 3: filter remaining C0/C1 control characters.
    // Keep `\n`, drop everything else that is a control char.  C1
    // controls (\u{0080}–\u{009f}) are also dropped —
    // `char::is_control()` covers both C0 and C1 ranges.
    let mut result = String::with_capacity(stripped.len());
    for c in stripped.chars() {
        if c == '\n' {
            result.push('\n');
        } else if c.is_control() {
            // Drop all other control characters (C0 + C1).
            // This catches BEL, BS, CR, DEL, ESC (if any survived
            // strip_str), and C1 controls.
        } else {
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_color_escape() {
        assert_eq!(sanitize_for_render("\x1b[31mred\x1b[0m"), "red");
    }

    #[test]
    fn strips_cursor_movement_escape() {
        let input = "\x1b[2J\x1b[Hhello\x1b[1;1H";
        let result = sanitize_for_render(input);
        assert_eq!(result, "hello");
    }

    #[test]
    fn strips_osc_terminal_title() {
        let input = "\x1b]0;owned\x07hello";
        let result = sanitize_for_render(input);
        assert_eq!(result, "hello");
    }

    #[test]
    fn strips_osc_terminated_with_st() {
        // OSC terminated with ST (ESC \) instead of BEL
        let input = "\x1b]0;title\x1b\\hello";
        let result = sanitize_for_render(input);
        assert_eq!(result, "hello");
    }

    #[test]
    fn strips_dcs_payload() {
        let input = "\x1bPqhello\x1b\\world";
        let result = sanitize_for_render(input);
        assert_eq!(result, "world");
    }

    #[test]
    fn removes_bel() {
        assert_eq!(sanitize_for_render("beep\x07!"), "beep!");
    }

    #[test]
    fn removes_backspace() {
        assert_eq!(sanitize_for_render("abc\x08def"), "abcdef");
    }

    #[test]
    fn removes_carriage_return() {
        assert_eq!(sanitize_for_render("line1\r\nline2"), "line1\nline2");
    }

    #[test]
    fn removes_del() {
        assert_eq!(sanitize_for_render("text\x7fend"), "textend");
    }

    #[test]
    fn removes_c1_controls() {
        // C1 controls: U+0080 to U+009F
        let input = "a\u{0085}b\u{0099}c";
        let result = sanitize_for_render(input);
        assert_eq!(result, "abc");
    }

    #[test]
    fn preserves_normal_unicode() {
        let input = "Ti₆Al₄V ΔH_mix 你好 café 🚀";
        let result = sanitize_for_render(input);
        assert_eq!(result, input);
    }

    #[test]
    fn preserves_newlines() {
        let input = "line1\nline2\nline3";
        assert_eq!(sanitize_for_render(input), input);
    }

    #[test]
    fn converts_tabs_to_spaces() {
        // Tabs are converted to 4 spaces for deterministic layout.
        assert_eq!(sanitize_for_render("a\tb"), "a    b");
    }

    #[test]
    fn safe_text_unchanged() {
        let input = "PRISM v2.7.1 — 42 tools available";
        assert_eq!(sanitize_for_render(input), input);
    }

    #[test]
    fn empty_string_returns_empty() {
        assert_eq!(sanitize_for_render(""), "");
    }

    #[test]
    fn long_safe_text_unchanged() {
        let input = "x".repeat(10_000);
        let result = sanitize_for_render(&input);
        assert_eq!(result.len(), 10_000);
        assert_eq!(result, input);
    }

    #[test]
    fn mixed_ansi_and_unicode() {
        let input = "\x1b[32mTi₆Al₄V\x1b[0m 你好 \x1b[1m🚀\x1b[0m";
        let result = sanitize_for_render(input);
        assert_eq!(result, "Ti₆Al₄V 你好 🚀");
    }

    #[test]
    fn no_escape_sequence_left_after_sanitize() {
        let inputs = [
            "\x1b[31mred\x1b[0m",
            "\x1b]0;title\x07text",
            "\x1b[2J\x1b[Hclear",
            "beep\x07back\x08del\x7f",
            "cr\r\nline",
            "\u{0085}\u{0099}c1",
        ];
        for input in inputs {
            let result = sanitize_for_render(input);
            assert!(!result.contains('\x1b'), "ESC left in: {result:?}");
            assert!(!result.contains('\x07'), "BEL left in: {result:?}");
            assert!(!result.contains('\x08'), "BS left in: {result:?}");
            assert!(!result.contains('\x0d'), "CR left in: {result:?}");
            assert!(!result.contains('\x7f'), "DEL left in: {result:?}");
        }
    }
}
