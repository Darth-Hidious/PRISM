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

/// The quantity kind of a declared extraction unit. `None` for anything
/// outside [`EXTRACTION_UNITS`] — including bare spellings like `"MPa"`,
/// which are a vocabulary problem (unit normalisation's job), not a
/// quantity-kind contradiction.
#[must_use]
pub fn unit_quantity_kind(unit: &str) -> Option<QuantityKind> {
    EXTRACTION_UNITS
        .iter()
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
    if name.contains("density") {
        return Some(QuantityKind::Density);
    }
    if name.contains("strength") || name.contains("stress") || name.contains("modulus") {
        return Some(QuantityKind::Pressure);
    }
    if name.contains("temperature") || name.contains("melting point") {
        return Some(QuantityKind::Temperature);
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
        // No duplicate identifiers.
        let mut seen = std::collections::HashSet::new();
        for (id, _) in EXTRACTION_UNITS {
            assert!(seen.insert(*id), "unit {id} declared twice");
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
        // No claim where none is defensible: hardness is deliberately
        // unmapped, and electrical conductivity must not ride on "thermal".
        assert_eq!(property_quantity_kind("Hardness_HV"), None);
        assert_eq!(property_quantity_kind("electrical conductivity"), None);
        assert_eq!(property_quantity_kind("corrosion resistance"), None);
    }
}
