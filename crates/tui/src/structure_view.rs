// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! A crystal structure as a reader sees it: the cell, the atoms in it, and the
//! numbers that define it — never the CIF text.
//!
//! A `cache://…` reference resolves to a CIF. Showing that CIF is showing a
//! file format, and nobody hovers a formula to read a file format. This module
//! turns the CIF into what the formula stands for — a lattice, a space group,
//! sites with species — and draws the unit cell with its atoms as a Braille
//! projection, so the thing on screen is the structure.
//!
//! Nothing here guesses. A CIF that lists only an asymmetric unit under a
//! space group with symmetry operations is shown with exactly the sites it
//! lists, and the panel SAYS the symmetry is not expanded. A missing space
//! group is "unknown", not "P 1".

use ratatui::style::Color;
use ratatui::widgets::canvas::{Context, Line as CanvasLine, Points};

/// One atomic site as the CIF lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Site {
    pub label: String,
    pub species: String,
    pub frac: [f64; 3],
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
    /// True when the listed sites are the whole cell (P 1, or the only
    /// symmetry operation is the identity). False means the CIF lists an
    /// asymmetric unit and the drawing shows only those sites.
    pub symmetry_expanded: bool,
}

fn strip_uncertainty(token: &str) -> &str {
    token.split('(').next().unwrap_or(token)
}

fn unquote(value: &str) -> String {
    let v = value.trim();
    let v = v
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(v);
    let v = v
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(v);
    v.to_string()
}

fn parse_f64(value: &str) -> Option<f64> {
    strip_uncertainty(value.trim()).parse::<f64>().ok()
}

/// Species from a CIF label such as `Fe12`, `O2-`, `Ti1`: the leading
/// element symbol only.
fn species_from_label(label: &str) -> String {
    let mut out = String::new();
    for (i, ch) in label.chars().enumerate() {
        if ch.is_ascii_alphabetic() && (i == 0 || (i == 1 && ch.is_ascii_lowercase())) {
            out.push(ch);
        } else {
            break;
        }
    }
    if out.is_empty() {
        label.to_string()
    } else {
        out
    }
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

/// Parse a CIF into what the panel shows. Errors name what is missing: a
/// CIF without a cell is not a structure PRISM can draw.
pub fn parse_cif(text: &str) -> Result<StructureView, String> {
    let mut lengths = [None::<f64>; 3];
    let mut angles = [None::<f64>; 3];
    let mut formula = None;
    let mut space_group = None;
    let mut space_group_number = None;
    let mut symops: Vec<String> = Vec::new();
    let mut sites = Vec::new();

    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.starts_with("loop_") {
            // Collect the loop's column tags, then its rows.
            let mut tags: Vec<String> = Vec::new();
            i += 1;
            while i < lines.len() {
                let l = lines[i].trim();
                if l.starts_with('_') {
                    tags.push(l.split_whitespace().next().unwrap_or(l).to_string());
                    i += 1;
                } else {
                    break;
                }
            }
            let mut rows: Vec<Vec<String>> = Vec::new();
            while i < lines.len() {
                let l = lines[i].trim();
                if l.is_empty()
                    || l.starts_with("loop_")
                    || l.starts_with('_')
                    || l.starts_with("data_")
                {
                    break;
                }
                if !l.starts_with('#') {
                    rows.push(split_cif_row(l));
                }
                i += 1;
            }
            let col = |name: &str| tags.iter().position(|t| t.eq_ignore_ascii_case(name));
            if let (Some(fx), Some(fy), Some(fz)) = (
                col("_atom_site_fract_x"),
                col("_atom_site_fract_y"),
                col("_atom_site_fract_z"),
            ) {
                let label_col = col("_atom_site_label");
                let type_col = col("_atom_site_type_symbol");
                for row in rows {
                    let get = |c: usize| row.get(c).map(String::as_str).unwrap_or("");
                    let (Some(x), Some(y), Some(z)) =
                        (parse_f64(get(fx)), parse_f64(get(fy)), parse_f64(get(fz)))
                    else {
                        continue;
                    };
                    let label = label_col.map(get).unwrap_or("").to_string();
                    let species = type_col
                        .map(get)
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
                    });
                }
            } else if let Some(op) = col("_space_group_symop_operation_xyz")
                .or_else(|| col("_symmetry_equiv_pos_as_xyz"))
            {
                for row in rows {
                    if let Some(v) = row.get(op) {
                        symops.push(v.replace(' ', "").to_lowercase());
                    }
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix('_') {
            let mut parts = rest.splitn(2, char::is_whitespace);
            let tag = parts.next().unwrap_or("").to_ascii_lowercase();
            let value = parts.next().unwrap_or("").trim();
            match tag.as_str() {
                "cell_length_a" => lengths[0] = parse_f64(value),
                "cell_length_b" => lengths[1] = parse_f64(value),
                "cell_length_c" => lengths[2] = parse_f64(value),
                "cell_angle_alpha" => angles[0] = parse_f64(value),
                "cell_angle_beta" => angles[1] = parse_f64(value),
                "cell_angle_gamma" => angles[2] = parse_f64(value),
                "chemical_formula_sum" => formula = Some(unquote(value)),
                "space_group_name_h-m_alt"
                | "symmetry_space_group_name_h-m"
                | "space_group_name_h-m" => {
                    space_group = Some(unquote(value));
                }
                "space_group_it_number" | "symmetry_int_tables_number" => {
                    space_group_number = value.trim().parse().ok();
                }
                _ => {}
            }
        }
        i += 1;
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
    let is_p1 = space_group
        .as_deref()
        .map(|s| s.replace(' ', "").eq_ignore_ascii_case("P1"))
        == Some(true)
        || space_group_number == Some(1);
    let only_identity = !symops.is_empty() && symops.iter().all(|s| s == "x,y,z");
    Ok(StructureView {
        formula,
        space_group,
        space_group_number,
        lengths,
        angles,
        lattice: lattice_from_params(lengths, angles),
        sites,
        symmetry_expanded: is_p1 || only_identity,
    })
}

/// Split a CIF data row on whitespace, keeping quoted fields whole.
fn split_cif_row(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in line.chars() {
        match quote {
            Some(q) if ch == q => {
                quote = None;
            }
            Some(_) => cur.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl StructureView {
    /// Species in first-seen order, with how many sites each has.
    pub fn species_counts(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        for s in &self.sites {
            if let Some(e) = out.iter_mut().find(|(sym, _)| *sym == s.species) {
                e.1 += 1;
            } else {
                out.push((s.species.clone(), 1));
            }
        }
        out
    }

    /// The numbers a reader wants first: formula, sites, space group, cell.
    pub fn header_lines(&self) -> Vec<String> {
        let sg = match (&self.space_group, self.space_group_number) {
            (Some(name), Some(n)) => format!("{name} (No. {n})"),
            (Some(name), None) => name.clone(),
            (None, Some(n)) => format!("No. {n}"),
            (None, None) => "unknown".to_string(),
        };
        let sites = if self.symmetry_expanded {
            format!("{} sites", self.sites.len())
        } else {
            format!(
                "{} sites listed (asymmetric unit; symmetry not expanded)",
                self.sites.len()
            )
        };
        vec![
            format!(
                "{}  ·  {sites}",
                self.formula.as_deref().unwrap_or("formula unknown")
            ),
            format!("space group  {sg}"),
            format!(
                "a b c  {:.3}  {:.3}  {:.3} Å",
                self.lengths[0], self.lengths[1], self.lengths[2]
            ),
            format!(
                "α β γ  {:.2}°  {:.2}°  {:.2}°",
                self.angles[0], self.angles[1], self.angles[2]
            ),
        ]
    }

    /// Legend: one entry per species, in the colour the drawing uses.
    pub fn legend(&self) -> Vec<(String, Color)> {
        self.species_counts()
            .into_iter()
            .map(|(sym, n)| (format!("{sym}×{n}"), species_color(&sym)))
            .collect()
    }

    /// The site table, bounded; the remainder is COUNTED, never silently cut.
    pub fn site_lines(&self, limit: usize) -> Vec<String> {
        let mut out = Vec::new();
        for s in self.sites.iter().take(limit) {
            out.push(format!(
                "{:<6} {:<3} {:>7.4} {:>7.4} {:>7.4}",
                s.label, s.species, s.frac[0], s.frac[1], s.frac[2]
            ));
        }
        if self.sites.len() > limit {
            out.push(format!("  +{} more sites", self.sites.len() - limit));
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

    /// Draw the cell edges and the atoms into a canvas context.
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
        for (sym, _) in self.species_counts() {
            let coords: Vec<(f64, f64)> = self
                .sites
                .iter()
                .filter(|s| s.species == sym)
                .map(|s| project(cartesian(self.lattice, s.frac)))
                .collect();
            ctx.draw(&Points {
                coords: &coords,
                color: species_color(&sym),
            });
        }
    }

    /// The drawing as text lines, for surfaces that show text rather than
    /// widgets (the Enter-key detail view). Colour is lost; the shape is not.
    pub fn text_render(&self, width: u16, height: u16) -> Vec<String> {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::symbols::Marker;
        use ratatui::widgets::Widget;
        use ratatui::widgets::canvas::Canvas;
        let area = Rect::new(0, 0, width.max(1), height.max(1));
        let mut buf = Buffer::empty(area);
        let (xb, yb) = self.bounds();
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
        assert!(
            !v.symmetry_expanded,
            "P 4/m m m with two listed sites is an asymmetric unit"
        );
        let header = v.header_lines().join("\n");
        assert!(header.contains("symmetry not expanded"), "{header}");
    }

    #[test]
    fn the_cache_s_own_ase_cifs_parse_as_full_cells() {
        let v = parse_cif(ASE_P1).expect("parses");
        assert_eq!(v.formula.as_deref(), Some("Fe2"));
        assert_eq!(v.space_group_number, Some(1));
        assert!(
            v.symmetry_expanded,
            "P 1 with an identity-only symop loop is the whole cell"
        );
        assert_eq!(v.sites.len(), 2);
        assert_eq!(v.sites[1].label, "Fe2");
        assert_eq!(v.sites[1].species, "Fe");
        assert!(
            v.header_lines()[0].starts_with("Fe2  ·  2 sites"),
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
