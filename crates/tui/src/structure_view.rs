// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! A crystal structure as a reader sees it: the cell, the atoms in it, and the
//! numbers that define it — never the CIF text.
//!
//! A `cache://…` reference resolves to a CIF. Showing that CIF is showing a
//! file format, and nobody hovers a formula to read a file format. This module
//! turns the CIF into what the formula stands for — a lattice, a space group,
//! sites with species and occupancies — and draws the unit cell with its atoms
//! as a Braille projection, so the thing on screen is the structure.
//!
//! Nothing here guesses. A CIF that lists only an asymmetric unit under a
//! space group with symmetry operations is shown with exactly the sites it
//! lists, and the panel SAYS the symmetry is not expanded. A CIF that says
//! nothing about symmetry says "symmetry unknown" rather than asserting
//! either. A row that cannot be read is an error naming itself, never an
//! empty site list presented as a structure with no atoms.

use ratatui::style::Color;
use ratatui::widgets::canvas::{Context, Line as CanvasLine, Points};

/// Braille dots per terminal cell: 2 across, 4 down. The projection is
/// isotropic, so the bounds must be too — a cubic cell that draws as a slab
/// is a lie about the material, and the anisotropy was measured at 2.3–2.8×
/// before this was accounted for.
const DOTS_X: f64 = 2.0;
const DOTS_Y: f64 = 4.0;

/// Fewer rows than this cannot hold a cell: the projection collapses and what
/// is left reads as a smear that a viewer would take for the structure.
/// Below it the panel shows the numbers and says what it needs.
pub const MIN_CANVAS_ROWS: u16 = 6;

/// One atomic site as the CIF lists it — one ROW of the atom loop, which is
/// not the same thing as one crystallographic position (a disordered site is
/// several rows sharing one position).
#[derive(Debug, Clone, PartialEq)]
pub struct Site {
    pub label: String,
    pub species: String,
    pub frac: [f64; 3],
    /// Fraction of this position the species occupies. 1.0 unless the CIF
    /// says otherwise.
    pub occupancy: f64,
}

/// A crystallographic position and everything that sits on it.
#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub frac: [f64; 3],
    /// `(species, occupancy)` in the order the CIF listed them.
    pub occupants: Vec<(String, f64)>,
}

impl Position {
    /// The species with the largest share — what the drawing colours it by.
    pub fn dominant(&self) -> &str {
        self.occupants
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(s, _)| s.as_str())
            .unwrap_or("")
    }
}

/// What the CIF says about symmetry — three states, because "nothing said"
/// and "a space group whose operations were not applied" are different facts
/// and only one of them justifies the words "asymmetric unit".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymmetryState {
    /// P 1, or the only listed operation is the identity: the sites ARE the
    /// cell.
    WholeCell,
    /// A named space group whose operations this view does not apply.
    AsymmetricUnit,
    /// The CIF says nothing about symmetry at all.
    Unknown,
}

/// The parsed structure: what the reader is shown.
#[derive(Debug, Clone, PartialEq)]
pub struct StructureView {
    pub formula: Option<String>,
    pub space_group: Option<String>,
    pub space_group_number: Option<u32>,
    /// a, b, c in Å.
    pub lengths: [f64; 3],
    /// α, β, γ in degrees.
    pub angles: [f64; 3],
    /// Row vectors a, b, c in Cartesian Å: a along x, b in the xy plane.
    pub lattice: [[f64; 3]; 3],
    pub sites: Vec<Site>,
    pub symmetry: SymmetryState,
}

// ── CIF tokens ──────────────────────────────────────────────────────
//
// A CIF is a token stream, not a set of lines. Reading it line-by-line was
// wrong in three ways that all corrupted the numbers on screen: a `;`-block
// of prose was read as data (a `_cell_length_a` inside a comment section
// drew a 999 Å cell), a loop row wrapped across physical lines dropped every
// atom silently, and an inline `# comment` on a cell length made the whole
// cell unreadable. Tokenizing removes all three by construction.

#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// `_cell_length_a`, lowercased without its leading underscore.
    Tag(String),
    Value(String),
    Loop,
    /// `data_…` / `save_…` — a block boundary, never data.
    Block,
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        // A `;` in column 1 opens a text field that runs to the next `;` in
        // column 1. Everything between is ONE value — prose, a citation, a
        // whole paragraph containing things that look like tags.
        if let Some(first) = line.strip_prefix(';') {
            let mut body = first.to_string();
            for l in lines.by_ref() {
                if l.starts_with(';') {
                    break;
                }
                body.push('\n');
                body.push_str(l);
            }
            out.push(Token::Value(body));
            continue;
        }
        let mut chars = line.chars().peekable();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                chars.next();
                continue;
            }
            // `#` at a token boundary comments out the rest of the line.
            if c == '#' {
                break;
            }
            if c == '\'' || c == '"' {
                let quote = c;
                chars.next();
                let mut value = String::new();
                while let Some(ch) = chars.next() {
                    if ch == quote && chars.peek().is_none_or(|n| n.is_whitespace()) {
                        break;
                    }
                    value.push(ch);
                }
                out.push(Token::Value(value));
                continue;
            }
            let mut word = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() {
                    break;
                }
                word.push(ch);
                chars.next();
            }
            let lower = word.to_ascii_lowercase();
            if lower == "loop_" {
                out.push(Token::Loop);
            } else if lower.starts_with("data_") || lower.starts_with("save_") {
                out.push(Token::Block);
            } else if let Some(tag) = word.strip_prefix('_') {
                out.push(Token::Tag(tag.to_ascii_lowercase()));
            } else {
                out.push(Token::Value(word));
            }
        }
    }
    out
}

fn strip_uncertainty(token: &str) -> &str {
    token.split('(').next().unwrap_or(token)
}

fn parse_f64(value: &str) -> Option<f64> {
    strip_uncertainty(value.trim()).parse::<f64>().ok()
}

/// Element symbol from a CIF label or type symbol — `Fe12`, `O2-`, `FE1`,
/// `ti` — as its canonical form. Case is not identity in a CIF written by
/// hand or by a tool that shouts: read as-is, `FE` became fluorine.
fn species_from_label(label: &str) -> String {
    let letters: String = label
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .take(2)
        .collect();
    if letters.is_empty() {
        return label.to_string();
    }
    let mut out = String::new();
    for (i, ch) in letters.chars().enumerate() {
        if i == 0 {
            out.extend(ch.to_uppercase());
        } else {
            out.extend(ch.to_lowercase());
        }
    }
    out
}

/// Row vectors for the cell: a along x, b in the xy plane, c completing it.
pub fn lattice_from_params(lengths: [f64; 3], angles: [f64; 3]) -> [[f64; 3]; 3] {
    let [a, b, c] = lengths;
    let [alpha, beta, gamma] = angles.map(f64::to_radians);
    let (ca, cb, cg, sg) = (alpha.cos(), beta.cos(), gamma.cos(), gamma.sin());
    let cx = c * cb;
    let cy = if sg.abs() > 1e-12 {
        c * (ca - cb * cg) / sg
    } else {
        0.0
    };
    let cz = (c * c - cx * cx - cy * cy).max(0.0).sqrt();
    [[a, 0.0, 0.0], [b * cg, b * sg, 0.0], [cx, cy, cz]]
}

/// Cartesian position of a fractional coordinate in this lattice.
pub fn cartesian(lattice: [[f64; 3]; 3], frac: [f64; 3]) -> [f64; 3] {
    let mut p = [0.0; 3];
    for (i, f) in frac.iter().enumerate() {
        for k in 0..3 {
            p[k] += f * lattice[i][k];
        }
    }
    p
}

/// Fixed oblique viewpoint: the cell is turned 35° about its c axis and the
/// camera tilted 25° down, so a cube shows a top face and two sides. The
/// projection is orthographic and linear, which is what makes the cell edges
/// straight and the atoms sit where their fractional coordinates say.
const YAW_DEG: f64 = 35.0;
const TILT_DEG: f64 = 25.0;

/// Screen coordinates (x right, y up) of a Cartesian point.
pub fn project(p: [f64; 3]) -> (f64, f64) {
    let (yaw, tilt) = (YAW_DEG.to_radians(), TILT_DEG.to_radians());
    let x = p[0] * yaw.cos() - p[1] * yaw.sin();
    let y_depth = p[0] * yaw.sin() + p[1] * yaw.cos();
    let y = p[2] * tilt.cos() + y_depth * tilt.sin();
    (x, y)
}

/// The twelve edges of the parallelepiped, as Cartesian endpoints.
pub fn cell_edges(lattice: [[f64; 3]; 3]) -> [([f64; 3], [f64; 3]); 12] {
    let corner = |i: u8| {
        cartesian(
            lattice,
            [
                f64::from(i & 1),
                f64::from((i >> 1) & 1),
                f64::from((i >> 2) & 1),
            ],
        )
    };
    let mut edges = [([0.0; 3], [0.0; 3]); 12];
    let mut n = 0;
    for i in 0u8..8 {
        for bit in [1u8, 2, 4] {
            if i & bit == 0 {
                edges[n] = (corner(i), corner(i | bit));
                n += 1;
            }
        }
    }
    edges
}

/// A colour per element, in the spirit of the CPK convention (the colours a
/// materials scientist already reads): oxygen red, nitrogen blue, carbon
/// grey, metals in their conventional families. Unknown symbols get a stable
/// colour from their name so a legend still tells them apart.
pub fn species_color(symbol: &str) -> Color {
    match symbol {
        "H" => Color::Rgb(230, 230, 230),
        "C" => Color::Rgb(144, 144, 144),
        "N" => Color::Rgb(48, 80, 248),
        "O" => Color::Rgb(255, 13, 13),
        "F" | "Cl" => Color::Rgb(144, 224, 80),
        "S" => Color::Rgb(255, 255, 48),
        "P" => Color::Rgb(255, 128, 0),
        "Si" => Color::Rgb(240, 200, 160),
        "Fe" => Color::Rgb(224, 102, 51),
        "Ti" => Color::Rgb(191, 194, 199),
        "Al" => Color::Rgb(191, 166, 166),
        "Ni" => Color::Rgb(80, 208, 80),
        "Co" => Color::Rgb(240, 144, 160),
        "Cr" => Color::Rgb(138, 153, 199),
        "Cu" => Color::Rgb(200, 128, 51),
        "Zn" => Color::Rgb(125, 128, 176),
        "Mo" | "W" | "Ta" | "Nb" => Color::Rgb(84, 181, 181),
        "Mg" => Color::Rgb(138, 255, 0),
        "Ca" => Color::Rgb(61, 255, 0),
        "Na" | "K" | "Li" => Color::Rgb(171, 92, 242),
        "Zr" | "Hf" => Color::Rgb(148, 224, 224),
        "Mn" => Color::Rgb(156, 122, 199),
        "V" => Color::Rgb(166, 166, 171),
        "Au" => Color::Rgb(255, 209, 35),
        "Ag" => Color::Rgb(192, 192, 192),
        "Pt" | "Pd" => Color::Rgb(208, 208, 224),
        _ => {
            let h = symbol
                .bytes()
                .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
            Color::Indexed(u8::try_from(16 + (h % 200)).unwrap_or(75))
        }
    }
}

/// Parse a CIF into what the panel shows. Errors name what is missing or
/// what could not be read: a CIF without a cell is not a structure PRISM can
/// draw, and an atom loop whose rows do not add up is a parse failure, never
/// a structure that happens to have no atoms.
pub fn parse_cif(text: &str) -> Result<StructureView, String> {
    let tokens = tokenize(text);
    let mut lengths = [None::<f64>; 3];
    let mut angles = [None::<f64>; 3];
    let mut formula = None;
    let mut space_group = None;
    let mut space_group_number = None;
    let mut symops: Vec<String> = Vec::new();
    let mut sites = Vec::new();
    let mut saw_atom_loop = false;

    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Loop => {
                i += 1;
                let mut tags: Vec<String> = Vec::new();
                while let Some(Token::Tag(tag)) = tokens.get(i) {
                    tags.push(tag.clone());
                    i += 1;
                }
                // Every value until the next tag, loop or block header
                // belongs to this loop — however the file wrapped them.
                let mut values: Vec<String> = Vec::new();
                while let Some(Token::Value(value)) = tokens.get(i) {
                    values.push(value.clone());
                    i += 1;
                }
                if tags.is_empty() {
                    continue;
                }
                let column = |name: &str| tags.iter().position(|t| t == name);
                let atom_loop = column("atom_site_fract_x").is_some();
                let symop_loop = column("space_group_symop_operation_xyz")
                    .or_else(|| column("symmetry_equiv_pos_as_xyz"))
                    .is_some();
                if !atom_loop && !symop_loop {
                    continue;
                }
                if !values.len().is_multiple_of(tags.len()) {
                    return Err(format!(
                        "CIF loop has {} values for {} columns — the rows do not add up",
                        values.len(),
                        tags.len()
                    ));
                }
                let rows: Vec<&[String]> = values.chunks(tags.len()).collect();
                if atom_loop {
                    saw_atom_loop = true;
                    let (fx, fy, fz) = (
                        column("atom_site_fract_x").unwrap_or(0),
                        column("atom_site_fract_y")
                            .ok_or("CIF atom loop has no _atom_site_fract_y")?,
                        column("atom_site_fract_z")
                            .ok_or("CIF atom loop has no _atom_site_fract_z")?,
                    );
                    let label_col = column("atom_site_label");
                    let type_col = column("atom_site_type_symbol");
                    let occ_col = column("atom_site_occupancy");
                    for row in rows {
                        let cell = |c: usize| row.get(c).map(String::as_str).unwrap_or("");
                        let (Some(x), Some(y), Some(z)) = (
                            parse_f64(cell(fx)),
                            parse_f64(cell(fy)),
                            parse_f64(cell(fz)),
                        ) else {
                            return Err(format!(
                                "CIF atom row has unreadable coordinates: {:?}",
                                row.join(" ")
                            ));
                        };
                        let label = label_col.map(cell).unwrap_or("").to_string();
                        let species = type_col
                            .map(cell)
                            .filter(|s| !s.is_empty())
                            .map(species_from_label)
                            .unwrap_or_else(|| species_from_label(&label));
                        sites.push(Site {
                            label: if label.is_empty() {
                                species.clone()
                            } else {
                                label
                            },
                            species,
                            frac: [x, y, z],
                            occupancy: occ_col
                                .map(cell)
                                .and_then(parse_f64)
                                .filter(|o| o.is_finite() && *o > 0.0)
                                .unwrap_or(1.0),
                        });
                    }
                } else if let Some(op) = column("space_group_symop_operation_xyz")
                    .or_else(|| column("symmetry_equiv_pos_as_xyz"))
                {
                    for row in rows {
                        if let Some(value) = row.get(op) {
                            symops.push(value.replace(' ', "").to_lowercase());
                        }
                    }
                }
                continue;
            }
            Token::Tag(tag) => {
                let value = match tokens.get(i + 1) {
                    Some(Token::Value(value)) => value.clone(),
                    _ => {
                        i += 1;
                        continue;
                    }
                };
                match tag.as_str() {
                    "cell_length_a" => lengths[0] = parse_f64(&value),
                    "cell_length_b" => lengths[1] = parse_f64(&value),
                    "cell_length_c" => lengths[2] = parse_f64(&value),
                    "cell_angle_alpha" => angles[0] = parse_f64(&value),
                    "cell_angle_beta" => angles[1] = parse_f64(&value),
                    "cell_angle_gamma" => angles[2] = parse_f64(&value),
                    "chemical_formula_sum" => formula = Some(value.trim().to_string()),
                    "space_group_name_h-m_alt"
                    | "symmetry_space_group_name_h-m"
                    | "space_group_name_h-m" => space_group = Some(value.trim().to_string()),
                    "space_group_it_number" | "symmetry_int_tables_number" => {
                        space_group_number = value.trim().parse().ok();
                    }
                    _ => {}
                }
                i += 2;
            }
            _ => i += 1,
        }
    }

    let lengths = [
        lengths[0].ok_or("CIF has no _cell_length_a")?,
        lengths[1].ok_or("CIF has no _cell_length_b")?,
        lengths[2].ok_or("CIF has no _cell_length_c")?,
    ];
    let angles = [
        angles[0].unwrap_or(90.0),
        angles[1].unwrap_or(90.0),
        angles[2].unwrap_or(90.0),
    ];
    if lengths.iter().any(|l| !(l.is_finite() && *l > 0.0)) {
        return Err("CIF cell lengths are not positive numbers".to_string());
    }
    // An atom loop that produced nothing is a parse failure. Reporting it as
    // a structure with no atoms would draw an empty cell and call it the
    // material.
    if saw_atom_loop && sites.is_empty() {
        return Err("CIF lists an atom loop but no site could be read from it".to_string());
    }
    let named_p1 = space_group
        .as_deref()
        .is_some_and(|s| s.replace(' ', "").eq_ignore_ascii_case("P1"))
        || space_group_number == Some(1);
    let only_identity = !symops.is_empty() && symops.iter().all(|s| s == "x,y,z");
    let said_nothing = space_group.is_none() && space_group_number.is_none() && symops.is_empty();
    let symmetry = if named_p1 || only_identity {
        SymmetryState::WholeCell
    } else if said_nothing {
        SymmetryState::Unknown
    } else {
        SymmetryState::AsymmetricUnit
    };
    Ok(StructureView {
        formula,
        space_group,
        space_group_number,
        lengths,
        angles,
        lattice: lattice_from_params(lengths, angles),
        sites,
        symmetry,
    })
}

impl StructureView {
    /// Crystallographic positions: rows sharing one point in the cell are one
    /// site with several occupants, which is what a disordered alloy is.
    pub fn positions(&self) -> Vec<Position> {
        let mut out: Vec<Position> = Vec::new();
        for site in &self.sites {
            let same = out.iter_mut().find(|p| {
                p.frac
                    .iter()
                    .zip(site.frac.iter())
                    .all(|(a, b)| (a - b).abs() < 1e-4)
            });
            match same {
                Some(position) => position
                    .occupants
                    .push((site.species.clone(), site.occupancy)),
                None => out.push(Position {
                    frac: site.frac,
                    occupants: vec![(site.species.clone(), site.occupancy)],
                }),
            }
        }
        out
    }

    /// True when any position is shared or partly filled.
    pub fn is_disordered(&self) -> bool {
        self.positions().iter().any(|p| {
            p.occupants.len() > 1 || p.occupants.iter().map(|(_, o)| o).sum::<f64>() < 1.0 - 1e-6
        })
    }

    /// Species in first-seen order with their total occupancy across the
    /// cell — for an ordered structure that is a count of atoms, and for a
    /// disordered one it is the share each element holds.
    pub fn species_totals(&self) -> Vec<(String, f64)> {
        let mut out: Vec<(String, f64)> = Vec::new();
        for site in &self.sites {
            if let Some(entry) = out.iter_mut().find(|(sym, _)| *sym == site.species) {
                entry.1 += site.occupancy;
            } else {
                out.push((site.species.clone(), site.occupancy));
            }
        }
        out
    }

    /// What the CIF says about symmetry, when that needs saying. `None` for a
    /// cell whose listed sites are the whole cell — nothing to disclose.
    pub fn symmetry_note(&self) -> Option<&'static str> {
        match self.symmetry {
            SymmetryState::WholeCell => None,
            SymmetryState::AsymmetricUnit => {
                Some("asymmetric unit — symmetry operations not applied")
            }
            SymmetryState::Unknown => Some("symmetry unknown — sites shown exactly as listed"),
        }
    }

    /// The numbers a reader wants first: formula, sites, space group, cell.
    /// The symmetry disclosure gets its OWN line — folded onto the formula
    /// line it was clipped by the panel width in every case, so the sentence
    /// the reader most needs never survived to the screen.
    pub fn header_lines(&self) -> Vec<String> {
        let sg = match (&self.space_group, self.space_group_number) {
            (Some(name), Some(n)) => format!("{name} (No. {n})"),
            (Some(name), None) => name.clone(),
            (None, Some(n)) => format!("No. {n}"),
            (None, None) => "unknown".to_string(),
        };
        let positions = self.positions().len();
        let mut first = format!(
            "{}  ·  {positions} site{}",
            self.formula.as_deref().unwrap_or("formula unknown"),
            if positions == 1 { "" } else { "s" }
        );
        if self.is_disordered() {
            first.push_str("  ·  disordered");
        }
        let mut out = vec![first];
        if let Some(note) = self.symmetry_note() {
            out.push(note.to_string());
        }
        out.push(format!("space group  {sg}"));
        out.push(format!(
            "a b c  {:.3}  {:.3}  {:.3} Å",
            self.lengths[0], self.lengths[1], self.lengths[2]
        ));
        out.push(format!(
            "α β γ  {:.2}°  {:.2}°  {:.2}°",
            self.angles[0], self.angles[1], self.angles[2]
        ));
        out
    }

    /// Legend: one entry per species, in the colour the drawing uses. A whole
    /// number of atoms reads as a count; a share reads as a share.
    pub fn legend(&self) -> Vec<(String, Color)> {
        self.species_totals()
            .into_iter()
            .map(|(sym, total)| {
                let amount = if (total - total.round()).abs() < 1e-6 {
                    format!("{}", total.round() as i64)
                } else {
                    format!("{total:.2}")
                };
                let color = species_color(&sym);
                (format!("{sym}×{amount}"), color)
            })
            .collect()
    }

    /// The site table, bounded; the remainder is COUNTED, never silently cut.
    /// Occupancy is shown whenever the structure is disordered, because a
    /// table of four species on one position with no occupancies reads as
    /// four atoms.
    pub fn site_lines(&self, limit: usize) -> Vec<String> {
        let show_occupancy = self.is_disordered();
        let mut out = Vec::new();
        for s in self.sites.iter().take(limit) {
            let mut line = format!(
                "{:<6} {:<3} {:>7.4} {:>7.4} {:>7.4}",
                s.label, s.species, s.frac[0], s.frac[1], s.frac[2]
            );
            if show_occupancy {
                line.push_str(&format!(" {:>5.2}", s.occupancy));
            }
            out.push(line);
        }
        if self.sites.len() > limit {
            out.push(format!("  +{} more rows", self.sites.len() - limit));
        }
        out
    }

    /// Screen bounds that contain every cell corner and every atom, with a
    /// margin so edge atoms are not on the frame.
    pub fn bounds(&self) -> ([f64; 2], [f64; 2]) {
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        for (p, q) in cell_edges(self.lattice) {
            for pt in [p, q] {
                let (x, y) = project(pt);
                xs.push(x);
                ys.push(y);
            }
        }
        for s in &self.sites {
            let (x, y) = project(cartesian(self.lattice, s.frac));
            xs.push(x);
            ys.push(y);
        }
        let (x0, x1) = (
            xs.iter().cloned().fold(f64::INFINITY, f64::min),
            xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        );
        let (y0, y1) = (
            ys.iter().cloned().fold(f64::INFINITY, f64::min),
            ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        );
        let mx = ((x1 - x0) * 0.08).max(0.1);
        let my = ((y1 - y0) * 0.08).max(0.1);
        ([x0 - mx, x1 + mx], [y0 - my, y1 + my])
    }

    /// Bounds for a canvas of `cols`×`rows` cells with the SAME Å per dot in
    /// both directions. Without this the cell is stretched to whatever shape
    /// the pane happens to be and a cubic cell draws as a slab.
    pub fn balanced_bounds(&self, cols: u16, rows: u16) -> ([f64; 2], [f64; 2]) {
        let (xb, yb) = self.bounds();
        let dots_x = f64::from(cols.max(1)) * DOTS_X;
        let dots_y = f64::from(rows.max(1)) * DOTS_Y;
        let scale = ((xb[1] - xb[0]) / dots_x).max((yb[1] - yb[0]) / dots_y);
        let (half_x, half_y) = (scale * dots_x / 2.0, scale * dots_y / 2.0);
        let cx = f64::midpoint(xb[0], xb[1]);
        let cy = f64::midpoint(yb[0], yb[1]);
        ([cx - half_x, cx + half_x], [cy - half_y, cy + half_y])
    }

    /// Draw the cell edges and the atoms into a canvas context. One point per
    /// crystallographic position — a disordered site is one atom in the cell,
    /// not four stacked on the same dot.
    pub fn paint(&self, ctx: &mut Context<'_>, edge_color: Color) {
        for (p, q) in cell_edges(self.lattice) {
            let (x1, y1) = project(p);
            let (x2, y2) = project(q);
            ctx.draw(&CanvasLine {
                x1,
                y1,
                x2,
                y2,
                color: edge_color,
            });
        }
        // Atoms after edges, so they sit on top; one Points call per species
        // so each keeps its colour.
        let positions = self.positions();
        let mut species: Vec<&str> = Vec::new();
        for position in &positions {
            let dominant = position.dominant();
            if !species.contains(&dominant) {
                species.push(dominant);
            }
        }
        for symbol in species {
            let coords: Vec<(f64, f64)> = positions
                .iter()
                .filter(|p| p.dominant() == symbol)
                .map(|p| project(cartesian(self.lattice, p.frac)))
                .collect();
            ctx.draw(&Points {
                coords: &coords,
                color: species_color(symbol),
            });
        }
    }

    /// The drawing as text lines, for surfaces that show text rather than
    /// widgets (the Enter-key detail view). Colour is lost; the shape is not.
    /// Too short to draw is SAID, never drawn deformed.
    pub fn text_render(&self, width: u16, height: u16) -> Vec<String> {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::symbols::Marker;
        use ratatui::widgets::Widget;
        use ratatui::widgets::canvas::Canvas;
        if height < MIN_CANVAS_ROWS {
            return vec![short_canvas_note(height)];
        }
        let area = Rect::new(0, 0, width.max(1), height);
        let mut buf = Buffer::empty(area);
        let (xb, yb) = self.balanced_bounds(area.width, area.height);
        Canvas::default()
            .marker(Marker::Braille)
            .x_bounds(xb)
            .y_bounds(yb)
            .paint(|ctx| self.paint(ctx, Color::Reset))
            .render(area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }
}

/// What a pane too short for the cell says instead of drawing one.
pub fn short_canvas_note(rows: u16) -> String {
    format!(
        "cell not drawn — needs {} more row{} of height",
        MIN_CANVAS_ROWS - rows.min(MIN_CANVAS_ROWS),
        if MIN_CANVAS_ROWS - rows.min(MIN_CANVAS_ROWS) == 1 {
            ""
        } else {
            "s"
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIAL: &str = "\
data_TiAl
_chemical_formula_sum \"Al1 Ti1\"
_cell_length_a 4.005
_cell_length_b 4.005
_cell_length_c 4.171
_cell_angle_alpha 90.0
_cell_angle_beta 90.0
_cell_angle_gamma 90.0
_symmetry_space_group_name_H-M \"P 4/m m m\"
loop_
_atom_site_label
_atom_site_type_symbol
_atom_site_fract_x
_atom_site_fract_y
_atom_site_fract_z
Ti1 Ti 0.00000 0.00000 0.00000
Al1 Al 0.50000 0.50000 0.50000
";

    /// The ASE shape the shared cache actually holds: P 1, a symop loop
    /// with only the identity, type symbol first, uncertainty-free floats.
    const ASE_P1: &str = "\
data_image0
_chemical_formula_sum              \"Fe2\"
_cell_length_a       2.8665
_cell_length_b       2.8665
_cell_length_c       2.8665
_cell_angle_alpha    90.0
_cell_angle_beta     90.0
_cell_angle_gamma    90.0

_space_group_name_H-M_alt    \"P 1\"
_space_group_IT_number       1

loop_
  _space_group_symop_operation_xyz
  'x, y, z'

loop_
  _atom_site_type_symbol
  _atom_site_label
  _atom_site_symmetry_multiplicity
  _atom_site_fract_x
  _atom_site_fract_y
  _atom_site_fract_z
  _atom_site_occupancy
  Fe  Fe1       1.0  0.0  0.0  0.0  1.0000
  Fe  Fe2       1.0  0.5  0.5  0.5  1.0000
";

    #[test]
    fn a_cif_becomes_a_cell_a_space_group_and_sites() {
        let v = parse_cif(TIAL).expect("parses");
        assert_eq!(v.formula.as_deref(), Some("Al1 Ti1"));
        assert_eq!(v.space_group.as_deref(), Some("P 4/m m m"));
        assert_eq!(v.lengths, [4.005, 4.005, 4.171]);
        assert_eq!(v.angles, [90.0, 90.0, 90.0]);
        assert_eq!(v.sites.len(), 2);
        assert_eq!(v.sites[0].species, "Ti");
        assert_eq!(v.sites[1].species, "Al");
        assert_eq!(v.sites[1].frac, [0.5, 0.5, 0.5]);
        assert_eq!(v.symmetry, SymmetryState::AsymmetricUnit);
        // The disclosure is its OWN line: folded onto the formula line it was
        // clipped by the panel width in every case.
        let header = v.header_lines();
        assert!(
            header
                .iter()
                .any(|l| l == "asymmetric unit — symmetry operations not applied"),
            "{header:?}"
        );
        assert!(
            header.iter().all(|l| l.chars().count() <= 60),
            "a header line must fit a panel: {header:?}"
        );
    }

    #[test]
    fn the_cache_s_own_ase_cifs_parse_as_full_cells() {
        let v = parse_cif(ASE_P1).expect("parses");
        assert_eq!(v.formula.as_deref(), Some("Fe2"));
        assert_eq!(v.space_group_number, Some(1));
        assert_eq!(v.symmetry, SymmetryState::WholeCell);
        assert_eq!(v.symmetry_note(), None, "P 1 has nothing to disclose");
        assert_eq!(v.sites.len(), 2);
        assert_eq!(v.sites[1].label, "Fe2");
        assert_eq!(v.sites[1].species, "Fe");
        assert!(
            v.header_lines()[0].starts_with("Fe2  ·  2 sites"),
            "{:?}",
            v.header_lines()
        );
    }

    /// A `;`-delimited text field is prose, not data. Read line-by-line, a
    /// `_cell_length_a` inside a comment section drew a 999 Å cell.
    #[test]
    fn prose_inside_a_semicolon_block_is_never_read_as_the_cell() {
        // The prose comes AFTER the real cell: read as data it would
        // OVERWRITE the true value, which is the way round that corrupts.
        let cif = "\
data_x
_cell_length_a 4.0
_cell_length_b 4.0
_cell_length_c 4.0
_journal_name_full
;
The cell was refined against
_cell_length_a 999.0
in the original work, and
_atom_site_fract_x
values were taken from Table 2.
;
loop_
_atom_site_label
_atom_site_type_symbol
_atom_site_fract_x
_atom_site_fract_y
_atom_site_fract_z
Fe1 Fe 0.0 0.0 0.0
";
        let v = parse_cif(cif).expect("parses");
        assert_eq!(v.lengths, [4.0, 4.0, 4.0], "prose must not become the cell");
        assert_eq!(v.sites.len(), 1);
    }

    /// A loop row wrapped across physical lines is still one row. Read
    /// line-by-line, every atom vanished and the panel said "0 sites".
    #[test]
    fn a_loop_row_wrapped_across_lines_still_reads_as_one_row() {
        let cif = "\
data_x
_cell_length_a 2.87
_cell_length_b 2.87
_cell_length_c 2.87
loop_
  _atom_site_type_symbol
  _atom_site_label
  _atom_site_symmetry_multiplicity
  _atom_site_fract_x
  _atom_site_fract_y
  _atom_site_fract_z
  _atom_site_occupancy
  Fe  Fe1  1.0  0.0  0.0
  0.0  1.0000
  Fe  Fe2  1.0  0.5
  0.5  0.5  1.0000
";
        let v = parse_cif(cif).expect("parses");
        assert_eq!(v.sites.len(), 2, "{:?}", v.sites);
        assert_eq!(v.sites[1].frac, [0.5, 0.5, 0.5]);
    }

    /// An atom loop that yields nothing is a parse failure with a reason,
    /// never a structure that happens to have no atoms.
    #[test]
    fn an_unreadable_atom_row_is_an_error_not_an_empty_structure() {
        let cif = "\
data_x
_cell_length_a 2.87
_cell_length_b 2.87
_cell_length_c 2.87
loop_
_atom_site_label
_atom_site_type_symbol
_atom_site_fract_x
_atom_site_fract_y
_atom_site_fract_z
Fe1 Fe 0.0 0.0
";
        let err = parse_cif(cif).unwrap_err();
        assert!(err.contains("do not add up"), "{err}");

        // And a loop with columns but no rows at all: nothing is ragged, so
        // only the empty-site guard stands between this and an empty cell
        // drawn as if it were the material.
        let no_rows = "\
data_x
_cell_length_a 2.87
_cell_length_b 2.87
_cell_length_c 2.87
loop_
_atom_site_label
_atom_site_type_symbol
_atom_site_fract_x
_atom_site_fract_y
_atom_site_fract_z
";
        let err = parse_cif(no_rows).unwrap_err();
        assert!(err.contains("no site could be read"), "{err}");
    }

    /// A disordered FCC solid solution: one crystallographic site, four
    /// species at quarter occupancy. Counting rows called it four sites,
    /// drew four legend entries and one screen point.
    #[test]
    fn a_disordered_site_counts_once_and_says_so() {
        let cif = "\
data_hea
_chemical_formula_sum \"Cr0.25 Mn0.25 Fe0.25 Co0.25\"
_cell_length_a 3.59
_cell_length_b 3.59
_cell_length_c 3.59
_space_group_name_H-M_alt \"P 1\"
loop_
_atom_site_label
_atom_site_type_symbol
_atom_site_fract_x
_atom_site_fract_y
_atom_site_fract_z
_atom_site_occupancy
Cr1 Cr 0.0 0.0 0.0 0.25
Mn1 Mn 0.0 0.0 0.0 0.25
Fe1 Fe 0.0 0.0 0.0 0.25
Co1 Co 0.0 0.0 0.0 0.25
";
        let v = parse_cif(cif).expect("parses");
        assert_eq!(v.sites.len(), 4, "four rows");
        assert_eq!(v.positions().len(), 1, "one crystallographic site");
        assert!(v.is_disordered());
        assert!(
            v.header_lines()[0].contains("1 site"),
            "{:?}",
            v.header_lines()
        );
        assert!(
            v.header_lines()[0].contains("disordered"),
            "{:?}",
            v.header_lines()
        );
        let legend = v.legend();
        assert_eq!(legend[0].0, "Cr×0.25", "{legend:?}");
        // Occupancy is in the table whenever it matters.
        assert!(
            v.site_lines(4)[0].trim_end().ends_with("0.25"),
            "{:?}",
            v.site_lines(4)
        );
    }

    /// An inline comment is not part of the number, and `FE` is iron.
    #[test]
    fn inline_comments_and_shouted_symbols_are_read_correctly() {
        let cif = "\
data_x
_cell_length_a 2.87  # refined; ignore _cell_length_a 999.0
_cell_length_b 2.87
_cell_length_c 2.87
loop_
_atom_site_label
_atom_site_type_symbol
_atom_site_fract_x
_atom_site_fract_y
_atom_site_fract_z
FE1 FE 0.0 0.0 0.0
";
        let v = parse_cif(cif).expect("a commented cell length is still a cell length");
        assert_eq!(v.lengths[0], 2.87);
        assert_eq!(v.sites[0].species, "Fe", "FE is iron, not fluorine");
    }

    /// Saying nothing about symmetry is not the same as saying "asymmetric
    /// unit" — the panel used to assert one while admitting the other.
    #[test]
    fn a_cif_that_says_nothing_about_symmetry_says_so() {
        let cif = "\
data_x
_cell_length_a 2.87
_cell_length_b 2.87
_cell_length_c 2.87
loop_
_atom_site_label
_atom_site_type_symbol
_atom_site_fract_x
_atom_site_fract_y
_atom_site_fract_z
Fe1 Fe 0.0 0.0 0.0
";
        let v = parse_cif(cif).expect("parses");
        assert_eq!(v.symmetry, SymmetryState::Unknown);
        assert_eq!(
            v.symmetry_note(),
            Some("symmetry unknown — sites shown exactly as listed")
        );
        assert!(
            !v.header_lines().iter().any(|l| l.contains("asymmetric")),
            "{:?}",
            v.header_lines()
        );
    }

    #[test]
    fn a_cif_without_a_cell_is_refused_by_name() {
        let err = parse_cif("data_x\n_chemical_formula_sum Fe\n").unwrap_err();
        assert!(err.contains("_cell_length_a"), "{err}");
    }

    /// The projection is what makes the drawing honest: straight edges and
    /// atoms where their coordinates put them. Linearity is the whole
    /// property, and the fixed viewpoint keeps c upright and a to the right.
    #[test]
    fn the_projection_is_linear_and_keeps_c_upright() {
        let add = |p: [f64; 3], q: [f64; 3]| [p[0] + q[0], p[1] + q[1], p[2] + q[2]];
        for (p, q) in [
            ([1.0, 0.0, 0.0], [0.0, 2.0, 0.0]),
            ([0.3, -1.2, 4.0], [2.5, 0.7, -0.4]),
        ] {
            let (sx, sy) = project(add(p, q));
            let ((px, py), (qx, qy)) = (project(p), project(q));
            assert!((sx - (px + qx)).abs() < 1e-9 && (sy - (py + qy)).abs() < 1e-9);
        }
        let (cx, cy) = project([0.0, 0.0, 1.0]);
        assert!(
            cx.abs() < 1e-9 && cy > 0.9,
            "c axis must point straight up: ({cx}, {cy})"
        );
        let (ax, _) = project([1.0, 0.0, 0.0]);
        assert!(ax > 0.0, "a axis must point right");
        // The depth axis climbs the screen: that is what makes the top face
        // visible instead of collapsing the cell onto a line.
        let (_, by) = project([0.0, 1.0, 0.0]);
        assert!(by > 0.1, "b axis must recede upward, got y={by}");
        // A cubic cell: the body centre projects to the centroid of the corners.
        let lat = lattice_from_params([1.0, 1.0, 1.0], [90.0, 90.0, 90.0]);
        let corners: Vec<(f64, f64)> = (0u8..8)
            .map(|i| {
                project(cartesian(
                    lat,
                    [
                        f64::from(i & 1),
                        f64::from((i >> 1) & 1),
                        f64::from((i >> 2) & 1),
                    ],
                ))
            })
            .collect();
        let (mx, my) = corners
            .iter()
            .fold((0.0, 0.0), |(x, y), (cx, cy)| (x + cx / 8.0, y + cy / 8.0));
        let (bx, by) = project(cartesian(lat, [0.5, 0.5, 0.5]));
        assert!(
            (bx - mx).abs() < 1e-9 && (by - my).abs() < 1e-9,
            "body centre {bx},{by} vs centroid {mx},{my}"
        );
        // Eight corners, eight distinct points: nothing hides behind anything.
        for i in 0..8 {
            for j in (i + 1)..8 {
                let (a, b) = (corners[i], corners[j]);
                assert!(
                    (a.0 - b.0).abs() > 1e-6 || (a.1 - b.1).abs() > 1e-6,
                    "corners {i} and {j} coincide"
                );
            }
        }
    }

    #[test]
    fn the_lattice_follows_the_cell_parameters() {
        let lat = lattice_from_params([2.0, 3.0, 4.0], [90.0, 90.0, 120.0]);
        assert!((lat[0][0] - 2.0).abs() < 1e-9);
        assert!((lat[1][0] - 3.0 * (120f64).to_radians().cos()).abs() < 1e-9);
        assert!((lat[1][1] - 3.0 * (120f64).to_radians().sin()).abs() < 1e-9);
        assert!((lat[2][2] - 4.0).abs() < 1e-9);
        let p = cartesian(lat, [0.0, 0.0, 0.5]);
        assert!((p[2] - 2.0).abs() < 1e-9);
    }

    /// One Å must be the same distance across as it is down, whatever shape
    /// the pane is — measured at 2.3–2.8× anisotropy before this, which draws
    /// a cubic cell as a slab.
    #[test]
    fn a_cubic_cell_is_drawn_square_in_any_pane() {
        let v = parse_cif(ASE_P1).unwrap();
        for (cols, rows) in [(60u16, 14u16), (30, 20), (100, 8), (40, 6)] {
            let (xb, yb) = v.balanced_bounds(cols, rows);
            let per_dot_x = (xb[1] - xb[0]) / (f64::from(cols) * DOTS_X);
            let per_dot_y = (yb[1] - yb[0]) / (f64::from(rows) * DOTS_Y);
            let ratio = per_dot_x / per_dot_y;
            assert!(
                (ratio - 1.0).abs() < 1e-9,
                "{cols}x{rows}: Å per dot differs by {ratio}× between x and y"
            );
            // Nothing is cut: the data's own bounds still fit inside.
            let (dx, dy) = v.bounds();
            assert!(
                xb[0] <= dx[0] + 1e-9 && xb[1] >= dx[1] - 1e-9,
                "x bounds cut the cell"
            );
            assert!(
                yb[0] <= dy[0] + 1e-9 && yb[1] >= dy[1] - 1e-9,
                "y bounds cut the cell"
            );
        }
    }

    /// A pane too short to hold a cell says so. Squeezed into four rows the
    /// projection deforms; into two it is a smear that reads as the cell.
    #[test]
    fn a_pane_too_short_says_so_instead_of_drawing_a_deformed_cell() {
        let v = parse_cif(ASE_P1).unwrap();
        for rows in 0..MIN_CANVAS_ROWS {
            let art = v.text_render(40, rows);
            assert_eq!(art.len(), 1, "{rows} rows must not draw");
            assert!(
                art[0].starts_with("cell not drawn — needs"),
                "{rows}: {:?}",
                art[0]
            );
            assert!(
                !art[0]
                    .chars()
                    .any(|c| ('\u{2800}'..='\u{28FF}').contains(&c)),
                "{rows}: nothing may be drawn"
            );
        }
        let art = v.text_render(40, MIN_CANVAS_ROWS);
        assert_eq!(art.len(), usize::from(MIN_CANVAS_ROWS));
        assert!(
            art.iter()
                .any(|l| l.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c))),
            "the minimum height must draw: {art:?}"
        );
    }

    #[test]
    fn the_text_render_draws_something_and_the_legend_names_every_species() {
        let v = parse_cif(TIAL).unwrap();
        let art = v.text_render(40, 12);
        assert_eq!(art.len(), 12);
        assert!(
            art.iter()
                .any(|l| l.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c))),
            "no Braille cells drawn: {art:?}"
        );
        let legend = v.legend();
        assert_eq!(legend.len(), 2);
        assert_eq!(legend[0].0, "Ti×1");
        assert_eq!(legend[1].0, "Al×1");
        assert_ne!(species_color("Ti"), species_color("Al"));
        let lines = v.site_lines(1);
        assert_eq!(
            lines.len(),
            2,
            "one shown, the remainder counted: {lines:?}"
        );
        assert!(lines[1].contains("+1 more"));
    }
}
