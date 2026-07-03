//! Generic form pane — the reusable structured-input widget.
//!
//! The command palette used to fire verbs straight into the chat; the
//! big verbs now open a *pane that asks the right questions first*.
//! This module is the shared foundation: a small declarative form
//! (text / toggle / select / stepper fields) with palette-style key
//! handling. Feature panes (deep research, knowledge, MCP settings)
//! and backend-requested forms (`ui.form.request`) all build on it.
//!
//! Keys mirror the other overlays: ↑/↓ (or Tab/Shift-Tab) move between
//! fields, Space toggles, ←/→ adjusts steppers and selects, typing
//! edits the focused text field, Enter submits, Esc/Ctrl-C cancels.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

/// One field's editable state.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    /// Free-text input (append/backspace editing, palette-query style).
    Text { value: String },
    /// Boolean checkbox — Space toggles.
    Toggle { value: bool },
    /// Single-select over fixed options — ←/→ cycles.
    Select {
        options: Vec<String>,
        selected: usize,
    },
    /// Clamped integer stepper — ←/→ (or -/+) adjusts.
    Stepper { value: i64, min: i64, max: i64 },
}

/// A single labeled form field.
#[derive(Debug, Clone)]
pub struct FormField {
    /// Key used for this field in the submitted values object.
    pub name: String,
    /// Human label shown left of the value.
    pub label: String,
    pub kind: FieldKind,
    /// Dim suffix rendered after the value, e.g. "(advisory)".
    pub note: Option<String>,
}

impl FormField {
    pub fn text(name: &str, label: &str, value: &str) -> Self {
        Self {
            name: name.to_string(),
            label: label.to_string(),
            kind: FieldKind::Text {
                value: value.to_string(),
            },
            note: None,
        }
    }

    pub fn toggle(name: &str, label: &str, value: bool) -> Self {
        Self {
            name: name.to_string(),
            label: label.to_string(),
            kind: FieldKind::Toggle { value },
            note: None,
        }
    }

    pub fn select(name: &str, label: &str, options: Vec<String>, selected: usize) -> Self {
        let last = options.len().saturating_sub(1);
        Self {
            name: name.to_string(),
            label: label.to_string(),
            kind: FieldKind::Select {
                options,
                selected: selected.min(last),
            },
            note: None,
        }
    }

    pub fn stepper(name: &str, label: &str, value: i64, min: i64, max: i64) -> Self {
        Self {
            name: name.to_string(),
            label: label.to_string(),
            kind: FieldKind::Stepper {
                value: value.clamp(min, max),
                min,
                max,
            },
            note: None,
        }
    }

    pub fn with_note(mut self, note: &str) -> Self {
        self.note = Some(note.to_string());
        self
    }

    /// This field's value as JSON (text → string, toggle → bool,
    /// select → the selected option string, stepper → integer).
    pub fn value_json(&self) -> Value {
        match &self.kind {
            FieldKind::Text { value } => Value::String(value.clone()),
            FieldKind::Toggle { value } => Value::Bool(*value),
            FieldKind::Select { options, selected } => {
                Value::String(options.get(*selected).cloned().unwrap_or_default())
            }
            FieldKind::Stepper { value, .. } => Value::from(*value),
        }
    }
}

/// What a keypress did to the form.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FormOutcome {
    /// Key consumed; the form stays open.
    Continue,
    /// Enter — the caller reads [`Form::values`] and closes the pane.
    Submit,
    /// Esc / Ctrl-C — the caller discards and closes the pane.
    Cancel,
}

/// A titled form: an ordered field list plus focus state.
#[derive(Debug, Clone, Default)]
pub struct Form {
    pub title: String,
    pub fields: Vec<FormField>,
    /// Index of the focused field.
    pub focused: usize,
    /// Verb shown in the footer hint, e.g. "launch" → "↵ launch".
    pub submit_label: String,
}

impl Form {
    pub fn new(title: &str, submit_label: &str, fields: Vec<FormField>) -> Self {
        Self {
            title: title.to_string(),
            fields,
            focused: 0,
            submit_label: submit_label.to_string(),
        }
    }

    /// Submitted values keyed by field name.
    pub fn values(&self) -> serde_json::Map<String, Value> {
        self.fields
            .iter()
            .map(|f| (f.name.clone(), f.value_json()))
            .collect()
    }

    /// The current string of a text field by name (empty if absent).
    pub fn text_value(&self, name: &str) -> String {
        self.fields
            .iter()
            .find(|f| f.name == name)
            .and_then(|f| match &f.kind {
                FieldKind::Text { value } => Some(value.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The current boolean of a toggle field by name (false if absent).
    pub fn toggle_value(&self, name: &str) -> bool {
        self.fields
            .iter()
            .find(|f| f.name == name)
            .and_then(|f| match &f.kind {
                FieldKind::Toggle { value } => Some(*value),
                _ => None,
            })
            .unwrap_or(false)
    }

    /// The current integer of a stepper field by name (0 if absent).
    pub fn stepper_value(&self, name: &str) -> i64 {
        self.fields
            .iter()
            .find(|f| f.name == name)
            .and_then(|f| match &f.kind {
                FieldKind::Stepper { value, .. } => Some(*value),
                _ => None,
            })
            .unwrap_or(0)
    }

    fn move_focus(&mut self, delta: i32) {
        let n = self.fields.len();
        if n == 0 {
            return;
        }
        let next = (self.focused as i32 + delta).rem_euclid(n as i32);
        self.focused = next as usize;
    }

    fn adjust(&mut self, delta: i64) {
        let Some(field) = self.fields.get_mut(self.focused) else {
            return;
        };
        match &mut field.kind {
            FieldKind::Stepper { value, min, max } => {
                *value = value.saturating_add(delta).clamp(*min, *max);
            }
            FieldKind::Select { options, selected } => {
                let n = options.len();
                if n > 0 {
                    let next = (*selected as i64 + delta).rem_euclid(n as i64);
                    *selected = next as usize;
                }
            }
            FieldKind::Toggle { value } => *value = !*value,
            FieldKind::Text { .. } => {}
        }
    }

    /// Handle a key while the form is open. Pure state transition —
    /// the caller acts on the returned [`FormOutcome`].
    pub fn handle_key(&mut self, key: KeyEvent) -> FormOutcome {
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;
        if cancel {
            return FormOutcome::Cancel;
        }

        let focused_is_text = matches!(
            self.fields.get(self.focused).map(|f| &f.kind),
            Some(FieldKind::Text { .. })
        );

        match key.code {
            KeyCode::Enter => return FormOutcome::Submit,
            KeyCode::Down | KeyCode::Tab => self.move_focus(1),
            KeyCode::Up | KeyCode::BackTab => self.move_focus(-1),
            KeyCode::Left => self.adjust(-1),
            KeyCode::Right => self.adjust(1),
            KeyCode::Char(' ') if !focused_is_text => self.adjust(1),
            KeyCode::Char('+') if !focused_is_text => self.adjust(1),
            KeyCode::Char('-') if !focused_is_text => self.adjust(-1),
            KeyCode::Backspace => {
                if let Some(FieldKind::Text { value }) =
                    self.fields.get_mut(self.focused).map(|f| &mut f.kind)
                {
                    value.pop();
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(FieldKind::Text { value }) =
                    self.fields.get_mut(self.focused).map(|f| &mut f.kind)
                {
                    value.push(c);
                }
            }
            _ => {}
        }
        FormOutcome::Continue
    }
}

/// Build a [`Form`] from a backend `ui.form.request` field spec:
/// `[{name, type, label, options?, default?}, …]`. Unknown types fall
/// back to text so a newer backend never renders an empty pane.
pub fn form_from_spec(title: &str, fields: &[Value]) -> Form {
    let parsed = fields
        .iter()
        .filter_map(|f| {
            let name = f.get("name").and_then(|v| v.as_str())?;
            let label = f.get("label").and_then(|v| v.as_str()).unwrap_or(name);
            let ftype = f.get("type").and_then(|v| v.as_str()).unwrap_or("text");
            let default = f.get("default");
            Some(match ftype {
                "toggle" | "checkbox" | "bool" => FormField::toggle(
                    name,
                    label,
                    default.and_then(|v| v.as_bool()).unwrap_or(false),
                ),
                "select" => {
                    let options: Vec<String> = f
                        .get("options")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|o| o.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    let selected = default
                        .and_then(|v| v.as_str())
                        .and_then(|d| options.iter().position(|o| o == d))
                        .unwrap_or(0);
                    FormField::select(name, label, options, selected)
                }
                "number" | "stepper" | "integer" => {
                    let min = f.get("min").and_then(|v| v.as_i64()).unwrap_or(0);
                    let max = f.get("max").and_then(|v| v.as_i64()).unwrap_or(100);
                    FormField::stepper(
                        name,
                        label,
                        default.and_then(|v| v.as_i64()).unwrap_or(min),
                        min,
                        max,
                    )
                }
                _ => FormField::text(name, label, default.and_then(|v| v.as_str()).unwrap_or("")),
            })
        })
        .collect();
    Form::new(title, "submit", parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample_form() -> Form {
        Form::new(
            "Test",
            "go",
            vec![
                FormField::text("q", "Question", ""),
                FormField::stepper("depth", "Depth", 1, 0, 5),
                FormField::toggle("web", "Web", true),
                FormField::select(
                    "transport",
                    "Transport",
                    vec!["stdio".into(), "http".into()],
                    0,
                ),
            ],
        )
    }

    #[test]
    fn typing_edits_focused_text_field() {
        let mut f = sample_form();
        for c in "NiTi".chars() {
            assert_eq!(f.handle_key(key(KeyCode::Char(c))), FormOutcome::Continue);
        }
        f.handle_key(key(KeyCode::Backspace));
        assert_eq!(f.text_value("q"), "NiT");
    }

    #[test]
    fn navigation_wraps_both_ways() {
        let mut f = sample_form();
        f.handle_key(key(KeyCode::Up));
        assert_eq!(f.focused, 3, "Up from first field wraps to last");
        f.handle_key(key(KeyCode::Tab));
        assert_eq!(f.focused, 0, "Tab from last field wraps to first");
        f.handle_key(key(KeyCode::Down));
        assert_eq!(f.focused, 1);
    }

    #[test]
    fn stepper_clamps_at_bounds() {
        let mut f = sample_form();
        f.focused = 1;
        for _ in 0..10 {
            f.handle_key(key(KeyCode::Right));
        }
        assert_eq!(f.stepper_value("depth"), 5, "stepper must clamp at max");
        for _ in 0..10 {
            f.handle_key(key(KeyCode::Left));
        }
        assert_eq!(f.stepper_value("depth"), 0, "stepper must clamp at min");
    }

    #[test]
    fn space_toggles_and_select_cycles() {
        let mut f = sample_form();
        f.focused = 2;
        f.handle_key(key(KeyCode::Char(' ')));
        assert!(!f.toggle_value("web"));
        f.focused = 3;
        f.handle_key(key(KeyCode::Right));
        assert_eq!(f.values()["transport"], "http");
        f.handle_key(key(KeyCode::Right));
        assert_eq!(f.values()["transport"], "stdio", "select wraps");
    }

    #[test]
    fn space_types_into_text_fields() {
        let mut f = sample_form();
        f.handle_key(key(KeyCode::Char('a')));
        f.handle_key(key(KeyCode::Char(' ')));
        f.handle_key(key(KeyCode::Char('b')));
        assert_eq!(f.text_value("q"), "a b");
    }

    #[test]
    fn enter_submits_and_esc_cancels() {
        let mut f = sample_form();
        assert_eq!(f.handle_key(key(KeyCode::Enter)), FormOutcome::Submit);
        assert_eq!(f.handle_key(key(KeyCode::Esc)), FormOutcome::Cancel);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(f.handle_key(ctrl_c), FormOutcome::Cancel);
    }

    #[test]
    fn values_map_types() {
        let f = sample_form();
        let v = f.values();
        assert_eq!(v["q"], "");
        assert_eq!(v["depth"], 1);
        assert_eq!(v["web"], true);
        assert_eq!(v["transport"], "stdio");
    }

    #[test]
    fn form_from_spec_parses_all_field_types() {
        let spec = vec![
            serde_json::json!({"name":"question","type":"text","label":"Question"}),
            serde_json::json!({"name":"depth","type":"number","label":"Depth","min":0,"max":5,"default":1}),
            serde_json::json!({"name":"web","type":"toggle","label":"Web","default":true}),
            serde_json::json!({"name":"mode","type":"select","label":"Mode","options":["graph","full"],"default":"full"}),
            serde_json::json!({"name":"mystery","type":"hologram","label":"Unknown"}),
        ];
        let f = form_from_spec("Backend form", &spec);
        assert_eq!(f.fields.len(), 5);
        let v = f.values();
        assert_eq!(v["question"], "");
        assert_eq!(v["depth"], 1);
        assert_eq!(v["web"], true);
        assert_eq!(v["mode"], "full");
        assert_eq!(v["mystery"], "", "unknown types fall back to text");
    }

    #[test]
    fn form_from_spec_skips_nameless_fields() {
        let spec = vec![serde_json::json!({"type":"text","label":"No name"})];
        assert!(form_from_spec("t", &spec).fields.is_empty());
    }
}
