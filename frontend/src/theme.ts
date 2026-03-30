// PRISM TUI Design Tokens
//
// Philosophy: content-first, muted palette, selective emphasis.
// Inspired by OpenCode's layered background system.

// ── Brand ──────────────────────────────────────────────────────
export const PRIMARY = "#fab283";        // PRISM warm orange — sparingly, for brand identity
export const SECONDARY = "#5c9cf5";      // Blue — links, interactive elements

// ── Semantic ───────────────────────────────────────────────────
export const SUCCESS = "#4fd6be";        // Teal-green — checkmarks, healthy
export const WARNING = "#e5c07b";        // Amber — permissions, caution
export const ERROR = "#e06c75";          // Rose — errors, failures
export const ACCENT = "#c4a7e7";         // Lavender — plans, emphasis

// ── Text hierarchy ─────────────────────────────────────────────
export const TEXT = "#e6edf3";           // Primary text
export const TEXT_MUTED = "#8b98a5";     // Timestamps, metadata, help hints
export const TEXT_DIM = "#5d6875";       // Barely visible (line numbers, inactive)

// ── Backgrounds (layered — each step slightly lighter) ─────────
export const BG = "";                    // Transparent (terminal default)
export const BG_PANEL = "#161b22";       // Message containers, sidebar
export const BG_ELEMENT = "#21262d";     // Hover states, input fields
export const BG_MENU = "#30363d";        // Menus, selected items

// ── Borders ────────────────────────────────────────────────────
export const BORDER = "#30363d";         // Default borders
export const BORDER_ACTIVE = "#8b98a5";  // Focused borders
export const BORDER_AGENT = "#fab283";   // Agent message left-border (brand color)
export const BORDER_USER = "#5c9cf5";    // User message left-border

// ── Diff ───────────────────────────────────────────────────────
export const DIFF_ADDED = "#4fd6be";
export const DIFF_REMOVED = "#e06c75";

// ── Convenience re-exports (backwards compat) ──────────────────
export const MUTED = TEXT_MUTED;
export const DIM = TEXT_DIM;
export const BORDER_DIM = BORDER;
export const ACCENT_MAGENTA = ACCENT;
export const ACCENT_CYAN = SECONDARY;
export const INFO = SECONDARY;
