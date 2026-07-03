//! Theme registry — the theming primitive (modeled on opencode).
//!
//! opencode's TUI reads every color from an active theme (`theme/index.ts`)
//! and offers a theme picker (`dialog-theme-list`, `<leader>t`). PRISM ports
//! that: render reads semantic colors from [`App::theme`] instead of hardcoded
//! `Color::Rgb(...)` / named colors, and the palette command `theme.list`
//! opens a picker.
//!
//! [`Theme`] is [`Copy`] so it threads through the pure render path by value
//! with no borrowing noise. `THEMES[0]` ("prism") reproduces the original
//! hardcoded palette exactly, so the default look is unchanged.

use ratatui::style::Color;

/// A complete semantic color palette for the TUI.
///
/// Every color the renderer uses is a field here, so switching themes
/// recolors the whole interface uniformly.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    /// Brand / assistant / titles / emphasis (was `Rgb(120,165,215)`).
    pub accent: Color,
    /// Primary content text (was `Rgb(224,226,234)`).
    pub text: Color,
    /// Secondary text + thinking tokens (was `Rgb(150,158,182)`).
    pub dim: Color,
    /// Footers, hints, keybind labels (was `Rgb(120,128,150)`).
    pub muted: Color,
    /// Panel borders / dividers (was `Rgb(70,84,120)`).
    pub divider: Color,
    /// Status bar foreground (was `Rgb(228,230,236)`).
    pub status_fg: Color,
    /// Status bar background (was `Rgb(38,52,78)`).
    pub status_bg: Color,
    /// User role (was `Blue`).
    pub user: Color,
    /// System / status lines (was `DarkGray`).
    pub system: Color,
    /// Success (was `Green`).
    pub ok: Color,
    /// Error (was `Red`).
    pub err: Color,
    /// Warnings / tool activity / focus (was `Yellow`).
    pub warn: Color,
    /// Approval accent (was `Magenta`).
    pub approval: Color,
    /// Popup background fill (was `Black`).
    pub overlay_bg: Color,
    /// Panel background — header, sidebar, bordered boxes (opencode `backgroundPanel`).
    pub panel: Color,
}

/// All built-in themes. Index 0 is the default.
///
/// The default is **opencode** — PRISM's port of opencode's own default
/// dark theme (peach primary, purple accent, near-black bg), so the TUI
/// looks like opencode out of the box. The original "prism" blue palette
/// is kept as an alternative.
pub static THEMES: &[Theme] = &[
    // ── prism (default) — ported from opencode.json ─────────────────
    Theme {
        name: "prism",
        accent: Color::Rgb(255, 158, 64), // brighter orange #ff9e40 (brand)
        text: Color::Rgb(238, 238, 238),  // #eeeeee
        dim: Color::Rgb(130, 139, 184),   // diffContext #828bb8 — blue-gray
        muted: Color::Rgb(128, 128, 128), // textMuted #808080
        divider: Color::Rgb(72, 72, 72),  // border #484848
        status_fg: Color::Rgb(238, 238, 238),
        status_bg: Color::Rgb(30, 30, 30), // backgroundElement #1e1e1e
        user: Color::Rgb(92, 156, 245),    // secondary #5c9cf5 — blue
        system: Color::Rgb(128, 128, 128), // textMuted
        ok: Color::Rgb(127, 216, 143),     // success #7fd88f
        err: Color::Rgb(224, 108, 117),    // error #e06c75
        warn: Color::Rgb(245, 167, 66),    // warning #f5a742
        approval: Color::Rgb(157, 124, 216), // accent #9d7cd8 — purple
        overlay_bg: Color::Rgb(10, 10, 10), // background #0a0a0a
        panel: Color::Rgb(20, 20, 20),     // backgroundPanel #141414
    },
    Theme {
        name: "my eyes hurt",
        accent: Color::Rgb(120, 165, 215),
        text: Color::Rgb(224, 226, 234),
        dim: Color::Rgb(150, 158, 182),
        muted: Color::Rgb(120, 128, 150),
        divider: Color::Rgb(70, 84, 120),
        status_fg: Color::Rgb(228, 230, 236),
        status_bg: Color::Rgb(38, 52, 78),
        user: Color::Blue,
        system: Color::DarkGray,
        ok: Color::Green,
        err: Color::Red,
        warn: Color::Yellow,
        approval: Color::Magenta,
        overlay_bg: Color::Black,
        panel: Color::Rgb(26, 38, 58),
    },
    Theme {
        name: "midnight",
        accent: Color::Rgb(150, 140, 240),
        text: Color::Rgb(214, 218, 240),
        dim: Color::Rgb(150, 152, 188),
        muted: Color::Rgb(112, 114, 150),
        divider: Color::Rgb(58, 60, 104),
        status_fg: Color::Rgb(220, 222, 246),
        status_bg: Color::Rgb(28, 28, 64),
        user: Color::Rgb(130, 150, 255),
        system: Color::Rgb(120, 124, 158),
        ok: Color::Rgb(120, 220, 168),
        err: Color::Rgb(244, 116, 140),
        warn: Color::Rgb(244, 204, 96),
        approval: Color::Rgb(206, 124, 226),
        overlay_bg: Color::Rgb(12, 12, 28),
        panel: Color::Rgb(18, 18, 44),
    },
    Theme {
        name: "forest",
        accent: Color::Rgb(122, 184, 122),
        text: Color::Rgb(222, 230, 210),
        dim: Color::Rgb(162, 178, 150),
        muted: Color::Rgb(122, 138, 110),
        divider: Color::Rgb(64, 86, 64),
        status_fg: Color::Rgb(222, 232, 216),
        status_bg: Color::Rgb(26, 48, 30),
        user: Color::Rgb(140, 204, 140),
        system: Color::Rgb(132, 148, 118),
        ok: Color::Rgb(152, 224, 152),
        err: Color::Rgb(224, 120, 110),
        warn: Color::Rgb(222, 192, 92),
        approval: Color::Rgb(184, 204, 122),
        overlay_bg: Color::Rgb(10, 20, 12),
        panel: Color::Rgb(16, 32, 20),
    },
    Theme {
        name: "gruvbox",
        accent: Color::Rgb(250, 189, 47),
        text: Color::Rgb(235, 219, 178),
        dim: Color::Rgb(168, 153, 132),
        muted: Color::Rgb(146, 131, 116),
        divider: Color::Rgb(80, 73, 69),
        status_fg: Color::Rgb(235, 219, 178),
        status_bg: Color::Rgb(40, 40, 40),
        user: Color::Rgb(214, 93, 14),
        system: Color::Rgb(146, 131, 116),
        ok: Color::Rgb(184, 187, 38),
        err: Color::Rgb(251, 73, 52),
        warn: Color::Rgb(250, 189, 47),
        approval: Color::Rgb(214, 93, 14),
        overlay_bg: Color::Rgb(20, 18, 16),
        panel: Color::Rgb(28, 24, 20),
    },
    Theme {
        name: "mono",
        accent: Color::Rgb(200, 200, 200),
        text: Color::Rgb(228, 228, 228),
        dim: Color::Rgb(150, 150, 150),
        muted: Color::Rgb(118, 118, 118),
        divider: Color::Rgb(78, 78, 78),
        status_fg: Color::Rgb(208, 208, 208),
        status_bg: Color::Rgb(38, 38, 38),
        user: Color::Rgb(220, 220, 220),
        system: Color::Rgb(138, 138, 138),
        ok: Color::Rgb(176, 216, 176),
        err: Color::Rgb(220, 176, 176),
        warn: Color::Rgb(212, 212, 162),
        approval: Color::Rgb(202, 182, 202),
        overlay_bg: Color::Rgb(12, 12, 12),
        panel: Color::Rgb(24, 24, 24),
    },
];

/// Default theme index (always valid as long as THEMES is non-empty).
pub const DEFAULT: usize = 0;

pub fn get(index: usize) -> Theme {
    THEMES[index.min(THEMES.len() - 1)]
}

pub fn find(name: &str) -> Option<usize> {
    THEMES.iter().position(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_is_prism_at_index_zero() {
        assert_eq!(THEMES[DEFAULT].name, "prism");
    }

    #[test]
    fn prism_default_matches_opencode_json_tokens() {
        // The default theme ("prism") is a faithful port of opencode's dark theme.
        let t = get(DEFAULT);
        assert_eq!(t.name, "prism");
        assert_eq!(t.accent, Color::Rgb(255, 158, 64)); // #ff9e40 brighter orange
        assert_eq!(t.approval, Color::Rgb(157, 124, 216)); // #9d7cd8 purple
        assert_eq!(t.ok, Color::Rgb(127, 216, 143)); // #7fd88f
        assert_eq!(t.err, Color::Rgb(224, 108, 117)); // #e06c75
        assert_eq!(t.user, Color::Rgb(92, 156, 245)); // #5c9cf5
        assert_eq!(t.overlay_bg, Color::Rgb(10, 10, 10)); // #0a0a0a
    }

    #[test]
    fn my_eyes_hurt_theme_matches_legacy_hardcoded_colors() {
        // The original harsh blue palette is preserved as "my eyes hurt".
        let t = THEMES
            .iter()
            .find(|t| t.name == "my eyes hurt")
            .expect("my eyes hurt theme must exist");
        assert_eq!(t.accent, Color::Rgb(120, 165, 215));
        assert_eq!(t.text, Color::Rgb(224, 226, 234));
        assert_eq!(t.divider, Color::Rgb(70, 84, 120));
        assert_eq!(t.status_bg, Color::Rgb(38, 52, 78));
        assert_eq!(t.user, Color::Blue);
        assert_eq!(t.ok, Color::Green);
        assert_eq!(t.err, Color::Red);
        assert_eq!(t.warn, Color::Yellow);
        assert_eq!(t.approval, Color::Magenta);
        assert_eq!(t.system, Color::DarkGray);
    }

    #[test]
    fn theme_names_are_unique() {
        let mut names: Vec<&str> = THEMES.iter().map(|t| t.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "duplicate theme names");
    }

    #[test]
    fn get_clamps_out_of_range_index() {
        let last = THEMES.len() - 1;
        assert_eq!(get(999).name, THEMES[last].name);
        assert_eq!(get(last).name, THEMES[last].name);
    }

    #[test]
    fn find_resolves_known_names() {
        assert_eq!(find("prism"), Some(0));
        assert_eq!(find("my eyes hurt"), Some(1));
        assert_eq!(find("mono"), Some(THEMES.len() - 1));
        assert_eq!(find("nope"), None);
    }
}
