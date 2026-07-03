//! Pragmatic LaTeX-math → Unicode approximation for the transcript.
//!
//! A terminal can't typeset real LaTeX, so we map the common subset that
//! models actually emit — Greek letters, relational/operator symbols,
//! super/subscripts, `\frac`, `\sqrt` — onto readable Unicode. Anything
//! unrecognized degrades to its best-effort literal form rather than
//! vanishing. Pure Rust, no dependencies.
//!
//! Delimiter detection (`$…$`, `$$…$$`, `\(…\)`, `\[…\]`) lives in the
//! markdown inline parser; this module converts the inner content.

/// Convert a LaTeX math fragment (delimiters already stripped) to a
/// Unicode approximation.
///
/// ```
/// # use prism_tui::latex::render_math;
/// assert_eq!(render_math("E = mc^2"), "E = mc²");
/// assert_eq!(render_math(r"\alpha + \beta"), "α + β");
/// ```
pub fn render_math(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                let (cmd, next) = read_command(&chars, i);
                i = next;
                apply_command(&cmd, &chars, &mut i, &mut out);
            }
            '^' => {
                i += 1;
                let (g, next) = take_group(&chars, i);
                i = next;
                push_script(&render_math(&g), true, &mut out);
            }
            '_' => {
                i += 1;
                let (g, next) = take_group(&chars, i);
                i = next;
                push_script(&render_math(&g), false, &mut out);
            }
            '{' | '}' => i += 1, // grouping braces are invisible
            '\'' => {
                out.push('′'); // f' → f prime
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Read a `\command` starting at `chars[i] == '\\'`. Alphabetic commands
/// (`\alpha`) read the whole word; a single non-letter after the
/// backslash (`\,`, `\{`, `\\`) is a one-char control symbol.
fn read_command(chars: &[char], i: usize) -> (String, usize) {
    let mut j = i + 1;
    if j >= chars.len() {
        return (String::new(), j);
    }
    if chars[j].is_ascii_alphabetic() {
        let start = j;
        while j < chars.len() && chars[j].is_ascii_alphabetic() {
            j += 1;
        }
        (chars[start..j].iter().collect(), j)
    } else {
        (chars[j].to_string(), j + 1)
    }
}

/// Take the argument of a command/script: a `{…}` group (returned raw, so
/// the caller re-renders it) or a single following token.
fn take_group(chars: &[char], i: usize) -> (String, usize) {
    let mut j = i;
    while j < chars.len() && chars[j] == ' ' {
        j += 1;
    }
    if j >= chars.len() {
        return (String::new(), j);
    }
    if chars[j] == '{' {
        let mut depth = 1;
        let mut k = j + 1;
        let mut s = String::new();
        while k < chars.len() && depth > 0 {
            match chars[k] {
                '{' => {
                    depth += 1;
                    s.push('{');
                }
                '}' => {
                    depth -= 1;
                    if depth > 0 {
                        s.push('}');
                    }
                }
                c => s.push(c),
            }
            k += 1;
        }
        (s, k)
    } else if chars[j] == '\\' {
        let (_, next) = read_command(chars, j);
        (chars[j..next].iter().collect(), next)
    } else {
        (chars[j].to_string(), j + 1)
    }
}

/// Dispatch a command: symbol lookup, `\frac`/`\sqrt`, font/accent macros
/// (output their argument), spacing macros, or best-effort fallthrough.
fn apply_command(cmd: &str, chars: &[char], i: &mut usize, out: &mut String) {
    match cmd {
        "frac" | "dfrac" | "tfrac" => {
            let (a, i1) = take_group(chars, *i);
            *i = i1;
            let (b, i2) = take_group(chars, *i);
            *i = i2;
            let (ra, rb) = (render_math(&a), render_math(&b));
            out.push_str(&paren_if_needed(&ra));
            out.push('/');
            out.push_str(&paren_if_needed(&rb));
        }
        "sqrt" => {
            let mut j = *i;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            // optional [index]
            if j < chars.len() && chars[j] == '[' {
                let mut k = j + 1;
                let mut s = String::new();
                while k < chars.len() && chars[k] != ']' {
                    s.push(chars[k]);
                    k += 1;
                }
                if k < chars.len() {
                    k += 1;
                }
                let idx = render_math(&s);
                out.push_str(&to_script(&idx, true).unwrap_or(idx));
                j = k;
            }
            *i = j;
            let (g, i1) = take_group(chars, *i);
            *i = i1;
            let rg = render_math(&g);
            out.push('√');
            if rg.chars().count() > 1 {
                out.push('(');
                out.push_str(&rg);
                out.push(')');
            } else {
                out.push_str(&rg);
            }
        }
        // Font / accent macros: keep the argument, drop the styling.
        "text" | "mathrm" | "mathbf" | "mathit" | "mathsf" | "mathtt" | "mathbb" | "mathcal"
        | "operatorname" | "vec" | "hat" | "bar" | "tilde" | "dot" | "ddot" | "overline"
        | "underline" | "boldsymbol" | "mathfrak" => {
            let (g, i1) = take_group(chars, *i);
            *i = i1;
            out.push_str(&render_math(&g));
        }
        // Sizing / style macros with no visible glyph.
        "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr" | "displaystyle"
        | "textstyle" | "scriptstyle" | "limits" | "nolimits" => {}
        _ => {
            if let Some(sym) = symbol(cmd) {
                out.push_str(sym);
            } else {
                match cmd {
                    "{" | "}" | "$" | "%" | "#" | "&" | "_" => out.push_str(cmd),
                    "," | ";" | ":" | " " => out.push(' '),
                    "quad" | "qquad" => out.push_str("  "),
                    "!" => {}
                    "\\" => out.push(' '),
                    "'" => out.push('′'),
                    // Unknown command: emit the bare name so nothing is lost.
                    other => out.push_str(other),
                }
            }
        }
    }
}

/// Append a super/sub-scripted string. Uses real Unicode super/subscripts
/// when every character maps; otherwise falls back to `^(…)` / `_(…)`.
fn push_script(rendered: &str, sup: bool, out: &mut String) {
    if let Some(s) = to_script(rendered, sup) {
        out.push_str(&s);
        return;
    }
    out.push(if sup { '^' } else { '_' });
    if rendered.chars().count() > 1 {
        out.push('(');
        out.push_str(rendered);
        out.push(')');
    } else {
        out.push_str(rendered);
    }
}

/// Map a whole string to Unicode super- (`sup=true`) or subscripts, or
/// `None` if any character has no mapping.
fn to_script(s: &str, sup: bool) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    let mut r = String::new();
    for c in s.chars() {
        let m = if sup { superscript(c) } else { subscript(c) };
        r.push(m?);
    }
    Some(r)
}

fn superscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        '+' => '⁺',
        '-' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'a' => 'ᵃ',
        'b' => 'ᵇ',
        'c' => 'ᶜ',
        'd' => 'ᵈ',
        'e' => 'ᵉ',
        'f' => 'ᶠ',
        'g' => 'ᵍ',
        'h' => 'ʰ',
        'i' => 'ⁱ',
        'j' => 'ʲ',
        'k' => 'ᵏ',
        'l' => 'ˡ',
        'm' => 'ᵐ',
        'n' => 'ⁿ',
        'o' => 'ᵒ',
        'p' => 'ᵖ',
        'r' => 'ʳ',
        's' => 'ˢ',
        't' => 'ᵗ',
        'u' => 'ᵘ',
        'v' => 'ᵛ',
        'w' => 'ʷ',
        'x' => 'ˣ',
        'y' => 'ʸ',
        'z' => 'ᶻ',
        _ => return None,
    })
}

fn subscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '₀',
        '1' => '₁',
        '2' => '₂',
        '3' => '₃',
        '4' => '₄',
        '5' => '₅',
        '6' => '₆',
        '7' => '₇',
        '8' => '₈',
        '9' => '₉',
        '+' => '₊',
        '-' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'h' => 'ₕ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'k' => 'ₖ',
        'l' => 'ₗ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'p' => 'ₚ',
        'r' => 'ᵣ',
        's' => 'ₛ',
        't' => 'ₜ',
        'u' => 'ᵤ',
        'v' => 'ᵥ',
        'x' => 'ₓ',
        _ => return None,
    })
}

/// Parenthesize a fraction operand only when it holds an operator/space so
/// `\frac{a}{b}` stays `a/b` but `\frac{a+b}{c}` becomes `(a+b)/c`.
fn paren_if_needed(s: &str) -> String {
    if s.chars().any(|c| " +-±×÷⋅·*/=<>≤≥".contains(c)) {
        format!("({s})")
    } else {
        s.to_string()
    }
}

/// LaTeX command name → Unicode symbol. Covers the Greek alphabet plus the
/// operators/relations/arrows that show up in scientific transcripts.
fn symbol(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        // lowercase Greek
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" => "ε",
        "varepsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" => "θ",
        "vartheta" => "ϑ",
        "iota" => "ι",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "omicron" => "ο",
        "pi" => "π",
        "varpi" => "ϖ",
        "rho" => "ρ",
        "varrho" => "ϱ",
        "sigma" => "σ",
        "varsigma" => "ς",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" => "φ",
        "varphi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        // uppercase Greek
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Upsilon" => "Υ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        // binary operators
        "times" => "×",
        "cdot" => "⋅",
        "div" => "÷",
        "pm" => "±",
        "mp" => "∓",
        "ast" => "∗",
        "star" => "⋆",
        "circ" => "∘",
        "bullet" => "∙",
        "oplus" => "⊕",
        "ominus" => "⊖",
        "otimes" => "⊗",
        "cup" => "∪",
        "cap" => "∩",
        "setminus" => "∖",
        "wedge" => "∧",
        "vee" => "∨",
        // relations
        "leq" | "le" => "≤",
        "geq" | "ge" => "≥",
        "neq" | "ne" => "≠",
        "approx" => "≈",
        "equiv" => "≡",
        "sim" => "∼",
        "simeq" => "≃",
        "cong" => "≅",
        "propto" => "∝",
        "ll" => "≪",
        "gg" => "≫",
        "subset" => "⊂",
        "subseteq" => "⊆",
        "supset" => "⊃",
        "supseteq" => "⊇",
        "in" => "∈",
        "notin" => "∉",
        "ni" => "∋",
        "perp" => "⊥",
        "parallel" => "∥",
        "mid" => "∣",
        // big operators / calculus
        "sum" => "∑",
        "prod" => "∏",
        "coprod" => "∐",
        "int" => "∫",
        "iint" => "∬",
        "oint" => "∮",
        "partial" => "∂",
        "nabla" => "∇",
        "infty" => "∞",
        "sqrt" => "√",
        // arrows
        "to" | "rightarrow" => "→",
        "leftarrow" => "←",
        "gets" => "←",
        "leftrightarrow" => "↔",
        "Rightarrow" => "⇒",
        "Leftarrow" => "⇐",
        "Leftrightarrow" => "⇔",
        "mapsto" => "↦",
        "uparrow" => "↑",
        "downarrow" => "↓",
        // logic / sets
        "forall" => "∀",
        "exists" => "∃",
        "nexists" => "∄",
        "neg" => "¬",
        "land" => "∧",
        "lor" => "∨",
        "emptyset" | "varnothing" => "∅",
        // misc
        "cdots" => "⋯",
        "ldots" | "dots" => "…",
        "vdots" => "⋮",
        "ddots" => "⋱",
        "prime" => "′",
        "angle" => "∠",
        "triangle" => "△",
        "degree" => "°",
        "hbar" => "ℏ",
        "ell" => "ℓ",
        "Re" => "ℜ",
        "Im" => "ℑ",
        "aleph" => "ℵ",
        "langle" => "⟨",
        "rangle" => "⟩",
        "lfloor" => "⌊",
        "rfloor" => "⌋",
        "lceil" => "⌈",
        "rceil" => "⌉",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn superscript_number() {
        assert_eq!(render_math("E = mc^2"), "E = mc²");
        assert_eq!(render_math("x^{10}"), "x¹⁰");
        assert_eq!(render_math("e^{-x}"), "e⁻ˣ");
    }

    #[test]
    fn subscripts() {
        assert_eq!(render_math("x_i + y_{2}"), "xᵢ + y₂");
        assert_eq!(render_math("a_{n+1}"), "aₙ₊₁");
    }

    #[test]
    fn greek_letters() {
        assert_eq!(render_math(r"\alpha + \beta = \gamma"), "α + β = γ");
        assert_eq!(render_math(r"\Gamma(\theta)"), "Γ(θ)");
        assert_eq!(render_math(r"\gamma'"), "γ′");
    }

    #[test]
    fn operators_and_relations() {
        assert_eq!(render_math(r"a \leq b \geq c \neq d"), "a ≤ b ≥ c ≠ d");
        assert_eq!(render_math(r"2 \times 3 \cdot 4 \pm 1"), "2 × 3 ⋅ 4 ± 1");
        assert_eq!(render_math(r"x \to \infty"), "x → ∞");
        assert_eq!(render_math(r"\sum \int \partial \nabla"), "∑ ∫ ∂ ∇");
    }

    #[test]
    fn fractions() {
        assert_eq!(render_math(r"\frac{a}{b}"), "a/b");
        assert_eq!(render_math(r"\frac{1}{2}"), "1/2");
        assert_eq!(render_math(r"\frac{a+b}{c}"), "(a+b)/c");
    }

    #[test]
    fn roots() {
        assert_eq!(render_math(r"\sqrt{x}"), "√x");
        assert_eq!(render_math(r"\sqrt{x+1}"), "√(x+1)");
        assert_eq!(render_math(r"\sqrt[3]{y}"), "³√y");
    }

    #[test]
    fn accent_and_font_macros_keep_argument() {
        assert_eq!(render_math(r"\vec{F} = m \vec{a}"), "F = m a");
        assert_eq!(render_math(r"\mathbf{x} + \text{const}"), "x + const");
    }

    #[test]
    fn spacing_and_delimiters_macros_drop() {
        assert_eq!(render_math(r"a\,b"), "a b");
        assert_eq!(render_math(r"\left(x\right)"), "(x)");
    }

    #[test]
    fn unknown_command_degrades_to_name() {
        // Nothing vanishes: an unmapped macro shows its bare name.
        assert_eq!(render_math(r"\foo bar"), "foo bar");
    }

    #[test]
    fn combined_formula() {
        assert_eq!(
            render_math(r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2}"),
            "∑ᵢ₌₁ⁿ i = (n(n+1))/2"
        );
    }
}
