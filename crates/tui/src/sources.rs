//! The source table and the descriptor card a tool result is shown with.
//!
//! A reader watching a live session asked what they were looking at. This
//! answers, in a table, before the result body: where the data came from,
//! what kind of data it is, how much, how it is classed and why, and when it
//! was fetched. Every cell is a field the tool stamped, carried here by the
//! engine under `data.sources` and `data.descriptors` on the result card.
//! A tool that stamped nothing gets a line that says so, in the same bold —
//! never a blank, never a guess.

use prism_provenance::EvidenceClass;
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use crate::sanitize::sanitize_for_render;

/// One source the result names, as the tool reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    /// The reference id this row opens under, `provenance://<seq>/<n>`.
    pub id: String,
    pub source: String,
    pub kind: Option<String>,
    pub count: Option<u64>,
    pub fetched: Option<String>,
    pub status: Option<String>,
    /// The tool's own record for this source, pretty-printed for the panel.
    pub record: String,
}

/// One descriptor and where it was computed from, as the tool listed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescriptorRow {
    pub name: String,
    pub value: String,
    pub unit: Option<String>,
    pub origin: Option<String>,
}

fn clean(value: Option<&Value>) -> Option<String> {
    let text = match value? {
        Value::String(s) => s.clone(),
        Value::Null => return None,
        other => other.to_string(),
    };
    let text = sanitize_for_render(&text);
    let text = text.replace(['\n', '\r'], " ");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The source rows on a result card, with their reference ids. `seq` is the
/// card's number in this session, so ids stay stable when the transcript is
/// trimmed.
#[must_use]
pub fn source_rows(data: Option<&Value>, seq: u64) -> Vec<SourceRow> {
    let Some(rows) = data
        .and_then(|d| d.get("sources"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    rows.iter()
        .enumerate()
        .filter_map(|(n, row)| {
            let source = clean(row.get("source"))?;
            let record = row.get("record").cloned().unwrap_or(Value::Null);
            let record = serde_json::to_string_pretty(&record).unwrap_or_default();
            Some(SourceRow {
                id: format!("provenance://{seq}/{n}"),
                source,
                kind: clean(row.get("kind")),
                count: row.get("count").and_then(Value::as_u64),
                fetched: clean(row.get("fetched")),
                status: clean(row.get("status")),
                record: sanitize_for_render(&record),
            })
        })
        .collect()
}

/// The descriptor rows on a result card.
#[must_use]
pub fn descriptor_rows(data: Option<&Value>) -> Vec<DescriptorRow> {
    let Some(rows) = data
        .and_then(|d| d.get("descriptors"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let name = clean(row.get("name"))?;
            let value = match row.get("value") {
                None | Some(Value::Null) => "not computed".to_string(),
                Some(Value::Number(n)) => n.to_string(),
                Some(other) => clean(Some(other)).unwrap_or_else(|| "not computed".to_string()),
            };
            Some(DescriptorRow {
                name,
                value,
                unit: clean(row.get("unit")),
                origin: clean(row.get("origin")),
            })
        })
        .collect()
}

impl SourceRow {
    /// The full record the panel shows: the table's cells, then the tool's
    /// own entry for this source.
    #[must_use]
    pub fn panel_text(&self) -> String {
        let mut out = format!("source:   {}\n", self.source);
        out.push_str(&format!(
            "kind:     {}\n",
            self.kind.as_deref().unwrap_or("not reported")
        ));
        out.push_str(&format!(
            "count:    {}\n",
            self.count
                .map(|c| c.to_string())
                .unwrap_or_else(|| "not reported".to_string())
        ));
        out.push_str(&format!(
            "fetched:  {}\n",
            self.fetched.as_deref().unwrap_or("not reported")
        ));
        if let Some(status) = &self.status {
            out.push_str(&format!("status:   {status}\n"));
        }
        out.push_str("\nrecord as the tool reported it\n");
        out.push_str(&self.record);
        out
    }
}

/// Why a result carries the evidence class it shows — the class's meaning,
/// or the fact that the tool declared none.
#[must_use]
pub fn evidence_reason(class: Option<EvidenceClass>) -> &'static str {
    match class {
        None => "the tool declared no evidence class",
        Some(EvidenceClass::Indeterminate) => "model assertion with no grounding, or a failed call",
        Some(EvidenceClass::Research) => "extracted from literature, not independently verified",
        Some(EvidenceClass::Screening) => "computed by a cited method",
        Some(EvidenceClass::ReferenceValidated) => "executed or measured with reference evidence",
    }
}

/// What the table says when the tool stamped no source at all.
#[must_use]
pub fn not_reported_line(tool_name: &str) -> String {
    format!("SOURCE NOT REPORTED BY {tool_name}")
}

/// Column widths for a table `width` columns wide. Count, evidence and the
/// fetch time are fixed; source and kind share what is left, never below a
/// floor that still reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub source: usize,
    pub kind: usize,
    pub evidence: usize,
}

pub const COUNT_W: usize = 5;
/// `2026-09-02 14:10 UTC` — a fetch time to the minute, zone said.
pub const FETCHED_W: usize = 20;
const FLOOR: usize = 8;
/// An origin column narrower than this cuts words in half, so below it the
/// origin is drawn full-width under its descriptor instead.
const MIN_INLINE_ORIGIN: usize = 24;

#[must_use]
pub fn layout(width: usize, badge: &str) -> Layout {
    let evidence = badge.width().max("EVIDENCE".len());
    let fixed = COUNT_W + evidence + FETCHED_W + 4;
    let free = width.saturating_sub(fixed).max(FLOOR * 2);
    let source = (free * 2 / 5).max(FLOOR);
    let kind = (free - source).max(FLOOR);
    Layout {
        source,
        kind,
        evidence,
    }
}

/// `text` fitted into exactly `w` columns: clipped with an ellipsis when it
/// is wider, padded when it is narrower.
#[must_use]
pub fn fit(text: &str, w: usize) -> String {
    if text.width() <= w {
        let pad = w - text.width();
        return format!("{text}{}", " ".repeat(pad));
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let cw = UnicodeWidthStr::width(ch.encode_utf8(&mut [0; 4]) as &str);
        if used + cw > w.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    let pad = w.saturating_sub(out.width());
    format!("{out}{}", " ".repeat(pad))
}

/// The table's header line.
#[must_use]
pub fn header_line(l: Layout) -> String {
    format!(
        "{} {} {:>COUNT_W$} {} {}",
        fit("SOURCE", l.source),
        fit("KIND OF DATA", l.kind),
        "COUNT",
        fit("EVIDENCE", l.evidence),
        "FETCHED"
    )
}

/// One row as two pieces: the SOURCE cell, which is drawn as an openable
/// reference, and the rest of the line.
#[must_use]
pub fn row_cells(row: &SourceRow, badge: &str, l: Layout) -> (String, String) {
    let count = row
        .count
        .map(|c| c.to_string())
        .unwrap_or_else(|| "—".to_string());
    let fetched = row
        .fetched
        .as_deref()
        .map(short_time)
        .unwrap_or_else(|| "not reported".to_string());
    let kind = row.kind.as_deref().unwrap_or("kind not reported");
    (
        fit(&row.source, l.source),
        format!(
            " {} {:>COUNT_W$} {} {}",
            fit(kind, l.kind),
            fit(&count, COUNT_W).trim_end(),
            fit(badge, l.evidence),
            fetched
        ),
    )
}

/// A timestamp to the minute; anything that is not one is shown as said.
fn short_time(stamp: &str) -> String {
    let looks_iso =
        stamp.len() >= 16 && stamp.as_bytes()[4] == b'-' && stamp.as_bytes()[10] == b'T';
    if looks_iso {
        format!("{} UTC", stamp[..16].replace('T', " "))
    } else {
        fit(stamp, FETCHED_W).trim_end().to_string()
    }
}

/// The one-line reason under the table.
#[must_use]
pub fn reason_line(badge: &str, class: Option<EvidenceClass>) -> String {
    format!("evidence {badge}: {}", evidence_reason(class))
}

pub const DESCRIPTOR_NAME_W: usize = 22;
pub const DESCRIPTOR_VALUE_W: usize = 12;
// Wide enough for the units materials tools actually emit —
// "electrons/atom" (14) and "dimensionless" (13) are the long ones. At 12
// the first rendered as "electrons/a…", which is not a unit anybody can
// read, and a clipped unit misstates what the number is.
pub const DESCRIPTOR_UNIT_W: usize = 14;

/// The descriptor card's header line.
#[must_use]
pub fn descriptor_header(width: usize) -> String {
    let head = format!(
        "{} {} {}",
        fit("DESCRIPTOR", DESCRIPTOR_NAME_W),
        fit("VALUE", DESCRIPTOR_VALUE_W),
        fit("UNIT", DESCRIPTOR_UNIT_W)
    );
    // Only advertise the column when the origin is actually drawn in one.
    // Narrower than that, the origin gets its own full-width lines below each
    // descriptor, and a "COMPUTED FROM" heading there pointed at nothing and
    // spilled its second word onto a line of its own.
    if width.saturating_sub(head.width() + 1) >= MIN_INLINE_ORIGIN {
        format!("{head} COMPUTED FROM")
    } else {
        head.trim_end().to_string()
    }
}

/// One descriptor as lines: name, value, unit, and the origin the tool gave
/// — or the fact that it gave none. An origin longer than the column WRAPS
/// onto continuation lines under it, so the table a number came from is
/// never clipped to a name a reader cannot look up.
#[must_use]
pub fn descriptor_lines(row: &DescriptorRow, width: usize) -> Vec<String> {
    let lead = format!(
        "{} {} {} ",
        fit(&row.name, DESCRIPTOR_NAME_W),
        fit(&row.value, DESCRIPTOR_VALUE_W),
        fit(row.unit.as_deref().unwrap_or("—"), DESCRIPTOR_UNIT_W)
    );
    // A column narrower than this cannot hold a source name without cutting
    // words in half — "Takeuchi & Inoue (2005) pair table" became
    // "(2005) pai". Below it the origin gets the full width on its own lines
    // instead, indented under the descriptor it belongs to.
    const BLOCK_INDENT: usize = 4;
    let inline_room = width.saturating_sub(lead.width());
    let inline = inline_room >= MIN_INLINE_ORIGIN;
    let room = if inline {
        inline_room
    } else {
        width.saturating_sub(BLOCK_INDENT).max(FLOOR)
    };
    let origin = row
        .origin
        .as_deref()
        .unwrap_or("ORIGIN NOT REPORTED BY THE TOOL");
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in origin.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if candidate.width() <= room || current.is_empty() {
            current = candidate;
        } else {
            chunks.push(std::mem::take(&mut current));
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    let indent = " ".repeat(if inline { lead.width() } else { BLOCK_INDENT });
    let mut lines: Vec<String> = Vec::new();
    if !inline {
        lines.push(lead.trim_end().to_string());
    }
    for (n, chunk) in chunks.into_iter().enumerate() {
        let chunk = fit(&chunk, room).trim_end().to_string();
        if inline && n == 0 {
            lines.push(format!("{lead}{chunk}"));
        } else {
            lines.push(format!("{indent}{chunk}"));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card() -> Value {
        json!({
            "sources": [
                {"source": "Materials Project", "kind": "crystal structure and computed properties",
                 "count": 1, "fetched": "2026-09-02T14:10:03+00:00", "status": "success",
                 "record": {"endpoint": "https://api.materialsproject.org", "status": "success"}},
                {"source": "OQMD\u{1b}[31m", "count": null, "record": {"status": "timeout"}}
            ],
            "descriptors": [
                {"name": "VEC", "value": 8.0, "unit": "electrons/atom",
                 "origin": "valence electron concentration table _VEC (Guo & Liu 2011)"},
                {"name": "omega", "value": null, "unit": "dimensionless"}
            ]
        })
    }

    /// Rows are read off the card with their ids, every string sanitized,
    /// every absence kept as an absence.
    #[test]
    fn rows_come_off_the_card_clean_and_with_their_ids() {
        let rows = source_rows(Some(&card()), 7);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "provenance://7/0");
        assert_eq!(rows[0].source, "Materials Project");
        assert_eq!(rows[0].count, Some(1));
        assert_eq!(rows[0].status.as_deref(), Some("success"));
        assert!(rows[0].record.contains("api.materialsproject.org"));
        assert_eq!(rows[1].id, "provenance://7/1");
        assert!(!rows[1].source.contains('\u{1b}'), "{:?}", rows[1].source);
        assert_eq!(rows[1].kind, None);
        assert_eq!(rows[1].count, None);
        assert!(source_rows(None, 1).is_empty());
        assert!(source_rows(Some(&json!({"summary": "x"})), 1).is_empty());
        let d = descriptor_rows(Some(&card()));
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].value, "8.0");
        assert_eq!(d[1].value, "not computed");
        assert_eq!(d[1].origin, None);
    }

    /// The panel shows the cells and then the tool's own record.
    #[test]
    fn the_panel_text_carries_the_record() {
        let rows = source_rows(Some(&card()), 1);
        let text = rows[0].panel_text();
        assert!(text.starts_with("source:   Materials Project\n"), "{text}");
        assert!(text.contains("fetched:  2026-09-02T14:10:03+00:00"));
        assert!(text.contains("\"endpoint\": \"https://api.materialsproject.org\""));
        let silent = rows[1].panel_text();
        assert!(silent.contains("kind:     not reported"));
        assert!(silent.contains("fetched:  not reported"));
    }

    /// Every row fits the width it was laid out for, source and kind
    /// clipped with an ellipsis rather than spilling, and the fixed
    /// columns stay where the header put them.
    #[test]
    fn rows_fit_their_width_and_the_columns_line_up() {
        let rows = source_rows(Some(&card()), 1);
        for width in [96usize, 64] {
            let badge = "[YELLOW screening]";
            let l = layout(width, badge);
            let header = header_line(l);
            let (cell, rest) = row_cells(&rows[0], badge, l);
            let line = format!("{cell}{rest}");
            assert_eq!(cell.width(), l.source, "{width}: {cell:?}");
            assert!(line.width() <= width.max(60), "{width}: {line:?}");
            // The header is ASCII, so its byte offset is a column offset;
            // the row is not — it clips with "…" — so walk columns, never
            // bytes. Slicing bytes here panicked mid-ellipsis.
            // The header clips with "…" too at narrow widths, so its byte
            // offset is not its column offset either. Measure both in columns.
            let count_col = header[..header.find("COUNT").unwrap()].width();
            let mut col = 0usize;
            let mut cell_at_count = String::new();
            for ch in line.chars() {
                if col >= count_col && col < count_col + COUNT_W {
                    cell_at_count.push(ch);
                }
                col += ch.to_string().width();
            }
            assert_eq!(cell_at_count.trim(), "1", "{line:?}");
            assert!(line.contains("2026-09-02 14:10 UTC"), "{line:?}");
        }
        let l = layout(64, "[YELLOW screening]");
        let (cell, _) = row_cells(&rows[0], "[YELLOW screening]", l);
        assert!(
            cell.trim_end().ends_with('…'),
            "a clipped cell says so: {cell:?}"
        );
        // At a readable width the phrase is there whole. Squeezed, it is
        // clipped like any other cell — still said, still visibly clipped,
        // never blank, and the openable panel carries it in full.
        let wide = layout(120, "[unclassified]");
        assert!(
            row_cells(&rows[1], "[unclassified]", wide)
                .1
                .contains("kind not reported"),
            "an unreported kind is said, not blank"
        );
        let squeezed = row_cells(&rows[1], "[unclassified]", layout(64, "[unclassified]")).1;
        assert!(squeezed.contains("kind not"), "{squeezed:?}");
        assert!(
            squeezed.contains('…'),
            "a clipped cell says so: {squeezed:?}"
        );
    }

    /// The reason names the class's meaning, or the tool's silence.
    #[test]
    fn the_reason_is_the_class_s_meaning_or_the_tool_s_silence() {
        assert_eq!(
            evidence_reason(Some(EvidenceClass::Screening)),
            "computed by a cited method"
        );
        assert!(evidence_reason(None).contains("declared no evidence class"));
        assert_eq!(
            reason_line("[unclassified]", None),
            "evidence [unclassified]: the tool declared no evidence class"
        );
        assert_eq!(
            not_reported_line("execute_python"),
            "SOURCE NOT REPORTED BY execute_python"
        );
    }

    /// A descriptor line names the origin, or says the tool gave none.
    #[test]
    fn a_descriptor_line_names_its_origin_or_the_lack_of_one() {
        let d = descriptor_rows(Some(&card()));
        let vec_lines = descriptor_lines(&d[0], 96);
        assert!(vec_lines[0].starts_with("VEC "), "{vec_lines:?}");
        assert!(vec_lines[0].contains("8.0"));
        assert!(vec_lines[0].contains("electrons/atom"));
        // Rejoin without the wrap indent — that is what the eye does. Joining
        // the padded lines splits the origin at whatever column it wrapped.
        let unwrapped = |ls: &[String]| ls.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
        assert!(
            unwrapped(&vec_lines).contains("(Guo & Liu 2011)"),
            "the origin must survive whole: {vec_lines:?}"
        );
        assert!(vec_lines.iter().all(|l| l.width() <= 96), "{vec_lines:?}");
        // Narrower, a long origin wraps under its column rather than
        // clipping to a name nobody can look up.
        let narrow = descriptor_lines(&d[0], 70);
        assert!(narrow.len() > 1, "{narrow:?}");
        assert!(narrow.iter().all(|l| l.width() <= 70), "{narrow:?}");
        assert!(narrow[1].starts_with("    "), "{narrow:?}");
        assert!(
            unwrapped(&narrow).contains("(Guo & Liu 2011)"),
            "{narrow:?}"
        );
        let omega = descriptor_lines(&d[1], 96);
        assert!(omega[0].contains("not computed"));
        assert!(
            omega.join(" ").contains("ORIGIN NOT REPORTED BY THE TOOL"),
            "{omega:?}"
        );
    }
}
