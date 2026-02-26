// Mirrors app/cli/tui/theme.py — keep in sync!

// ── Base palette ───────────────────────────────────────────────────
export const PRIMARY = "#fab283";
export const SECONDARY = "#5c9cf5";
export const ACCENT_MAGENTA = "#bb86fc";
export const ACCENT_CYAN = "#56b6c2";

// ── Semantic colours ───────────────────────────────────────────────
// ACCENT in Python is `f"bold {PRIMARY}"` (Rich style string).
// In Ink bold is a prop, so we export the colour only.
export const ACCENT = PRIMARY;

export const SUCCESS = "#7fd88f";
export const WARNING = "#e5c07b";
export const ERROR = "#e06c75";
export const INFO = "#61afef";
// DIM in Python is the literal string "dim" (a Rich style).
// Ink uses the `dimColor` prop; when a hex is needed, use MUTED.
export const DIM = "#808080";
export const TEXT = "#e0e0e0";
export const MUTED = "#808080";

// ── Card icons ─────────────────────────────────────────────────────
export const ICONS: Record<string, string> = {
  input: "\u276f",       // ❯
  output: "\u25cb",      // ○
  tool: "\u2699",        // ⚙
  error: "\u2717",       // ✗
  success: "\u2714",     // ✔
  metrics: "\u25a0",     // ■
  calphad: "\u2206",     // ∆
  labs: "\u2726",        // ✦
  validation: "\u25cf",  // ●
  results: "\u2261",     // ≡
  plot: "\u25a3",        // ▣
  approval: "\u26a0",    // ⚠
  plan: "\u25b7",        // ▷
  pending: "\u223c",     // ∼
};

// ── Crystal mascot — 3-tier glow ──────────────────────────────────
export const CRYSTAL_OUTER_DIM = "#555577";
export const CRYSTAL_OUTER = "#7777aa";
export const CRYSTAL_INNER = "#ccccff";
export const CRYSTAL_CORE = "#ffffff";

// VIBGYOR rainbow (15 stops for ray length)
export const RAINBOW = [
  "#ff0000", "#ff5500", "#ff8800", "#ffcc00", "#88ff00",
  "#00cc44", "#00cccc", "#0088ff", "#5500ff", "#8b00ff",
  "#ff0000", "#ff5500", "#ff8800", "#ffcc00", "#88ff00",
];

// Welcome header commands (shown next to rainbow rays)
export const HEADER_COMMANDS_L = ["/help", "/tools", "/skills"];
export const HEADER_COMMANDS_R = ["/scratchpad", "/status", "/save"];

// ── Card border colours by result type ─────────────────────────────
export const BORDERS: Record<string, string> = {
  input: ACCENT_CYAN,
  output: MUTED,
  tool: SUCCESS,
  error: ERROR,
  error_partial: WARNING,
  metrics: INFO,
  calphad: SECONDARY,
  labs: ACCENT_MAGENTA,
  validation_critical: ERROR,
  validation_warning: WARNING,
  validation_info: INFO,
  results: MUTED,
  plot: SUCCESS,
  approval: WARNING,
  plan: ACCENT_MAGENTA,
};
