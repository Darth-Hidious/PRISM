//! The QUDT unit vocabulary tabular extraction may emit, and the quantity
//! kind each unit measures.
//!
//! ONE declaration serves two consumers, so they cannot drift apart:
//!
//! - [`crate::extraction_schema`] builds the enum-locked `unit` field of the
//!   extraction JSON schema from [`EXTRACTION_UNITS`], making the model
//!   structurally incapable of emitting a unit outside this list (measured
//!   2026-08-08: both a 3B and a 12B model emit `"MPa"` / `"g/cm3"` when
//!   unconstrained, which the strict `prism_provenance::QudtUnit`
//!   deserialiser then rejects).
//! - [`crate::graph_validation`] checks each unit's [`QuantityKind`] against
//!   the quantity the property NAME states — the half constrained decoding
//!   cannot do. Constraining guarantees FORM, never TRUTH: the same 12B
//!   model, enum-locked, tagged a density of 8.19 with `QUDT:GigaPA` — a
//!   pressure unit on a density, legal to the grammar and false.
//!
//! The list is deliberately SMALL and justified: every identifier is a real
//! QUDT unit local name (`http://qudt.org/vocab/unit/<name>`) already used
//! by this codebase's text-extraction prompt/tests or needed by the property
//! columns PRISM's tabular fixtures actually carry (strengths in MPa,
//! densities in g/cm³ and kg/m³, temperatures, thermal conductivity,
//! percent fractions). It is NOT an attempt at a QUDT ontology — quantities
//! we cannot justify from data in this repo (e.g. Vickers hardness, whose
//! QUDT modelling is not a clean pressure) are left out on purpose.

/// The physical quantity a unit measures, for the handful of quantities
/// PRISM's ingest actually sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantityKind {
    /// Pressure and mechanical stress (Pa family) — yield/tensile strength,
    /// elastic moduli.
    Pressure,
    Density,
    Temperature,
    ThermalConductivity,
    /// Dimensionless fraction (percent).
    Fraction,
    /// Linear speed — LPBF scan speeds (mm/s, m/s).
    Speed,
    /// Elapsed time / duration (s, min, h).
    Time,
    /// A COUNT of discrete events or objects — fatigue cycles to failure,
    /// numbers of pores. Dimensionless, but NOT [`Self::Fraction`]: 50,000
    /// cycles is not 50,000 percent, and conflating them would let a
    /// percentage stand as evidence for a cycle count.
    Count,
    /// Counts per unit VOLUME — pore number density (mm⁻³).
    ///
    /// Deliberately distinct from [`Self::Density`], which is MASS per
    /// volume. Both properties are called "density" in papers, and a
    /// substring match on that word alone would let a printed `8.19 g/cm³`
    /// satisfy the kind guard for a pore-count property — storing a mass
    /// density as a number density, with a genuine verbatim span attached.
    NumberDensity,
    /// Events per unit time — fatigue test frequency in cycles/min.
    Frequency,
}

impl QuantityKind {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pressure => "pressure/stress",
            Self::Density => "density",
            Self::Temperature => "temperature",
            Self::ThermalConductivity => "thermal conductivity",
            Self::Fraction => "fraction",
            Self::Speed => "speed",
            Self::Time => "time",
            Self::Count => "count",
            Self::NumberDensity => "number density",
            Self::Frequency => "frequency",
        }
    }
}

/// The unit vocabulary, each with its quantity kind. Declaration order is
/// the order the extraction schema's enum presents.
pub const EXTRACTION_UNITS: &[(&str, QuantityKind)] = &[
    ("QUDT:PA", QuantityKind::Pressure),
    ("QUDT:KiloPA", QuantityKind::Pressure),
    ("QUDT:MegaPA", QuantityKind::Pressure),
    ("QUDT:GigaPA", QuantityKind::Pressure),
    ("QUDT:KiloGM-PER-M3", QuantityKind::Density),
    ("QUDT:GM-PER-CentiM3", QuantityKind::Density),
    ("QUDT:K", QuantityKind::Temperature),
    ("QUDT:DEG_C", QuantityKind::Temperature),
    ("QUDT:W-PER-M-K", QuantityKind::ThermalConductivity),
    ("QUDT:PERCENT", QuantityKind::Fraction),
];

/// Quantity kinds for canonical units OUTSIDE the tabular-extraction
/// vocabulary. These identifiers are ones `prism_provenance::units` already
/// resolves from document spellings (mm/s and m/s carry LPBF scan speeds;
/// s/min/h are hold and exposure times) — the repair tier's strict
/// quantity-kind guard needs their kinds to tell a printed scan speed from
/// a printed dwell time sitting next to the same number.
///
/// Deliberately a SEPARATE table: [`EXTRACTION_UNITS`] also generates the
/// tabular extraction schema's `unit` enum, and these units must not widen
/// what that schema lets a model emit. They only inform kind lookups.
pub const NON_SCHEMA_UNIT_KINDS: &[(&str, QuantityKind)] = &[
    ("QUDT:MilliM-PER-SEC", QuantityKind::Speed),
    ("QUDT:M-PER-SEC", QuantityKind::Speed),
    ("QUDT:SEC", QuantityKind::Time),
    ("QUDT:MIN", QuantityKind::Time),
    ("QUDT:HR", QuantityKind::Time),
    // Counts, count densities and event rates. Measured on a real 36-page
    // LPBF fatigue paper through the live pipeline (Gemma 4 12B): of 47
    // extracted facts, TEN were refused for these three quantities alone —
    // six pore number densities, three LCF cycle counts, one test frequency.
    // The repair tier could not rescue any of them, because a unit whose
    // kind is unknown can never satisfy its strict kind-equality guard, so
    // every one queued for a model tier that does not exist yet.
    //
    // Each identifier verified against the live QUDT vocabulary
    // (`http://qudt.org/vocab/unit/<name>` → HTTP 200) rather than recalled;
    // `CYC-PER-MIN`, the obvious spelling for a cycle rate, is NOT a QUDT
    // unit (404) — cycles are dimensionless, so a cycle rate is PER-MIN.
    ("QUDT:NUM", QuantityKind::Count),
    ("QUDT:NUM-PER-MilliM3", QuantityKind::NumberDensity),
    ("QUDT:PER-MIN", QuantityKind::Frequency),
];

/// The quantity kind of a declared unit — [`EXTRACTION_UNITS`] plus
/// [`NON_SCHEMA_UNIT_KINDS`]. `None` for anything else — including bare
/// spellings like `"MPa"`, which are a vocabulary problem (unit
/// normalisation's job), not a quantity-kind contradiction.
#[must_use]
pub fn unit_quantity_kind(unit: &str) -> Option<QuantityKind> {
    EXTRACTION_UNITS
        .iter()
        .chain(NON_SCHEMA_UNIT_KINDS)
        .find(|(id, _)| *id == unit)
        .map(|(_, kind)| *kind)
}

/// The quantity kind a property NAME states, for names this module can
/// defend. Substring match on the lowercased name — extraction emits
/// "Density", "density_g_cm3", "Yield Strength", "yield_strength_mpa" for
/// the same columns depending on the model and run. `None` means "this
/// module makes no claim about that property", never "no unit is allowed".
#[must_use]
pub fn property_quantity_kind(property_name: &str) -> Option<QuantityKind> {
    let name = property_name.to_lowercase();
    // Most-specific first: "thermal conductivity" must not fall through to
    // a broader match, and nothing here may claim bare "conductivity"
    // (electrical conductivity is a different quantity).
    if name.contains("thermal conductivity") {
        return Some(QuantityKind::ThermalConductivity);
    }
    // BEFORE the bare "density" arm, and load-bearing. A pore NUMBER density
    // is a count per volume (mm⁻³); a mass density is g/cm³. Both are spelt
    // "density" in papers. Falling through to `Density` here would let a
    // printed mass density satisfy the repair tier's kind-equality guard for
    // a pore-count property and store it as evidenced — a false fact wearing
    // a genuine verbatim quote, which is worse than a dropped one.
    // `a_pore_number_density_is_not_a_mass_density` fails if this arm is
    // moved below the next one.
    if name.contains("number density") || name.contains("pore density") {
        return Some(QuantityKind::NumberDensity);
    }
    if name.contains("density") {
        return Some(QuantityKind::Density);
    }
    // "cycles to failure", "LCF cycles", "fatigue life (cycles)".
    if name.contains("cycles") || name.contains("cycle count") {
        return Some(QuantityKind::Count);
    }
    if name.contains("frequency") {
        return Some(QuantityKind::Frequency);
    }
    if name.contains("strength") || name.contains("stress") || name.contains("modulus") {
        return Some(QuantityKind::Pressure);
    }
    if name.contains("temperature") || name.contains("melting point") {
        return Some(QuantityKind::Temperature);
    }
    if name.contains("speed") || name.contains("velocity") {
        return Some(QuantityKind::Speed);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every declared unit resolves to its own kind — the table IS the
    /// lookup, no second list to drift.
    #[test]
    fn every_declared_unit_resolves_to_its_declared_kind() {
        for (id, kind) in EXTRACTION_UNITS {
            assert_eq!(unit_quantity_kind(id), Some(*kind), "{id}");
            assert!(
                id.starts_with("QUDT:") && id.len() > "QUDT:".len(),
                "{id} is not a QUDT-prefixed identifier"
            );
        }
        // No duplicate identifiers — across BOTH tables: a unit declared in
        // each with different kinds would make the lookup order-dependent.
        let mut seen = std::collections::HashSet::new();
        for (id, _) in EXTRACTION_UNITS.iter().chain(NON_SCHEMA_UNIT_KINDS) {
            assert!(seen.insert(*id), "unit {id} declared twice");
        }
    }

    /// A pore NUMBER density (count per volume) must never be classified as
    /// a MASS density. Both are spelt "density"; only the ordering of the
    /// arms in `property_quantity_kind` separates them.
    ///
    /// Without the `number density` arm sitting ABOVE the bare `density`
    /// arm, these properties resolve to `Density`, whose units are g/cm³ and
    /// kg/m³ — so a mass density printed anywhere near the value would
    /// satisfy the repair tier's kind-equality guard and be stored as a pore
    /// count with a real verbatim span attached. Move that arm down and this
    /// test fails.
    #[test]
    fn a_pore_number_density_is_not_a_mass_density() {
        for property in [
            "internal pore number density",
            "surface pore number density",
            "Pore Number Density",
            "pore density",
        ] {
            assert_eq!(
                property_quantity_kind(property),
                Some(QuantityKind::NumberDensity),
                "{property} must be a number density, not a mass density"
            );
        }
        // The mass-density path is untouched by the new arm.
        assert_eq!(
            property_quantity_kind("density"),
            Some(QuantityKind::Density)
        );
        assert_eq!(
            property_quantity_kind("density_g_cm3"),
            Some(QuantityKind::Density)
        );
        // And the two kinds are genuinely different, so the guard that
        // compares them cannot be satisfied across the pair.
        assert_ne!(QuantityKind::NumberDensity, QuantityKind::Density);
    }

    /// The three quantities that cost the LPBF paper ten facts now have both
    /// halves the repair tier needs: a property name that states a kind, and
    /// a unit identifier carrying the SAME kind. Either half alone leaves the
    /// tier unable to decide.
    #[test]
    fn the_lpbf_refused_quantities_have_matching_property_and_unit_kinds() {
        for (property, unit) in [
            ("internal pore number density", "QUDT:NUM-PER-MilliM3"),
            ("LCF cycles @ 758 MPa", "QUDT:NUM"),
            ("fatigue test frequency", "QUDT:PER-MIN"),
        ] {
            let from_property = property_quantity_kind(property);
            let from_unit = unit_quantity_kind(unit);
            assert!(
                from_property.is_some() && from_property == from_unit,
                "{property} states {from_property:?} but {unit} carries \
                 {from_unit:?} — the kind guard cannot establish equality"
            );
        }
    }

    /// A count is not a fraction. 50,000 cycles is not 50,000 percent, and
    /// keeping them distinct stops a printed percentage standing as evidence
    /// for a cycle count.
    #[test]
    fn a_count_is_not_a_fraction() {
        assert_ne!(QuantityKind::Count, QuantityKind::Fraction);
        assert_eq!(unit_quantity_kind("QUDT:NUM"), Some(QuantityKind::Count));
        assert_eq!(
            unit_quantity_kind("QUDT:PERCENT"),
            Some(QuantityKind::Fraction)
        );
    }

    /// The non-schema table informs kind lookups without touching the
    /// extraction schema: every identifier resolves to its declared kind,
    /// is a canonical identifier the unit vocabulary itself vouches for,
    /// and is NOT in [`EXTRACTION_UNITS`] (that would widen the schema).
    #[test]
    fn non_schema_units_resolve_kinds_without_entering_the_schema() {
        for (id, kind) in NON_SCHEMA_UNIT_KINDS {
            assert_eq!(unit_quantity_kind(id), Some(*kind), "{id}");
            assert_eq!(
                prism_provenance::units::resolve_unit(id).map(|u| u.as_str().to_string()),
                Some((*id).to_string()),
                "{id} must be a canonical identifier of the controlled vocabulary"
            );
            assert!(
                !EXTRACTION_UNITS
                    .iter()
                    .any(|(schema_id, _)| schema_id == id),
                "{id} must not leak into the extraction schema enum"
            );
        }
    }

    #[test]
    fn undeclared_units_get_no_kind_claim() {
        // Bare spellings and unknowns are a vocabulary problem, not a
        // quantity-kind contradiction — no claim, no false positive.
        for unit in ["MPa", "g/cm3", "QUDT:UNHEARD-OF", ""] {
            assert_eq!(unit_quantity_kind(unit), None, "{unit:?}");
        }
    }

    #[test]
    fn property_names_map_to_defensible_kinds_only() {
        assert_eq!(
            property_quantity_kind("Density"),
            Some(QuantityKind::Density)
        );
        assert_eq!(
            property_quantity_kind("density_g_cm3"),
            Some(QuantityKind::Density)
        );
        assert_eq!(
            property_quantity_kind("Yield Strength"),
            Some(QuantityKind::Pressure)
        );
        assert_eq!(
            property_quantity_kind("yield_strength_mpa"),
            Some(QuantityKind::Pressure)
        );
        assert_eq!(
            property_quantity_kind("Young's modulus"),
            Some(QuantityKind::Pressure)
        );
        assert_eq!(
            property_quantity_kind("Thermal Conductivity"),
            Some(QuantityKind::ThermalConductivity)
        );
        assert_eq!(
            property_quantity_kind("melting point"),
            Some(QuantityKind::Temperature)
        );
        assert_eq!(
            property_quantity_kind("scan speed"),
            Some(QuantityKind::Speed)
        );
        assert_eq!(
            property_quantity_kind("scanning velocity"),
            Some(QuantityKind::Speed)
        );
        // No claim where none is defensible: hardness is deliberately
        // unmapped, and electrical conductivity must not ride on "thermal".
        assert_eq!(property_quantity_kind("Hardness_HV"), None);
        assert_eq!(property_quantity_kind("electrical conductivity"), None);
        assert_eq!(property_quantity_kind("corrosion resistance"), None);
    }
}
