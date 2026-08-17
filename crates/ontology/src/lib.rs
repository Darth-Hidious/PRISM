/*
 * @file    lib.rs
 * @brief   Validated, auditable loading of PRISM's materialised ontology graph.
 *
 * @project PRISM / Ontology
 * @req     REQ-OWL-1.1, REQ-OWL-1.2, REQ-OWL-1.5
 *
 * @author  Mirdyne
 * @date    2026-08-09
 *
 * @copyright (c) 2026 Mirdyne. All rights reserved.
 * @classification ESA-funded deliverable; project source-available.
 *
 * @version 1.0.0
 *
 * @history
 *   2026-08-09 - 1.0.0 - Mirdyne - Initial OWL/RDFS ontology layer.
 *
 * @note    The runtime performs RDFS named-class closure only. No OWL 2 DL
 *          entailment is claimed by this crate.
 * @warning Artifact bytes are rejected unless their SHA-256 matches the
 *          provenance manifest.
 */

//! PRISM's RDF ontology boundary.
//!
//! This crate deliberately owns RDF parsing and IRI validation so callers do
//! not need to depend on Sophia. It loads a small, materialised Turtle artifact,
//! verifies its supply-chain hash, and precomputes cycle-safe named
//! `rdfs:subClassOf` closure in both directions.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use sophia::api::prelude::{Term, TripleSource};
use sophia::api::term::SimpleTerm;
use sophia::turtle::parser::turtle;
use thiserror::Error;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const RDFS_SUBPROPERTY_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
const RDFS_DOMAIN: &str = "http://www.w3.org/2000/01/rdf-schema#domain";
const RDFS_RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";
const SKOS_PREF_LABEL: &str = "http://www.w3.org/2004/02/skos/core#prefLabel";
const OWL_CLASS: &str = "http://www.w3.org/2002/07/owl#Class";
const OWL_OBJECT_PROPERTY: &str = "http://www.w3.org/2002/07/owl#ObjectProperty";
const OWL_ONTOLOGY: &str = "http://www.w3.org/2002/07/owl#Ontology";
const OWL_VERSION_IRI: &str = "http://www.w3.org/2002/07/owl#versionIRI";

/// An owned IRI validated by Sophia against the absolute-IRI grammar.
///
/// # Requirement
///
/// REQ-OWL-1.2: canonical ontology identities shall be validated IRIs rather
/// than bare English strings.
pub type Iri = sophia::iri::Iri<String>;

/// A named OWL class declaration retained by the materialised graph.
///
/// `pref_label` is upstream vocabulary metadata. `extraction_labels` are
/// explicitly curated PRISM aliases and must not be interpreted as upstream
/// labels (for example, `Alloy` maps to the broader `MetallicMaterial`).
///
/// # Requirement
///
/// REQ-OWL-1.2: expose canonical class IRIs, human labels, and named parents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassDecl {
    /// Canonical vocabulary identity.
    pub iri: Iri,
    /// Upstream `skos:prefLabel`, if the source supplies one.
    pub pref_label: Option<String>,
    /// Direct named `rdfs:subClassOf` parents.
    pub parents: Vec<Iri>,
    /// Curated labels accepted from PRISM extraction.
    pub extraction_labels: Vec<String>,
}

/// A named OWL object-property declaration retained by the materialised graph.
///
/// # Requirement
///
/// REQ-OWL-1.2: expose canonical object-property IRIs independently from the
/// extraction vocabulary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PropDecl {
    /// Canonical vocabulary identity.
    pub iri: Iri,
    /// Upstream `skos:prefLabel`, if the source supplies one.
    pub pref_label: Option<String>,
    /// Direct named `rdfs:subPropertyOf` parents.
    pub parents: Vec<Iri>,
    /// Named `rdfs:domain` declarations retained verbatim from the artifact.
    pub domains: Vec<Iri>,
    /// Named `rdfs:range` declarations retained verbatim from the artifact.
    pub ranges: Vec<Iri>,
    /// Curated relation labels accepted from PRISM extraction.
    pub extraction_labels: Vec<String>,
}

/// A validated materialised ontology and its precomputed class closure.
///
/// # Requirement
///
/// REQ-OWL-1.2: provide multi-namespace declarations and cycle-safe
/// subsumption queries without exposing RDF implementation details to ingest.
#[derive(Clone, Debug)]
pub struct OntologyGraph {
    version_iri: Iri,
    sha256: String,
    prefixes: BTreeMap<String, Iri>,
    classes: Vec<ClassDecl>,
    properties: Vec<PropDecl>,
    class_index: HashMap<Iri, usize>,
    property_index: HashMap<Iri, usize>,
    class_alias_index: HashMap<String, usize>,
    property_alias_index: HashMap<String, usize>,
    ancestors: HashMap<Iri, BTreeSet<Iri>>,
    descendants: HashMap<Iri, BTreeSet<Iri>>,
}

/// Failures that prevent an ontology from being trusted or queried.
///
/// # Requirement
///
/// REQ-OWL-1.1 and REQ-OWL-1.2: malformed, incomplete, or unauthenticated
/// ontology inputs shall fail explicitly.
#[derive(Debug, Error)]
pub enum OntologyError {
    /// A required artifact or manifest could not be read.
    #[error("failed to read ontology input {path}: {source}")]
    Read {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// The provenance manifest is invalid JSON.
    #[error("invalid ontology provenance manifest: {0}")]
    Manifest(#[from] serde_json::Error),
    /// Artifact bytes are not UTF-8 Turtle text.
    #[error("ontology artifact is not UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    /// Sophia rejected the Turtle input.
    #[error("failed to parse Turtle ontology artifact: {message}")]
    Turtle {
        /// Sophia parser diagnostic.
        message: String,
    },
    /// The artifact's digest differs from its signed-off manifest value.
    #[error("ontology artifact SHA-256 mismatch: expected {expected}, got {actual}")]
    HashMismatch {
        /// Digest recorded in the provenance manifest.
        expected: String,
        /// Digest computed from the supplied artifact bytes.
        actual: String,
    },
    /// A string expected to be an absolute IRI is invalid.
    #[error("invalid IRI for {context}: {value}")]
    InvalidIri {
        /// Location or role of the invalid value.
        context: String,
        /// Invalid string value.
        value: String,
    },
    /// Required ontology metadata is absent from the artifact.
    #[error("ontology artifact is missing required statement: {0}")]
    MissingStatement(String),
    /// Manifest and artifact ontology versions disagree.
    #[error("ontology artifact does not declare manifest version IRI {0}")]
    VersionMismatch(String),
    /// A manifest class alias targets no declared class.
    #[error("class mapping {label:?} targets undeclared class {iri}")]
    MissingMappedClass {
        /// PRISM extraction alias.
        label: String,
        /// Missing canonical IRI.
        iri: String,
    },
    /// A manifest relation alias targets no declared object property.
    #[error("relation mapping {label:?} targets undeclared object property {iri}")]
    MissingMappedProperty {
        /// PRISM extraction alias.
        label: String,
        /// Missing canonical IRI.
        iri: String,
    },
    /// A retained class points to a parent omitted from the artifact.
    #[error("class {class} has parent {parent}, which is not declared in the artifact")]
    MissingParent {
        /// Child class IRI.
        class: String,
        /// Missing parent class IRI.
        parent: String,
    },
    /// A retained object property points to a parent omitted from the artifact.
    #[error(
        "object property {property} has parent {parent}, which is not declared in the artifact"
    )]
    MissingPropertyParent {
        /// Child object-property IRI.
        property: String,
        /// Missing parent object-property IRI.
        parent: String,
    },
    /// A retained declaration has no human-facing upstream preferred label.
    #[error("{kind} {iri} has no skos:prefLabel in the ontology artifact")]
    MissingPreferredLabel {
        /// Declaration kind (`class` or `object property`).
        kind: &'static str,
        /// Unlabelled canonical IRI.
        iri: String,
    },
    /// More than one equally preferred human label is present.
    #[error("{kind} {iri} has ambiguous skos:prefLabel values: {labels:?}")]
    AmbiguousPreferredLabel {
        /// Declaration kind (`class` or `object property`).
        kind: &'static str,
        /// Ambiguous canonical IRI.
        iri: String,
        /// Equally ranked lexical forms.
        labels: Vec<String>,
    },
    /// Two canonical declarations claim the same extraction alias.
    #[error("duplicate {kind} extraction alias {label:?}")]
    DuplicateAlias {
        /// Declaration kind (`class` or `relation`).
        kind: &'static str,
        /// Colliding extraction alias.
        label: String,
    },
}

#[derive(Debug, Deserialize)]
struct ProvenanceManifest {
    ontology_iri: String,
    version_iri: String,
    materialised_sha256: String,
    prefixes: BTreeMap<String, String>,
    #[serde(default)]
    class_mappings: Vec<ExtractionMapping>,
    #[serde(default)]
    relation_mappings: Vec<ExtractionMapping>,
}

#[derive(Debug, Deserialize)]
struct ExtractionMapping {
    extraction_label: String,
    iri: String,
}

impl OntologyGraph {
    /// Load and authenticate an ontology artifact and provenance manifest.
    ///
    /// # Requirement
    ///
    /// REQ-OWL-1.1: every load shall verify the exact artifact SHA-256 before
    /// parsing declarations.
    ///
    /// # Errors
    ///
    /// Returns [`OntologyError`] for filesystem, manifest, hash, RDF, IRI, or
    /// graph-integrity failures. No partially loaded graph is returned.
    pub fn load_from_files(
        artifact_path: impl AsRef<Path>,
        manifest_path: impl AsRef<Path>,
    ) -> Result<Self, OntologyError> {
        let artifact_path = artifact_path.as_ref();
        let manifest_path = manifest_path.as_ref();
        let artifact = std::fs::read(artifact_path).map_err(|source| OntologyError::Read {
            path: artifact_path.to_path_buf(),
            source,
        })?;
        let manifest = std::fs::read(manifest_path).map_err(|source| OntologyError::Read {
            path: manifest_path.to_path_buf(),
            source,
        })?;
        Self::from_bytes(&artifact, &manifest)
    }

    /// Load and authenticate an ontology from in-memory artifact bytes.
    ///
    /// This is the production dispatch used by [`load_bundled_emmo`] and is
    /// also useful to hosts that embed their own verified artifact.
    ///
    /// # Requirement
    ///
    /// REQ-OWL-1.1 and REQ-OWL-1.2: authenticate before parsing, validate every
    /// canonical IRI, then compute cycle-safe closure.
    ///
    /// # Errors
    ///
    /// Returns [`OntologyError`] if authentication, parsing, validation, or
    /// closure preconditions fail.
    pub fn from_bytes(artifact: &[u8], manifest: &[u8]) -> Result<Self, OntologyError> {
        let manifest: ProvenanceManifest = serde_json::from_slice(manifest)?;
        let actual_sha256 = sha256_hex(artifact);
        if actual_sha256 != manifest.materialised_sha256 {
            return Err(OntologyError::HashMismatch {
                expected: manifest.materialised_sha256,
                actual: actual_sha256,
            });
        }

        let ontology_iri = validated_iri(&manifest.ontology_iri, "manifest ontology_iri")?;
        let version_iri = validated_iri(&manifest.version_iri, "manifest version_iri")?;
        let text = std::str::from_utf8(artifact)?;
        let triples: Vec<[SimpleTerm<'static>; 3]> = turtle::parse_str(text)
            .collect_triples()
            .map_err(|error| OntologyError::Turtle {
                message: error.to_string(),
            })?;

        let mut class_iris = BTreeSet::new();
        let mut property_iris = BTreeSet::new();
        let mut labels: BTreeMap<String, Vec<(u8, String)>> = BTreeMap::new();
        let mut direct_parents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut property_parents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut property_domains: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut property_ranges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut declares_ontology = false;
        let mut declares_version = false;

        for triple in &triples {
            let subject = term_iri(&triple[0]);
            let predicate = term_iri(&triple[1]);
            let object = term_iri(&triple[2]);

            if predicate.as_deref() == Some(RDF_TYPE) && object.as_deref() == Some(OWL_CLASS) {
                if let Some(subject) = subject.as_ref() {
                    class_iris.insert(subject.clone());
                }
            } else if predicate.as_deref() == Some(RDF_TYPE)
                && object.as_deref() == Some(OWL_OBJECT_PROPERTY)
                && let Some(subject) = subject.as_ref()
            {
                property_iris.insert(subject.clone());
            }

            if subject.as_deref() == Some(ontology_iri.as_str())
                && predicate.as_deref() == Some(RDF_TYPE)
                && object.as_deref() == Some(OWL_ONTOLOGY)
            {
                declares_ontology = true;
            }
            if subject.as_deref() == Some(ontology_iri.as_str())
                && predicate.as_deref() == Some(OWL_VERSION_IRI)
                && object.as_deref() == Some(version_iri.as_str())
            {
                declares_version = true;
            }

            if predicate.as_deref() == Some(SKOS_PREF_LABEL)
                && let (Some(subject), Some(value)) = (subject.as_ref(), term_literal(&triple[2]))
            {
                let rank = match triple[2].language_tag() {
                    Some(language) if language.as_str().eq_ignore_ascii_case("en") => 0,
                    None => 1,
                    Some(_) => 2,
                };
                labels
                    .entry(subject.clone())
                    .or_default()
                    .push((rank, value));
            }

            if predicate.as_deref() == Some(RDFS_SUBCLASS_OF)
                && let (Some(subject), Some(object)) = (subject.as_ref(), object.as_ref())
            {
                direct_parents
                    .entry(subject.clone())
                    .or_default()
                    .insert(object.clone());
            }

            if predicate.as_deref() == Some(RDFS_SUBPROPERTY_OF)
                && let (Some(subject), Some(object)) = (subject.as_ref(), object.as_ref())
            {
                property_parents
                    .entry(subject.clone())
                    .or_default()
                    .insert(object.clone());
            }
            if predicate.as_deref() == Some(RDFS_DOMAIN)
                && let (Some(subject), Some(object)) = (subject.as_ref(), object.as_ref())
            {
                property_domains
                    .entry(subject.clone())
                    .or_default()
                    .insert(object.clone());
            }
            if predicate.as_deref() == Some(RDFS_RANGE)
                && let (Some(subject), Some(object)) = (subject, object)
            {
                property_ranges.entry(subject).or_default().insert(object);
            }
        }

        if !declares_ontology {
            return Err(OntologyError::MissingStatement(format!(
                "{} rdf:type owl:Ontology",
                ontology_iri.as_str()
            )));
        }
        if !declares_version {
            return Err(OntologyError::VersionMismatch(
                version_iri.as_str().to_owned(),
            ));
        }

        for values in labels.values_mut() {
            values.sort();
            values.dedup();
        }

        let mut class_aliases: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut seen_class_aliases = BTreeSet::new();
        for mapping in manifest.class_mappings {
            let iri = validated_iri(&mapping.iri, "class mapping")?;
            if !class_iris.contains(iri.as_str()) {
                return Err(OntologyError::MissingMappedClass {
                    label: mapping.extraction_label,
                    iri: mapping.iri,
                });
            }
            if !seen_class_aliases.insert(mapping.extraction_label.clone()) {
                return Err(OntologyError::DuplicateAlias {
                    kind: "class",
                    label: mapping.extraction_label,
                });
            }
            class_aliases
                .entry(iri.as_str().to_owned())
                .or_default()
                .push(mapping.extraction_label);
        }

        let mut property_aliases: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut seen_property_aliases = BTreeSet::new();
        for mapping in manifest.relation_mappings {
            let iri = validated_iri(&mapping.iri, "relation mapping")?;
            if !property_iris.contains(iri.as_str()) {
                return Err(OntologyError::MissingMappedProperty {
                    label: mapping.extraction_label,
                    iri: mapping.iri,
                });
            }
            if !seen_property_aliases.insert(mapping.extraction_label.clone()) {
                return Err(OntologyError::DuplicateAlias {
                    kind: "relation",
                    label: mapping.extraction_label,
                });
            }
            property_aliases
                .entry(iri.as_str().to_owned())
                .or_default()
                .push(mapping.extraction_label);
        }

        let mut classes = Vec::with_capacity(class_iris.len());
        for iri_text in class_iris {
            let mut parents = Vec::new();
            for parent_text in direct_parents.remove(&iri_text).unwrap_or_default() {
                let parent = validated_iri(&parent_text, "rdfs:subClassOf parent")?;
                parents.push(parent);
            }
            parents.sort();
            let pref_label = preferred_label(&labels, &iri_text, "class")?;
            let mut extraction_labels = class_aliases.remove(&iri_text).unwrap_or_default();
            extraction_labels.sort();
            classes.push(ClassDecl {
                iri: validated_iri(&iri_text, "owl:Class")?,
                pref_label: Some(pref_label),
                parents,
                extraction_labels,
            });
        }

        let known_classes: BTreeSet<Iri> = classes.iter().map(|class| class.iri.clone()).collect();
        for class in &classes {
            for parent in &class.parents {
                if !known_classes.contains(parent) {
                    return Err(OntologyError::MissingParent {
                        class: class.iri.as_str().to_owned(),
                        parent: parent.as_str().to_owned(),
                    });
                }
            }
        }

        let mut properties = Vec::with_capacity(property_iris.len());
        for iri_text in property_iris {
            let pref_label = preferred_label(&labels, &iri_text, "object property")?;
            let parents = property_parents
                .remove(&iri_text)
                .unwrap_or_default()
                .into_iter()
                .map(|iri| validated_iri(&iri, "rdfs:subPropertyOf parent"))
                .collect::<Result<Vec<_>, _>>()?;
            let domains = property_domains
                .remove(&iri_text)
                .unwrap_or_default()
                .into_iter()
                .map(|iri| validated_iri(&iri, "rdfs:domain"))
                .collect::<Result<Vec<_>, _>>()?;
            let ranges = property_ranges
                .remove(&iri_text)
                .unwrap_or_default()
                .into_iter()
                .map(|iri| validated_iri(&iri, "rdfs:range"))
                .collect::<Result<Vec<_>, _>>()?;
            let mut extraction_labels = property_aliases.remove(&iri_text).unwrap_or_default();
            extraction_labels.sort();
            properties.push(PropDecl {
                iri: validated_iri(&iri_text, "owl:ObjectProperty")?,
                pref_label: Some(pref_label),
                parents,
                domains,
                ranges,
                extraction_labels,
            });
        }

        let known_properties: BTreeSet<Iri> = properties
            .iter()
            .map(|property| property.iri.clone())
            .collect();
        for property in &properties {
            for parent in &property.parents {
                if !known_properties.contains(parent) {
                    return Err(OntologyError::MissingPropertyParent {
                        property: property.iri.as_str().to_owned(),
                        parent: parent.as_str().to_owned(),
                    });
                }
            }
        }

        let prefixes = manifest
            .prefixes
            .into_iter()
            .map(|(prefix, value)| {
                validated_iri(&value, &format!("prefix {prefix:?}")).map(|iri| (prefix, iri))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;

        let class_index = classes
            .iter()
            .enumerate()
            .map(|(index, class)| (class.iri.clone(), index))
            .collect();
        let property_index = properties
            .iter()
            .enumerate()
            .map(|(index, property)| (property.iri.clone(), index))
            .collect();
        let class_alias_index = build_class_alias_index(&classes);
        let property_alias_index = build_property_alias_index(&properties);
        let (ancestors, descendants) = build_closure(&classes);

        Ok(Self {
            version_iri,
            sha256: actual_sha256,
            prefixes,
            classes,
            properties,
            class_index,
            property_index,
            class_alias_index,
            property_alias_index,
            ancestors,
            descendants,
        })
    }

    /// Return the ontology version IRI authenticated by the manifest.
    ///
    /// # Requirement
    ///
    /// REQ-OWL-1.5: classification provenance shall identify its ontology
    /// version.
    #[must_use]
    pub fn version_iri(&self) -> &Iri {
        &self.version_iri
    }

    /// Return the lowercase SHA-256 of the exact loaded artifact bytes.
    ///
    /// # Requirement
    ///
    /// REQ-OWL-1.1 and REQ-OWL-1.5: classification provenance shall identify
    /// the exact authenticated artifact.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Return the manifest's validated namespace-prefix map.
    ///
    /// # Requirement
    ///
    /// REQ-OWL-1.2: the loader shall support multiple vocabulary namespaces.
    #[must_use]
    pub fn prefixes(&self) -> &BTreeMap<String, Iri> {
        &self.prefixes
    }

    /// Return every class retained in the materialised artifact.
    ///
    /// # Requirement
    ///
    /// REQ-OWL-1.2: direct declarations and their named ancestry shall remain
    /// auditable.
    #[must_use]
    pub fn classes(&self) -> &[ClassDecl] {
        &self.classes
    }

    /// Return every object property retained in the materialised artifact.
    ///
    /// # Requirement
    ///
    /// REQ-OWL-1.2: relation identities shall be canonical object-property
    /// IRIs.
    #[must_use]
    pub fn properties(&self) -> &[PropDecl] {
        &self.properties
    }

    /// Resolve a canonical class IRI.
    ///
    /// # Requirement
    ///
    /// REQ-OWL-1.2: class lookup shall use canonical identity.
    #[must_use]
    pub fn class(&self, iri: &Iri) -> Option<&ClassDecl> {
        self.class_index.get(iri).map(|index| &self.classes[*index])
    }

    /// Resolve a canonical object-property IRI.
    ///
    /// # Requirement
    ///
    /// REQ-OWL-1.2: relation lookup shall use canonical identity.
    #[must_use]
    pub fn property(&self, iri: &Iri) -> Option<&PropDecl> {
        self.property_index
            .get(iri)
            .map(|index| &self.properties[*index])
    }

    /// Resolve an exact, curated PRISM extraction class label.
    ///
    /// Lookup is deliberately case-sensitive to preserve the validator's
    /// existing contract. Upstream `skos:prefLabel` values are not aliases
    /// unless the manifest explicitly maps them.
    ///
    /// # Requirement
    ///
    /// REQ-OWL-1.2: prompt labels and canonical IRIs shall be connected by an
    /// explicit, auditable mapping.
    #[must_use]
    pub fn class_for_label(&self, label: &str) -> Option<&ClassDecl> {
        self.class_alias_index
            .get(label)
            .map(|index| &self.classes[*index])
    }

    /// Resolve an exact, curated PRISM extraction relation label.
    ///
    /// # Requirement
    ///
    /// REQ-OWL-1.2: accepted relationship labels shall map to canonical
    /// object-property IRIs.
    #[must_use]
    pub fn property_for_label(&self, label: &str) -> Option<&PropDecl> {
        self.property_alias_index
            .get(label)
            .map(|index| &self.properties[*index])
    }

    /// Test reflexive named-class subsumption.
    ///
    /// `is_a(C, C)` is true only when `C` is a declared class. All other
    /// results use the precomputed, explicit named `rdfs:subClassOf` closure.
    ///
    /// # Requirement
    ///
    /// REQ-OWL-1.2: subsumption shall be transitive, support multiple
    /// inheritance, and terminate in the presence of RDF cycles.
    #[must_use]
    pub fn is_a(&self, sub: &Iri, sup: &Iri) -> bool {
        if sub == sup {
            return self.class_index.contains_key(sub);
        }
        self.ancestors
            .get(sub)
            .is_some_and(|ancestors| ancestors.contains(sup))
    }

    /// Return all strict named ancestors of a declared class.
    ///
    /// # Requirement
    ///
    /// REQ-OWL-1.2: callers shall be able to audit upward transitive closure.
    #[must_use]
    pub fn ancestors(&self, iri: &Iri) -> Option<&BTreeSet<Iri>> {
        self.ancestors.get(iri)
    }

    /// Return all strict named descendants of a declared class.
    ///
    /// # Requirement
    ///
    /// REQ-OWL-1.2: callers shall be able to audit inverse transitive closure.
    #[must_use]
    pub fn descendants(&self, iri: &Iri) -> Option<&BTreeSet<Iri>> {
        self.descendants.get(iri)
    }
}

/// Load the EMMO 1.0.3 artifact compiled into this crate.
///
/// The manifest and Turtle bytes are embedded together, but the same SHA-256
/// check used for filesystem inputs still runs. Embedding therefore does not
/// bypass supply-chain verification.
///
/// # Requirement
///
/// REQ-OWL-1.1: production shall load the vendored, authenticated artifact.
///
/// # Errors
///
/// Returns [`OntologyError`] if bundled bytes fail authentication or semantic
/// validation.
pub fn load_bundled_emmo() -> Result<OntologyGraph, OntologyError> {
    OntologyGraph::from_bytes(
        include_bytes!("../../../assets/ontology/emmo-1.0.3.materialised.ttl"),
        include_bytes!("../../../assets/ontology/emmo-1.0.3.provenance.json"),
    )
}

/// Load the MatKG 1.4 class vocabulary compiled into this crate.
///
/// MatKG (Venugopal & Olivetti, Scientific Data 11:217, 2024;
/// doi:10.5281/zenodo.10144972, CC BY 4.0) declares its seven NER entity
/// categories only under the placeholder namespace `http://example.com/`,
/// so this artifact re-expresses them under a PRISM-minted namespace. The
/// same SHA-256 authentication as the EMMO artifact applies; the manifest
/// carries the licence and the REQUIRED CC-BY attribution.
///
/// # Errors
///
/// Returns [`OntologyError`] if bundled bytes fail authentication or semantic
/// validation.
pub fn load_bundled_matkg() -> Result<OntologyGraph, OntologyError> {
    OntologyGraph::from_bytes(
        include_bytes!("../../../assets/ontology/matkg-1.4.materialised.ttl"),
        include_bytes!("../../../assets/ontology/matkg-1.4.provenance.json"),
    )
}

fn validated_iri(value: &str, context: &str) -> Result<Iri, OntologyError> {
    Iri::new(value.to_owned()).map_err(|_| OntologyError::InvalidIri {
        context: context.to_owned(),
        value: value.to_owned(),
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn term_iri(term: &SimpleTerm<'_>) -> Option<String> {
    term.iri().map(|iri| iri.as_str().to_owned())
}

fn term_literal(term: &SimpleTerm<'_>) -> Option<String> {
    term.lexical_form().map(|value| value.to_string())
}

fn preferred_label(
    labels: &BTreeMap<String, Vec<(u8, String)>>,
    iri: &str,
    kind: &'static str,
) -> Result<String, OntologyError> {
    let values = labels
        .get(iri)
        .ok_or_else(|| OntologyError::MissingPreferredLabel {
            kind,
            iri: iri.to_owned(),
        })?;
    let best_rank = values.first().map(|(rank, _)| *rank).ok_or_else(|| {
        OntologyError::MissingPreferredLabel {
            kind,
            iri: iri.to_owned(),
        }
    })?;
    let best = values
        .iter()
        .take_while(|(rank, _)| *rank == best_rank)
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>();
    if best.len() != 1 {
        return Err(OntologyError::AmbiguousPreferredLabel {
            kind,
            iri: iri.to_owned(),
            labels: best,
        });
    }
    Ok(best.into_iter().next().expect("length checked above"))
}

fn build_class_alias_index(classes: &[ClassDecl]) -> HashMap<String, usize> {
    classes
        .iter()
        .enumerate()
        .flat_map(|(index, class)| {
            class
                .extraction_labels
                .iter()
                .cloned()
                .map(move |label| (label, index))
        })
        .collect()
}

fn build_property_alias_index(properties: &[PropDecl]) -> HashMap<String, usize> {
    properties
        .iter()
        .enumerate()
        .flat_map(|(index, property)| {
            property
                .extraction_labels
                .iter()
                .cloned()
                .map(move |label| (label, index))
        })
        .collect()
}

fn build_closure(
    classes: &[ClassDecl],
) -> (HashMap<Iri, BTreeSet<Iri>>, HashMap<Iri, BTreeSet<Iri>>) {
    let direct: HashMap<Iri, Vec<Iri>> = classes
        .iter()
        .map(|class| (class.iri.clone(), class.parents.clone()))
        .collect();
    let mut ancestors = HashMap::with_capacity(classes.len());

    for class in classes {
        let mut visited = BTreeSet::new();
        visited.insert(class.iri.clone());
        let mut pending = class.parents.clone();
        while let Some(parent) = pending.pop() {
            if visited.insert(parent.clone())
                && let Some(grandparents) = direct.get(&parent)
            {
                pending.extend(grandparents.iter().cloned());
            }
        }
        visited.remove(&class.iri);
        ancestors.insert(class.iri.clone(), visited);
    }

    let mut descendants: HashMap<Iri, BTreeSet<Iri>> = classes
        .iter()
        .map(|class| (class.iri.clone(), BTreeSet::new()))
        .collect();
    for (class, supers) in &ancestors {
        for sup in supers {
            if let Some(children) = descendants.get_mut(sup) {
                children.insert(class.clone());
            }
        }
    }

    (ancestors, descendants)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{
        Iri, OntologyError, OntologyGraph, load_bundled_emmo, load_bundled_matkg, sha256_hex,
    };
    use serde::Deserialize;
    use serde_json::json;

    const ARTIFACT: &[u8] = include_bytes!("../../../assets/ontology/emmo-1.0.3.materialised.ttl");
    const MANIFEST: &[u8] = include_bytes!("../../../assets/ontology/emmo-1.0.3.provenance.json");
    const MATKG_ARTIFACT: &[u8] =
        include_bytes!("../../../assets/ontology/matkg-1.4.materialised.ttl");
    const MATKG_MANIFEST: &[u8] =
        include_bytes!("../../../assets/ontology/matkg-1.4.provenance.json");

    #[derive(Deserialize)]
    struct TestManifest {
        materialised_sha256: String,
        prefixes: BTreeMap<String, String>,
        class_mappings: Vec<TestMapping>,
        relation_mappings: Vec<TestMapping>,
    }

    #[derive(Deserialize)]
    struct TestMapping {
        extraction_label: String,
        iri: String,
    }

    #[test]
    fn bundled_artifact_hash_matches_manifest_at_production_loader() {
        let manifest: TestManifest = serde_json::from_slice(MANIFEST).unwrap();
        let digest = sha256_hex(ARTIFACT);
        assert_eq!(digest, manifest.materialised_sha256);

        let graph = load_bundled_emmo().unwrap();
        assert_eq!(graph.sha256(), digest);
    }

    /// The MatKG artifact is pinned exactly as EMMO's is: the vendored bytes
    /// must match the manifest digest, and the production loader must load
    /// and re-verify those same bytes. A one-byte edit to the artifact
    /// without a manifest update fails here.
    #[test]
    fn bundled_matkg_artifact_hash_matches_manifest_at_production_loader() {
        let manifest: TestManifest = serde_json::from_slice(MATKG_MANIFEST).unwrap();
        let digest = sha256_hex(MATKG_ARTIFACT);
        assert_eq!(digest, manifest.materialised_sha256);

        let graph = load_bundled_matkg().unwrap();
        assert_eq!(graph.sha256(), digest);
        assert_eq!(
            graph.version_iri().as_str(),
            "https://marc27.com/ontology/matkg/1.4"
        );
    }

    /// The MatKG manifest must carry the licence and the REQUIRED CC-BY
    /// attribution naming the authors and the dataset DOI — attribution is a
    /// licence condition, not decoration.
    #[test]
    fn matkg_manifest_records_doi_licence_and_required_attribution() {
        let manifest: serde_json::Value = serde_json::from_slice(MATKG_MANIFEST).unwrap();
        assert_eq!(manifest["license"], "CC-BY-4.0");
        assert_eq!(manifest["dataset_doi"], "10.5281/zenodo.10144972");
        let attribution = manifest["attribution"]
            .as_str()
            .expect("attribution must be present");
        for required in [
            "Venugopal",
            "Olivetti",
            "CC BY 4.0",
            "10.5281/zenodo.10144972",
        ] {
            assert!(
                attribution.contains(required),
                "attribution is missing {required:?}: {attribution}"
            );
        }
    }

    /// All seven MatKG classes and the single co-occurrence relationship
    /// resolve through the manifest mappings to declared IRIs, exactly as
    /// EMMO's do.
    #[test]
    fn every_matkg_manifest_mapping_resolves_to_its_declared_canonical_iri() {
        let manifest: TestManifest = serde_json::from_slice(MATKG_MANIFEST).unwrap();
        let graph = load_bundled_matkg().unwrap();

        assert_eq!(manifest.class_mappings.len(), 7, "MatKG declares 7 classes");
        assert_eq!(manifest.relation_mappings.len(), 1);
        for mapping in manifest.class_mappings {
            let declaration = graph
                .class_for_label(&mapping.extraction_label)
                .unwrap_or_else(|| panic!("unresolved class alias {}", mapping.extraction_label));
            assert_eq!(declaration.iri.as_str(), mapping.iri);
        }
        for mapping in manifest.relation_mappings {
            let declaration = graph
                .property_for_label(&mapping.extraction_label)
                .unwrap_or_else(|| {
                    panic!("unresolved relation alias {}", mapping.extraction_label)
                });
            assert_eq!(declaration.iri.as_str(), mapping.iri);
        }
    }

    #[test]
    fn corrupt_matkg_artifact_is_rejected_before_rdf_dispatch() {
        let mut corrupt = MATKG_ARTIFACT.to_vec();
        let byte = corrupt
            .iter_mut()
            .find(|byte| **byte == b'@')
            .expect("artifact has a prefix declaration");
        *byte = b'#';

        let error = OntologyGraph::from_bytes(&corrupt, MATKG_MANIFEST).unwrap_err();
        assert!(matches!(error, OntologyError::HashMismatch { .. }));
    }

    #[test]
    fn every_manifest_mapping_resolves_to_its_declared_canonical_iri() {
        let manifest: TestManifest = serde_json::from_slice(MANIFEST).unwrap();
        let graph = load_bundled_emmo().unwrap();

        for mapping in manifest.class_mappings {
            let declaration = graph
                .class_for_label(&mapping.extraction_label)
                .unwrap_or_else(|| panic!("unresolved class alias {}", mapping.extraction_label));
            assert_eq!(declaration.iri.as_str(), mapping.iri);
            assert!(graph.class(&declaration.iri).is_some());
        }
        for mapping in manifest.relation_mappings {
            let declaration = graph
                .property_for_label(&mapping.extraction_label)
                .unwrap_or_else(|| {
                    panic!("unresolved relation alias {}", mapping.extraction_label)
                });
            assert_eq!(declaration.iri.as_str(), mapping.iri);
            assert!(graph.property(&declaration.iri).is_some());
        }
    }

    #[test]
    fn every_manifest_prefix_is_exposed_as_a_validated_iri() {
        let manifest: TestManifest = serde_json::from_slice(MANIFEST).unwrap();
        let graph = load_bundled_emmo().unwrap();
        assert_eq!(graph.prefixes().len(), manifest.prefixes.len());
        for (prefix, expected) in manifest.prefixes {
            let actual = graph
                .prefixes()
                .get(&prefix)
                .unwrap_or_else(|| panic!("manifest prefix {prefix:?} was dropped"));
            assert_eq!(actual.as_str(), expected);
        }
    }

    #[test]
    fn contains_element_range_conclusion_is_materialised_for_rdfs_queries() {
        let graph = load_bundled_emmo().unwrap();
        let element = graph
            .class_for_label("Element")
            .expect("Element extraction alias resolves");
        let chemical_species =
            Iri::new("https://w3id.org/emmo#EMMO_cbcf8fe6_6da6_49e0_ab4d_00f737ea9689".to_owned())
                .unwrap();
        let species_decl = graph
            .class(&chemical_species)
            .expect("the entailed parent and its ancestry are vendored");
        assert_eq!(species_decl.pref_label.as_deref(), Some("ChemicalSpecies"));
        assert!(
            graph.is_a(&element.iri, &chemical_species),
            "ChemicalElement must be below the hasChemicalSpecies range without runtime OWL reasoning"
        );
        assert!(
            graph.property_for_label("CONTAINS").is_some(),
            "the range conclusion must accompany the mapped relation"
        );
    }

    #[test]
    fn closure_matches_independent_warshall_computation_in_both_directions() {
        let graph = load_bundled_emmo().unwrap();
        let classes = graph.classes();
        let positions: BTreeMap<Iri, usize> = classes
            .iter()
            .enumerate()
            .map(|(index, class)| (class.iri.clone(), index))
            .collect();
        let mut reach = vec![vec![false; classes.len()]; classes.len()];
        for (child_index, class) in classes.iter().enumerate() {
            for parent in &class.parents {
                reach[child_index][positions[parent]] = true;
            }
        }
        for via in 0..classes.len() {
            for child in 0..classes.len() {
                for parent in 0..classes.len() {
                    reach[child][parent] |= reach[child][via] && reach[via][parent];
                }
            }
        }

        for (child_index, class) in classes.iter().enumerate() {
            let expected_ancestors: BTreeSet<Iri> = classes
                .iter()
                .enumerate()
                .filter(|(parent_index, _)| {
                    *parent_index != child_index && reach[child_index][*parent_index]
                })
                .map(|(_, parent)| parent.iri.clone())
                .collect();
            assert_eq!(graph.ancestors(&class.iri).unwrap(), &expected_ancestors);

            let expected_descendants: BTreeSet<Iri> = classes
                .iter()
                .enumerate()
                .filter(|(candidate_index, _)| {
                    *candidate_index != child_index && reach[*candidate_index][child_index]
                })
                .map(|(_, child)| child.iri.clone())
                .collect();
            assert_eq!(
                graph.descendants(&class.iri).unwrap(),
                &expected_descendants
            );
        }
    }

    #[test]
    fn subclass_cycles_terminate_and_keep_closure_strict() {
        let artifact = br#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
<https://example.test/ontology> a owl:Ontology ;
    owl:versionIRI <https://example.test/1/ontology> .
<https://example.test/A> a owl:Class ;
    rdfs:subClassOf <https://example.test/B> ;
    skos:prefLabel "A"@en .
<https://example.test/B> a owl:Class ;
    rdfs:subClassOf <https://example.test/A> ;
    skos:prefLabel "B"@en .
"#;
        let manifest = synthetic_manifest(artifact);
        let graph = OntologyGraph::from_bytes(artifact, &manifest).unwrap();
        let a = Iri::new("https://example.test/A".to_owned()).unwrap();
        let b = Iri::new("https://example.test/B".to_owned()).unwrap();

        assert_eq!(graph.ancestors(&a).unwrap(), &BTreeSet::from([b.clone()]));
        assert_eq!(graph.descendants(&a).unwrap(), &BTreeSet::from([b.clone()]));
        assert!(graph.is_a(&a, &a));
        assert!(graph.is_a(&a, &b));
        assert!(graph.is_a(&b, &a));
    }

    /// A customer-supplied ontology may use any language. Navigation retains
    /// its RDF declarations rather than relying on extraction aliases or a
    /// built-in vocabulary.
    #[test]
    fn object_property_navigation_retains_named_rdf_declarations() {
        let artifact = br#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
<https://beispiel.invalid/ontologie> a owl:Ontology ;
    owl:versionIRI <https://beispiel.invalid/ontologie/7> .
<https://beispiel.invalid/klasse/Stoff> a owl:Class ;
    skos:prefLabel "Stoff"@de .
<https://beispiel.invalid/klasse/Arzneistoff> a owl:Class ;
    rdfs:subClassOf <https://beispiel.invalid/klasse/Stoff> ;
    skos:prefLabel "Arzneistoff"@de .
<https://beispiel.invalid/klasse/Krankheit> a owl:Class ;
    skos:prefLabel "Krankheit"@de .
<https://beispiel.invalid/relation/wirktAuf> a owl:ObjectProperty ;
    skos:prefLabel "wirkt auf"@de .
<https://beispiel.invalid/relation/behandelt> a owl:ObjectProperty ;
    rdfs:subPropertyOf <https://beispiel.invalid/relation/wirktAuf> ;
    rdfs:domain <https://beispiel.invalid/klasse/Arzneistoff> ;
    rdfs:range <https://beispiel.invalid/klasse/Krankheit> ;
    skos:prefLabel "behandelt"@de .
"#;
        let manifest = serde_json::to_vec(&json!({
            "ontology_iri": "https://beispiel.invalid/ontologie",
            "version_iri": "https://beispiel.invalid/ontologie/7",
            "materialised_sha256": sha256_hex(artifact),
            "prefixes": { "de": "https://beispiel.invalid/" },
            "class_mappings": [],
            "relation_mappings": []
        }))
        .unwrap();

        let graph = OntologyGraph::from_bytes(artifact, &manifest).unwrap();
        let relation = Iri::new("https://beispiel.invalid/relation/behandelt".to_owned()).unwrap();
        let declaration = graph.property(&relation).unwrap();

        assert_eq!(declaration.pref_label.as_deref(), Some("behandelt"));
        assert_eq!(
            declaration
                .parents
                .iter()
                .map(Iri::as_str)
                .collect::<Vec<_>>(),
            ["https://beispiel.invalid/relation/wirktAuf"]
        );
        assert_eq!(
            declaration
                .domains
                .iter()
                .map(Iri::as_str)
                .collect::<Vec<_>>(),
            ["https://beispiel.invalid/klasse/Arzneistoff"]
        );
        assert_eq!(
            declaration
                .ranges
                .iter()
                .map(Iri::as_str)
                .collect::<Vec<_>>(),
            ["https://beispiel.invalid/klasse/Krankheit"]
        );
    }

    #[test]
    fn undeclared_object_property_parent_is_rejected() {
        let artifact = br#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
<https://example.test/ontology> a owl:Ontology ;
    owl:versionIRI <https://example.test/1/ontology> .
<https://example.test/relation> a owl:ObjectProperty ;
    rdfs:subPropertyOf <https://example.test/missing> ;
    skos:prefLabel "relation"@en .
"#;
        let manifest = synthetic_manifest(artifact);

        let error = OntologyGraph::from_bytes(artifact, &manifest).unwrap_err();
        assert!(matches!(error, OntologyError::MissingPropertyParent { .. }));
    }

    #[test]
    fn corrupt_artifact_is_rejected_before_rdf_dispatch() {
        let mut corrupt = ARTIFACT.to_vec();
        let byte = corrupt
            .iter_mut()
            .find(|byte| **byte == b'@')
            .expect("artifact has a prefix declaration");
        *byte = b'#';

        let error = OntologyGraph::from_bytes(&corrupt, MANIFEST).unwrap_err();
        assert!(matches!(error, OntologyError::HashMismatch { .. }));
    }

    #[test]
    fn extraction_aliases_do_not_overwrite_upstream_labels() {
        let graph = load_bundled_emmo().unwrap();
        let alloy = graph.class_for_label("Alloy").unwrap();
        assert_eq!(alloy.pref_label.as_deref(), Some("MetallicMaterial"));
        assert!(alloy.extraction_labels.iter().any(|label| label == "Alloy"));
        assert!(graph.class_for_label("alloy").is_none());
    }

    fn synthetic_manifest(artifact: &[u8]) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "ontology_iri": "https://example.test/ontology",
            "version_iri": "https://example.test/1/ontology",
            "materialised_sha256": sha256_hex(artifact),
            "prefixes": { "ex": "https://example.test/" },
            "class_mappings": [],
            "relation_mappings": []
        }))
        .unwrap()
    }
}
