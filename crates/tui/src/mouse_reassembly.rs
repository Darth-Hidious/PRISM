//! Mouse reports that arrive in two pieces are put back together.
//!
//! Measured 2026-09-05 in the owner's terminal: scrolling the wheel typed
//! `[<64;193;34M[<65;167;39M` into the prompt. Those are SGR mouse reports
//! (`ESC [ < button ; column ; row M`) whose leading escape byte reached the
//! parser in one read and the rest in the next. A lone escape decodes as the
//! Esc key; the remainder decodes as ordinary characters, and the prompt
//! faithfully showed them. Nothing scrolled.
//!
//! This holds a bare Esc briefly. If the characters that follow spell out a
//! mouse report, the report is what the app sees — one scroll, at the place
//! the pointer was. If they do not, the Esc and the characters are delivered
//! exactly as they came, only a few milliseconds later than they would have
//! been. A reader who presses Esc and then types `[` still gets both.

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::time::{Duration, Instant};

/// How long a bare Esc waits for the rest of a report before it is released
/// as an Esc key press. Reports arrive within a millisecond or two; a human
/// pressing Esc and then a key takes far longer.
pub const HOLD: Duration = Duration::from_millis(60);

#[derive(Default)]
pub struct SgrReassembler {
    /// A bare Esc seen at `since`, with the characters that followed it so far.
    pending: Option<(Instant, String)>,
}

impl SgrReassembler {
    /// Hand every terminal event through here. Returns the events the app
    /// should see now — possibly none (held), possibly several (released).
    pub fn feed(&mut self, ev: Event, now: Instant) -> Vec<Event> {
        match (&mut self.pending, &ev) {
            (None, Event::Key(k)) if is_bare_esc(k) => {
                self.pending = Some((now, String::new()));
                Vec::new()
            }
            (None, _) => vec![ev],
            (Some((since, buf)), Event::Key(k))
                if k.modifiers.is_empty() || k.modifiers == KeyModifiers::SHIFT =>
            {
                if let KeyCode::Char(c) = k.code
                    && now.duration_since(*since) <= HOLD
                {
                    let mut candidate = buf.clone();
                    candidate.push(c);
                    if let Some(mouse) = parse_sgr(&candidate) {
                        self.pending = None;
                        return vec![Event::Mouse(mouse)];
                    }
                    if is_sgr_prefix(&candidate) {
                        *buf = candidate;
                        return Vec::new();
                    }
                }
                let mut out = self.release();
                out.push(ev);
                out
            }
            (Some(_), _) => {
                let mut out = self.release();
                out.push(ev);
                out
            }
        }
    }

    /// Called on the render tick: a held Esc older than `HOLD` is released as
    /// the key press it was.
    pub fn flush(&mut self, now: Instant) -> Vec<Event> {
        match &self.pending {
            Some((since, _)) if now.duration_since(*since) > HOLD => self.release(),
            _ => Vec::new(),
        }
    }

    fn release(&mut self) -> Vec<Event> {
        let Some((_, buf)) = self.pending.take() else {
            return Vec::new();
        };
        let mut out = vec![Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))];
        out.extend(
            buf.chars()
                .map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))),
        );
        out
    }
}

fn is_bare_esc(k: &KeyEvent) -> bool {
    k.code == KeyCode::Esc && k.modifiers.is_empty()
}

/// Whether `buf` could still grow into `[<b;x;yM` / `[<b;x;ym`.
fn is_sgr_prefix(buf: &str) -> bool {
    let mut chars = buf.chars();
    match chars.next() {
        None => return true,
        Some('[') => {}
        _ => return false,
    }
    match chars.next() {
        None => return true,
        Some('<') => {}
        _ => return false,
    }
    let rest: String = chars.collect();
    let mut fields = 0usize;
    for c in rest.chars() {
        match c {
            '0'..='9' => {}
            ';' => fields += 1,
            _ => return false,
        }
    }
    fields <= 2 && rest.len() <= 16
}

/// A complete `[<b;x;yM` (press / wheel) or `[<b;x;ym` (release), as the
/// mouse event the terminal meant. Columns and rows are 1-based on the wire.
fn parse_sgr(buf: &str) -> Option<MouseEvent> {
    let body = buf.strip_prefix("[<")?;
    let (nums, final_byte) = body.split_at(body.len().checked_sub(1)?);
    let release = match final_byte {
        "M" => false,
        "m" => true,
        _ => return None,
    };
    let mut it = nums.split(';');
    let button: u16 = it.next()?.parse().ok()?;
    let column: u16 = it.next()?.parse().ok()?;
    let row: u16 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    let column = column.saturating_sub(1);
    let row = row.saturating_sub(1);
    let modifiers = KeyModifiers::NONE;
    let kind = match button & !0b11100 {
        64 => MouseEventKind::ScrollUp,
        65 => MouseEventKind::ScrollDown,
        66 => MouseEventKind::ScrollLeft,
        67 => MouseEventKind::ScrollRight,
        b if b & 32 != 0 => match b & 0b11 {
            3 => MouseEventKind::Moved,
            0 => MouseEventKind::Drag(MouseButton::Left),
            1 => MouseEventKind::Drag(MouseButton::Middle),
            _ => MouseEventKind::Drag(MouseButton::Right),
        },
        b => {
            let btn = match b & 0b11 {
                0 => MouseButton::Left,
                1 => MouseButton::Middle,
                2 => MouseButton::Right,
                _ => return None,
            };
            if release {
                MouseEventKind::Up(btn)
            } else {
                MouseEventKind::Down(btn)
            }
        }
    };
    Some(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }
    fn esc() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
    }

    /// The exact bytes the owner saw typed into the prompt become the two
    /// wheel events they were.
    #[test]
    fn a_split_wheel_report_becomes_a_scroll_and_types_nothing() {
        let mut r = SgrReassembler::default();
        let t0 = Instant::now();
        let mut out = Vec::new();
        for ev in std::iter::once(esc()).chain("[<64;193;34M".chars().map(key)) {
            out.extend(r.feed(ev, t0));
        }
        assert_eq!(out.len(), 1, "{out:?}");
        match &out[0] {
            Event::Mouse(m) => {
                assert_eq!(m.kind, MouseEventKind::ScrollUp);
                assert_eq!((m.column, m.row), (192, 33));
            }
            other => panic!("{other:?}"),
        }
        let mut out = Vec::new();
        for ev in std::iter::once(esc()).chain("[<65;167;39M".chars().map(key)) {
            out.extend(r.feed(ev, t0));
        }
        assert!(
            matches!(&out[..], [Event::Mouse(m)] if m.kind == MouseEventKind::ScrollDown),
            "{out:?}"
        );
    }

    /// Esc followed by an ordinary key is still Esc and that key, in order.
    #[test]
    fn esc_then_a_letter_is_delivered_as_both() {
        let mut r = SgrReassembler::default();
        let t0 = Instant::now();
        assert!(r.feed(esc(), t0).is_empty(), "held");
        let out = r.feed(key('x'), t0);
        assert!(
            matches!(&out[..], [Event::Key(e), Event::Key(x)] if e.code == KeyCode::Esc && x.code == KeyCode::Char('x')),
            "{out:?}"
        );
    }

    /// A bare Esc alone is released by the tick once the hold has passed —
    /// never swallowed.
    #[test]
    fn a_lone_esc_is_released_after_the_hold() {
        let mut r = SgrReassembler::default();
        let t0 = Instant::now();
        assert!(r.feed(esc(), t0).is_empty());
        assert!(
            r.flush(t0 + Duration::from_millis(10)).is_empty(),
            "still within the hold"
        );
        let out = r.flush(t0 + HOLD + Duration::from_millis(1));
        assert!(
            matches!(&out[..], [Event::Key(e)] if e.code == KeyCode::Esc),
            "{out:?}"
        );
    }

    /// `[<` typed slowly by a person is not a mouse report; both keys arrive.
    #[test]
    fn a_slow_bracket_after_esc_is_typed_not_eaten() {
        let mut r = SgrReassembler::default();
        let t0 = Instant::now();
        assert!(r.feed(esc(), t0).is_empty());
        let out = r.feed(key('['), t0 + HOLD + Duration::from_millis(5));
        assert_eq!(out.len(), 2, "{out:?}");
    }

    #[test]
    fn a_press_and_release_decode_with_their_button() {
        let m = parse_sgr("[<0;10;5M").unwrap();
        assert_eq!(m.kind, MouseEventKind::Down(MouseButton::Left));
        assert_eq!((m.column, m.row), (9, 4));
        let m = parse_sgr("[<2;10;5m").unwrap();
        assert_eq!(m.kind, MouseEventKind::Up(MouseButton::Right));
        assert!(
            parse_sgr("[<64;193M").is_none(),
            "two fields is not a report"
        );
    }
}
