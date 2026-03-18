export const PRIMARY = "#7ce38b";
export const SECONDARY = "#8fb8ff";
export const ACCENT_MAGENTA = "#f4c96b";
export const ACCENT_CYAN = "#6bd6ff";

export const ACCENT = PRIMARY;

export const SUCCESS = "#7ce38b";
export const WARNING = "#f4c96b";
export const ERROR = "#ff7a7a";
export const INFO = "#6bd6ff";
export const DIM = "#5d6875";
export const TEXT = "#e6edf3";
export const MUTED = "#8b98a5";

export const ICONS: Record<string, string> = {
  input: ">",
  output: "·",
  tool: "!",
  error: "x",
  success: "+",
  metrics: "=",
  calphad: "d",
  labs: "*",
  validation: "i",
  results: "#",
  plot: "%",
  approval: "?",
  plan: "~",
  pending: "...",
};

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
