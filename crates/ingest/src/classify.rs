//! Ontology classification metadata carried to provenance writes.
//!
//! Paper population now selects classes through the bounded tool loop. The
//! former second, single-shot classifier had no production caller and could
//! overwrite those choices, so only the storage-facing value type remains.

/// An entity class resolved against the active ontology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityClass {
    /// Extraction label retained separately from the storage label.
    pub entity_type: String,
    /// Label used as part of the persisted entity key.
    pub storage_label: String,
    /// Canonical class IRI from the active ontology.
    pub class_iri: String,
}
