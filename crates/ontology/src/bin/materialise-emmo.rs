/*
 * @file    materialise-emmo.rs
 * @brief   Reproducibly derives PRISM's audited EMMO 1.0.3 Turtle subset.
 *
 * @project PRISM / Ontology
 * @req     REQ-OWL-1.1
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
 *   2026-08-09 - 1.0.0 - Mirdyne - Initial deterministic materialiser.
 *
 * @note    All 58 upstream Turtle files are parsed with Sophia. Only explicit
 *          named RDFS ancestry is emitted; OWL 2 DL inference is not claimed.
 * @warning The input directory must be the pristine EMMO 1.0.3 source tree.
 */
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::Write as _;
use std::io::{Error as IoError, ErrorKind};
use std::path::{Path, PathBuf};

use serde_json::json;
use sha2::{Digest, Sha256};
use sophia::api::prelude::{Term, TripleSource};
use sophia::api::term::SimpleTerm;
use sophia::turtle::parser::turtle;

const EXPECTED_TURTLE_FILES: usize = 58;
const ONTOLOGY_IRI: &str = "https://w3id.org/emmo/emmo";
const VERSION_IRI: &str = "https://w3id.org/emmo/1.0.3/emmo";
const UPSTREAM_URL: &str = "https://github.com/emmo-repo/EMMO/archive/refs/tags/1.0.3.tar.gz";
const UPSTREAM_SHA256: &str = "e2f4d10db76e97c2a02cc08829387f9cbfa028c9c6c05a27d4dd9023c7179e79";
const EXPECTED_SOURCE_INPUT_SHA256: &str =
    "921e013f28655e605a922341a76ee6d14b43fc0d03d691633cfe7a6deddf71dc";
const LICENSE_IRI: &str = "https://creativecommons.org/licenses/by/4.0/legalcode";
const PUBLISHER_IRI: &str = "https://w3id.org/emmo#EMMC_ASBL";
const CREATORS: [&str; 3] = [
    "https://orcid.org/0000-0003-3805-8761",
    "https://orcid.org/0000-0002-4181-2852",
    "https://orcid.org/0000-0002-1560-809X",
];
const CONTRIBUTORS: [&str; 6] = [
    "https://orcid.org/0000-0003-0514-9229",
    "https://orcid.org/0000-0001-8869-3718",
    "https://orcid.org/0009-0008-8009-5009",
    "https://orcid.org/0000-0003-4065-9742",
    "https://orcid.org/0000-0001-7815-6636",
    "https://orcid.org/0000-0002-8758-6109",
];
const UNITS_PARSE_DEFECT: &str = "<https://w3id.org/emmo/1.0.3/disciplines/units/otherunits> .\n                                          dcterms:abstract";
const UNITS_PARSE_REPAIR: &str = "<https://w3id.org/emmo/1.0.3/disciplines/units/otherunits> ;\n                                          dcterms:abstract";

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const SKOS_PREF_LABEL: &str = "http://www.w3.org/2004/02/skos/core#prefLabel";
const OWL_CLASS: &str = "http://www.w3.org/2002/07/owl#Class";
const OWL_EQUIVALENT_CLASS: &str = "http://www.w3.org/2002/07/owl#equivalentClass";
const OWL_OBJECT_PROPERTY: &str = "http://www.w3.org/2002/07/owl#ObjectProperty";
const OWL_ONTOLOGY: &str = "http://www.w3.org/2002/07/owl#Ontology";
const OWL_UNION_OF: &str = "http://www.w3.org/2002/07/owl#unionOf";
const OWL_VERSION_IRI: &str = "http://www.w3.org/2002/07/owl#versionIRI";
const DCTERMS_CONTRIBUTOR: &str = "http://purl.org/dc/terms/contributor";
const DCTERMS_CREATOR: &str = "http://purl.org/dc/terms/creator";
const DCTERMS_LICENSE: &str = "http://purl.org/dc/terms/license";
const DCTERMS_PUBLISHER: &str = "http://purl.org/dc/terms/publisher";

const MATERIAL: &str = "https://w3id.org/emmo#EMMO_4207e895_8b83_4318_996a_72cfb32acd94";
const METALLIC_MATERIAL: &str = "https://w3id.org/emmo#EMMO_4c1f58cd_6e2c_48fb_8098_1cbb762abb05";
const CHEMICAL_ELEMENT: &str = "https://w3id.org/emmo#EMMO_4f40def1_3cd7_4067_9596_541e9a5134cf";
const CHEMICAL_SPECIES: &str = "https://w3id.org/emmo#EMMO_cbcf8fe6_6da6_49e0_ab4d_00f737ea9689";
const PROPERTY: &str = "https://w3id.org/emmo#EMMO_b7bcff25_ffc3_474e_9ab5_01b1664bd4ba";
const PROCESS: &str = "https://w3id.org/emmo#EMMO_43e9a05d_98af_41b4_92f6_00f79a09bfce";
const PHASE_OF_MATTER: &str = "https://w3id.org/emmo#EMMO_668fbd5b_6f1b_405c_9c6b_d6067bd0595a";
const DOCUMENT: &str = "https://w3id.org/emmo#EMMO_ccdc1a41_6e96_416b_92ec_efe67917434a";
const DATASET: &str = "https://w3id.org/emmo#EMMO_194e367c_9783_4bf5_96d0_9ad597d48d9a";

const HAS_CHEMICAL_SPECIES: &str =
    "https://w3id.org/emmo#EMMO_7aec67a4_fb96_47c6_80a3_8fea557b2cbb";
const HAS_PROPERTY: &str = "https://w3id.org/emmo#EMMO_e1097637_70d2_4895_973f_2396f04fa204";
const MANUFACTURED_WITH: &str = "https://w3id.org/emmo#EMMO_b1c64830_45c7_499d_9a72_68f517d57823";
const IS_PART_OF: &str = "https://w3id.org/emmo#EMMO_a8bd7094_6b40_47af_b1f4_a69d81a3afbd";
const HAS_CONSTITUENT: &str = "https://w3id.org/emmo#EMMO_dba27ca1_33c9_4443_a912_1519ce4c39ec";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Label {
    value: String,
    language: Option<String>,
}

#[derive(Default)]
struct SourceGraph {
    classes: BTreeSet<String>,
    properties: BTreeSet<String>,
    labels: BTreeMap<String, BTreeSet<Label>>,
    parents: BTreeMap<String, BTreeSet<String>>,
    named_facts: BTreeSet<(String, String, String)>,
    named_union_memberships: BTreeSet<(String, String)>,
}

/// Materialise the audited EMMO subset and write its provenance manifest.
///
/// @req REQ-OWL-1.1 - The vendored artifact shall be deterministic,
/// attributable, and reproducible from the pinned upstream release.
///
/// @pre The input tree contains exactly the 58 Turtle files from EMMO 1.0.3.
/// @post The Turtle file is written before a manifest containing its SHA-256.
/// @warning Existing output paths are replaced only when every input parses and
/// the requested declarations and ancestry validate.
fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os();
    let program = args.next().unwrap_or_default();
    let input = args.next().map(PathBuf::from);
    let artifact = args.next().map(PathBuf::from);
    let manifest = args.next().map(PathBuf::from);
    if input.is_none() || artifact.is_none() || manifest.is_none() || args.next().is_some() {
        return Err(invalid_data(format!(
            "usage: {} <emmo-source-dir> <artifact.ttl> <provenance.json>",
            Path::new(&program).display()
        ))
        .into());
    }
    let input = input.expect("validated above");
    let artifact = artifact.expect("validated above");
    let manifest = manifest.expect("validated above");

    let files = turtle_files(&input)?;
    if files.len() != EXPECTED_TURTLE_FILES {
        return Err(invalid_data(format!(
            "expected {EXPECTED_TURTLE_FILES} Turtle files under {}, found {}",
            input.display(),
            files.len()
        ))
        .into());
    }

    let source_input_sha256 = source_input_sha256(&input, &files)?;
    if source_input_sha256 != EXPECTED_SOURCE_INPUT_SHA256 {
        return Err(invalid_data(format!(
            "EMMO source input SHA-256 mismatch: expected {EXPECTED_SOURCE_INPUT_SHA256}, got {source_input_sha256}"
        ))
        .into());
    }

    let mut source = parse_source(&files)?;
    validate_upstream_metadata(&source)?;
    materialise_required_owl_conclusions(&mut source)?;
    let selected_classes = selected_classes();
    let retained_classes = named_ancestry(&source, selected_classes.values())?;
    let selected_properties = selected_properties();
    for iri in selected_properties.values() {
        if !source.properties.contains(*iri) {
            return Err(invalid_data(format!(
                "selected object property is not declared upstream: {iri}"
            ))
            .into());
        }
    }

    let turtle = render_turtle(&source, &retained_classes, selected_properties.values())?;
    let artifact_sha256 = sha256_hex(turtle.as_bytes());
    let provenance = render_manifest(&artifact_sha256, &source_input_sha256, files.len());
    validate_rendered_manifest_mappings(&provenance, &selected_classes, &selected_properties)?;

    std::fs::write(&artifact, turtle)?;
    std::fs::write(&manifest, provenance)?;
    println!(
        "wrote {} and {} from {} Turtle files",
        artifact.display(),
        manifest.display(),
        files.len()
    );
    Ok(())
}

/// Recursively enumerate Turtle inputs in deterministic path order.
///
/// @req REQ-OWL-1.1 - Every upstream module shall be parsed.
/// @post Returned paths are sorted and contain only regular `.ttl` files.
fn turtle_files(root: &Path) -> Result<Vec<PathBuf>, IoError> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "ttl") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Hash every derivation input (the 58 Turtle files plus the upstream
/// licence) with unambiguous path and byte-length framing.
///
/// This tree digest pins the extracted input actually consumed by the
/// generator. The release-tarball digest remains separately recorded in the
/// manifest; together they prevent a modified 58-file scratch tree from
/// silently inheriting the official tarball's provenance.
///
/// @req REQ-OWL-1.1 - Derivation input shall be authenticated, not inferred
/// from file count and ontology metadata alone.
fn source_input_sha256(root: &Path, turtle_files: &[PathBuf]) -> Result<String, IoError> {
    let mut inputs = turtle_files.to_vec();
    inputs.push(root.join("LICENSE"));
    inputs.sort();

    let mut digest = Sha256::new();
    digest.update(b"PRISM EMMO source input v1\0");
    for path in inputs {
        let relative = path.strip_prefix(root).map_err(|_| {
            invalid_data(format!(
                "source input {} is outside {}",
                path.display(),
                root.display()
            ))
        })?;
        let relative = relative
            .components()
            .map(|component| {
                component.as_os_str().to_str().ok_or_else(|| {
                    invalid_data(format!("non-UTF-8 source path: {}", path.display()))
                })
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("/");
        let bytes = std::fs::read(&path)?;
        digest.update((relative.len() as u64).to_be_bytes());
        digest.update(relative.as_bytes());
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(&bytes);
    }
    let bytes = digest.finalize();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(output)
}

/// Parse every upstream file with Sophia and retain the RDF facts needed for
/// deterministic materialisation.
///
/// @req REQ-OWL-1.1 - Derivation shall use a standards-compliant Turtle parser.
/// @post Named classes, object properties, labels, and named superclass edges
/// from every input are represented in the returned graph.
fn parse_source(files: &[PathBuf]) -> Result<SourceGraph, Box<dyn Error>> {
    let mut graph = SourceGraph::default();
    for path in files {
        let text = std::fs::read_to_string(path)?;
        let text = turtle_for_sophia(path, &text)?;
        let triples: Vec<[SimpleTerm<'static>; 3]> = turtle::parse_str(&text)
            .collect_triples()
            .map_err(|error| {
                invalid_data(format!(
                    "Sophia failed to parse {}: {error}",
                    path.display()
                ))
            })?;
        graph
            .named_union_memberships
            .extend(named_union_memberships(&triples)?);
        for triple in triples {
            let subject = term_iri(&triple[0]);
            let predicate = term_iri(&triple[1]);
            let object = term_iri(&triple[2]);
            if let (Some(subject), Some(predicate), Some(object)) =
                (subject.as_ref(), predicate.as_ref(), object.as_ref())
            {
                graph
                    .named_facts
                    .insert((subject.clone(), predicate.clone(), object.clone()));
            }
            if predicate.as_deref() == Some(RDF_TYPE) && object.as_deref() == Some(OWL_CLASS) {
                if let Some(subject) = subject.as_ref() {
                    graph.classes.insert(subject.clone());
                }
            } else if predicate.as_deref() == Some(RDF_TYPE)
                && object.as_deref() == Some(OWL_OBJECT_PROPERTY)
                && let Some(subject) = subject.as_ref()
            {
                graph.properties.insert(subject.clone());
            }
            if predicate.as_deref() == Some(SKOS_PREF_LABEL)
                && let (Some(subject), Some(value)) = (subject.as_ref(), triple[2].lexical_form())
            {
                graph
                    .labels
                    .entry(subject.clone())
                    .or_default()
                    .insert(Label {
                        value: value.to_string(),
                        language: triple[2]
                            .language_tag()
                            .map(|language| language.as_str().to_owned()),
                    });
            }
            if predicate.as_deref() == Some(RDFS_SUBCLASS_OF)
                && let (Some(subject), Some(parent)) = (subject, object)
            {
                graph.parents.entry(subject).or_default().insert(parent);
            }
        }
    }
    Ok(graph)
}

/// Extract the sound named-class consequences of `C owl:equivalentClass
/// [ owl:unionOf (...) ]`: every named union member is an `rdfs:subClassOf`
/// `C`. Blank-node scope is local to one parsed Turtle document, so this runs
/// before that document's triples are merged into [`SourceGraph`].
fn named_union_memberships(
    triples: &[[SimpleTerm<'static>; 3]],
) -> Result<BTreeSet<(String, String)>, IoError> {
    let mut conclusions = BTreeSet::new();
    for equivalent in triples {
        let Some(union_class) = term_iri(&equivalent[0]) else {
            continue;
        };
        if term_iri(&equivalent[1]).as_deref() != Some(OWL_EQUIVALENT_CLASS) {
            continue;
        }
        for union in triples.iter().filter(|triple| {
            Term::eq(&triple[0], &equivalent[2])
                && term_iri(&triple[1]).as_deref() == Some(OWL_UNION_OF)
        }) {
            for member in rdf_list_named_members(triples, &union[2])? {
                conclusions.insert((member, union_class.clone()));
            }
        }
    }
    Ok(conclusions)
}

/// Traverse one RDF collection iteratively, rejecting malformed or cyclic
/// list structure instead of guessing at an OWL expression.
fn rdf_list_named_members(
    triples: &[[SimpleTerm<'static>; 3]],
    head: &SimpleTerm<'static>,
) -> Result<Vec<String>, IoError> {
    let mut current = head.clone();
    let mut visited = BTreeSet::new();
    let mut members = Vec::new();
    loop {
        if term_iri(&current).as_deref() == Some(RDF_NIL) {
            return Ok(members);
        }
        if !visited.insert(current.clone()) {
            return Err(invalid_data("cyclic RDF list in owl:unionOf expression"));
        }

        let first: BTreeSet<SimpleTerm<'static>> = triples
            .iter()
            .filter(|triple| {
                Term::eq(&triple[0], &current) && term_iri(&triple[1]).as_deref() == Some(RDF_FIRST)
            })
            .map(|triple| triple[2].clone())
            .collect();
        let rest: BTreeSet<SimpleTerm<'static>> = triples
            .iter()
            .filter(|triple| {
                Term::eq(&triple[0], &current) && term_iri(&triple[1]).as_deref() == Some(RDF_REST)
            })
            .map(|triple| triple[2].clone())
            .collect();
        if first.len() != 1 || rest.len() != 1 {
            return Err(invalid_data(format!(
                "malformed RDF list node: expected one rdf:first and rdf:rest, got {} and {}",
                first.len(),
                rest.len()
            )));
        }
        if let Some(member) = first.first().and_then(term_iri) {
            members.push(member);
        }
        current = rest.first().expect("length checked above").clone();
    }
}

/// Add the specific OWL conclusion required to use `hasChemicalSpecies` for
/// PRISM `Element` nodes. The conclusion is accepted only when Sophia found
/// the supporting equivalent-union axiom in the pinned source.
fn materialise_required_owl_conclusions(source: &mut SourceGraph) -> Result<(), IoError> {
    let required = (CHEMICAL_ELEMENT.to_owned(), CHEMICAL_SPECIES.to_owned());
    if !source.named_union_memberships.contains(&required) {
        return Err(invalid_data(format!(
            "required OWL union conclusion is not entailed: <{CHEMICAL_ELEMENT}> rdfs:subClassOf <{CHEMICAL_SPECIES}>"
        )));
    }
    source
        .parents
        .entry(CHEMICAL_ELEMENT.to_owned())
        .or_default()
        .insert(CHEMICAL_SPECIES.to_owned());
    Ok(())
}

/// Repair the single pinned EMMO 1.0.3 Turtle syntax defect before parsing.
///
/// Upstream `disciplines/units/units.ttl:22` terminates its ontology predicate
/// list with `.` immediately before another predicate at line 23. Replacing
/// that terminator with `;` preserves the evident subject and makes the file
/// valid Turtle. The exact-context check prevents this compatibility repair
/// from silently applying to any other source.
///
/// @req REQ-OWL-1.1 - Every pinned upstream module shall be parsed, and any
/// source repair shall be deterministic and auditable.
/// @post Only the known line-22 terminator can differ from the input text.
fn turtle_for_sophia<'a>(path: &Path, text: &'a str) -> Result<Cow<'a, str>, IoError> {
    if !path.ends_with(Path::new("disciplines/units/units.ttl")) {
        return Ok(Cow::Borrowed(text));
    }
    if text.matches(UNITS_PARSE_DEFECT).count() != 1 {
        return Err(invalid_data(format!(
            "known EMMO 1.0.3 units.ttl syntax defect did not match exactly once in {}",
            path.display()
        )));
    }
    Ok(Cow::Owned(text.replacen(
        UNITS_PARSE_DEFECT,
        UNITS_PARSE_REPAIR,
        1,
    )))
}

/// Verify that provenance copied into the manifest is present in the source.
///
/// @req REQ-OWL-1.1 - Attribution and version metadata shall be source facts,
/// not generator assumptions.
/// @post The pinned ontology, version, licence, publisher, creators, and
/// contributors have all been observed as named RDF statements.
fn validate_upstream_metadata(source: &SourceGraph) -> Result<(), IoError> {
    let mut required = vec![
        (ONTOLOGY_IRI, RDF_TYPE, OWL_ONTOLOGY),
        (ONTOLOGY_IRI, OWL_VERSION_IRI, VERSION_IRI),
        (ONTOLOGY_IRI, DCTERMS_LICENSE, LICENSE_IRI),
        (ONTOLOGY_IRI, DCTERMS_PUBLISHER, PUBLISHER_IRI),
    ];
    required.extend(
        CREATORS
            .iter()
            .map(|creator| (ONTOLOGY_IRI, DCTERMS_CREATOR, *creator)),
    );
    required.extend(
        CONTRIBUTORS
            .iter()
            .map(|contributor| (ONTOLOGY_IRI, DCTERMS_CONTRIBUTOR, *contributor)),
    );
    for (subject, predicate, object) in required {
        if !source.named_facts.contains(&(
            subject.to_owned(),
            predicate.to_owned(),
            object.to_owned(),
        )) {
            return Err(invalid_data(format!(
                "required upstream provenance statement is absent: <{subject}> <{predicate}> <{object}>"
            )));
        }
    }
    Ok(())
}

/// Compute the complete explicit named-superclass ancestry iteratively.
///
/// @req REQ-OWL-1.2 - Multiple inheritance and cycles shall terminate safely.
/// @post The returned set contains every selected class and every reachable
/// named `rdfs:subClassOf` ancestor, with no blank-node OWL restriction.
fn named_ancestry<'a>(
    source: &SourceGraph,
    selected: impl Iterator<Item = &'a &'static str>,
) -> Result<BTreeSet<String>, IoError> {
    let mut retained = BTreeSet::new();
    let mut pending: Vec<String> = selected.map(|iri| (*iri).to_owned()).collect();
    while let Some(iri) = pending.pop() {
        if !source.classes.contains(&iri) {
            return Err(invalid_data(format!(
                "selected or ancestral class is not declared upstream: {iri}"
            )));
        }
        if !retained.insert(iri.clone()) {
            continue;
        }
        if let Some(parents) = source.parents.get(&iri) {
            pending.extend(parents.iter().cloned());
        }
    }
    Ok(retained)
}

/// Render a stable, human-auditable Turtle artifact.
///
/// @req REQ-OWL-1.1 - The vendored subset shall preserve source attribution,
/// class labels, direct named parents, and selected object-property identities.
/// @post Statements are ordered by canonical IRI and serialize identically for
/// identical source facts.
fn render_turtle<'a>(
    source: &SourceGraph,
    retained_classes: &BTreeSet<String>,
    selected_properties: impl Iterator<Item = &'a &'static str>,
) -> Result<String, IoError> {
    let mut output = String::new();
    writeln!(
        output,
        "# PRISM materialised subset of EMMO 1.0.3.\n# Contains material by the EMMO authors and contributors; publisher EMMC ASBL.\n# Source: {UPSTREAM_URL}\n# Licensed under CC BY 4.0: {LICENSE_IRI}\n"
    )
    .expect("writing to String cannot fail");
    output.push_str("@prefix dcterms: <http://purl.org/dc/terms/> .\n");
    output.push_str("@prefix emmo: <https://w3id.org/emmo#> .\n");
    output.push_str("@prefix owl: <http://www.w3.org/2002/07/owl#> .\n");
    output.push_str("@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n");
    output.push_str("@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n");
    output.push_str("@prefix skos: <http://www.w3.org/2004/02/skos/core#> .\n\n");
    writeln!(
        output,
        "<{ONTOLOGY_IRI}> a owl:Ontology ;\n    owl:versionIRI <{VERSION_IRI}> ;\n    dcterms:creator <{}>, <{}>, <{}> ;\n    dcterms:contributor <{}>, <{}>, <{}>, <{}>, <{}>, <{}> ;\n    dcterms:publisher <{PUBLISHER_IRI}> ;\n    dcterms:license <{LICENSE_IRI}> ;\n    dcterms:source <{UPSTREAM_URL}> ;\n    dcterms:title \"PRISM materialised subset of EMMO 1.0.3\"@en .\n",
        CREATORS[0],
        CREATORS[1],
        CREATORS[2],
        CONTRIBUTORS[0],
        CONTRIBUTORS[1],
        CONTRIBUTORS[2],
        CONTRIBUTORS[3],
        CONTRIBUTORS[4],
        CONTRIBUTORS[5],
    )
    .expect("writing to String cannot fail");

    for iri in retained_classes {
        let parents = source.parents.get(iri).cloned().unwrap_or_default();
        for parent in &parents {
            if !retained_classes.contains(parent) {
                return Err(invalid_data(format!(
                    "retained class {iri} has omitted named parent {parent}"
                )));
            }
        }
        let label = required_preferred_label(source.labels.get(iri), iri)?;
        if iri == CHEMICAL_ELEMENT {
            output.push_str(
                "# Materialised OWL conclusion: ChemicalElement rdfs:subClassOf ChemicalSpecies.\n\
# Basis: ChemicalSpecies owl:equivalentClass an owl:unionOf containing ChemicalElement.\n",
            );
        }
        writeln!(output, "<{iri}> a owl:Class").expect("writing to String cannot fail");
        if !parents.is_empty() {
            output.push_str("    ; rdfs:subClassOf ");
            for (index, parent) in parents.iter().enumerate() {
                if index > 0 {
                    output.push_str(",\n        ");
                }
                write!(output, "<{parent}>").expect("writing to String cannot fail");
            }
            output.push('\n');
        }
        output.push_str("    ; skos:prefLabel ");
        render_label(&mut output, &label);
        output.push('\n');
        output.push_str("    .\n\n");
    }

    let selected_properties: BTreeSet<&str> = selected_properties.copied().collect();
    for iri in selected_properties {
        let label = required_preferred_label(source.labels.get(iri), iri)?;
        writeln!(output, "<{iri}> a owl:ObjectProperty").expect("writing to String cannot fail");
        output.push_str("    ; skos:prefLabel ");
        render_label(&mut output, &label);
        output.push('\n');
        output.push_str("    .\n\n");
    }
    Ok(output)
}

/// Choose an upstream preferred label without silently resolving ambiguity.
///
/// @req REQ-OWL-1.1 - Human labels shall be source facts, not generator guesses.
/// @post English is preferred, then an untagged singleton, then a sole
/// non-English label. Equal-priority ambiguity is an error.
fn required_preferred_label(
    labels: Option<&BTreeSet<Label>>,
    subject: &str,
) -> Result<Label, IoError> {
    let Some(labels) = labels else {
        return Err(invalid_data(format!(
            "retained declaration has no skos:prefLabel: {subject}"
        )));
    };
    let rank = |label: &Label| match label.language.as_deref() {
        Some(language) if language.eq_ignore_ascii_case("en") => 0,
        None => 1,
        Some(_) => 2,
    };
    let best_rank = labels.iter().map(rank).min().expect("nonempty set");
    let best: Vec<&Label> = labels
        .iter()
        .filter(|label| rank(label) == best_rank)
        .collect();
    if best.len() != 1 {
        return Err(invalid_data(format!(
            "ambiguous skos:prefLabel values for {subject}: {best:?}"
        )));
    }
    Ok(best[0].clone())
}

/// Render one validated source label as a Turtle literal.
///
/// @req REQ-OWL-1.1 - Generated Turtle shall preserve label text and language.
/// @post Reserved Turtle string characters are escaped.
fn render_label(output: &mut String, label: &Label) {
    output.push('"');
    for character in label.value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            other => output.push(other),
        }
    }
    output.push('"');
    if let Some(language) = &label.language {
        output.push('@');
        output.push_str(language);
    }
}

/// Render the provenance manifest containing the artifact digest and curated
/// extraction mappings.
///
/// @req REQ-OWL-1.1 - The manifest shall record upstream provenance, required
/// CC BY attribution, and the exact materialised artifact digest.
/// @post Output is stable pretty-printed JSON terminated by one newline.
fn render_manifest(
    artifact_sha256: &str,
    source_input_sha256: &str,
    source_file_count: usize,
) -> String {
    let value = json!({
        "schema_version": 1,
        "ontology_iri": ONTOLOGY_IRI,
        "version_iri": VERSION_IRI,
        "release": "1.0.3",
        "release_published": "2026-01-20",
        "upstream_url": UPSTREAM_URL,
        "upstream_tarball_sha256": UPSTREAM_SHA256,
        "source_input_sha256": source_input_sha256,
        "source_input_sha256_algorithm": "SHA-256 over PRISM EMMO source input v1 framing of each sorted relative path and byte payload: 58 Turtle files plus LICENSE",
        "retrieved": "2026-08-09",
        "license": "CC-BY-4.0",
        "license_url": LICENSE_IRI,
        "attribution": "Contains material from the Elementary Multiperspective Material Ontology (EMMO) 1.0.3 by the EMMO authors and contributors, published by EMMC ASBL, used under CC BY 4.0.",
        "modifications": "PRISM selected and deterministically re-serialized the required declarations and named ancestry, and added the documented OWL-entailed ChemicalElement-to-ChemicalSpecies subclass conclusion. The upstream source files are not modified.",
        "creators": CREATORS,
        "contributors": CONTRIBUTORS,
        "publisher": PUBLISHER_IRI,
        "source_file_count": source_file_count,
        "materialised_sha256": artifact_sha256,
        "prefixes": {
            "bibo": "http://purl.org/ontology/bibo/",
            "dcterms": "http://purl.org/dc/terms/",
            "emmo": "https://w3id.org/emmo#",
            "foaf": "http://xmlns.com/foaf/0.1/",
            "owl": "http://www.w3.org/2002/07/owl#",
            "rdf": "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
            "rdfs": "http://www.w3.org/2000/01/rdf-schema#",
            "skos": "http://www.w3.org/2004/02/skos/core#",
            "xsd": "http://www.w3.org/2001/XMLSchema#"
        },
        "class_mappings": [
            {
                "extraction_label": "Alloy",
                "iri": METALLIC_MATERIAL,
                "upstream_pref_label": "MetallicMaterial",
                "resolution": "broader_alias",
                "rationale": "EMMO 1.0.3 declares no Alloy class. MetallicMaterial is the nearest safe superclass and explicitly notes that metallic materials often form alloys; it also includes pure metals."
            },
            {
                "extraction_label": "Element",
                "iri": CHEMICAL_ELEMENT,
                "upstream_pref_label": "ChemicalElement",
                "resolution": "alias",
                "rationale": "PRISM element nodes are element names or symbols; EMMO defines ChemicalElement as the symbol for a specific chemical element."
            },
            {
                "extraction_label": "Property",
                "iri": PROPERTY,
                "upstream_pref_label": "Property",
                "resolution": "exact_pref_label"
            },
            {
                "extraction_label": "Process",
                "iri": PROCESS,
                "upstream_pref_label": "Process",
                "resolution": "exact_pref_label"
            },
            {
                "extraction_label": "Phase",
                "iri": PHASE_OF_MATTER,
                "upstream_pref_label": "PhaseOfMatter",
                "resolution": "exact_alt_label",
                "rationale": "The upstream class has skos:altLabel Phase."
            },
            {
                "extraction_label": "Paper",
                "iri": DOCUMENT,
                "upstream_pref_label": "Document",
                "resolution": "broader_alias",
                "rationale": "EMMO 1.0.3 declares no Paper class. Document is a broader graphical-document class; no local bibo paper class is declared."
            },
            {
                "extraction_label": "Dataset",
                "iri": DATASET,
                "upstream_pref_label": "Dataset",
                "resolution": "exact_pref_label"
            },
            {
                "extraction_label": "Material",
                "iri": MATERIAL,
                "upstream_pref_label": "Material",
                "resolution": "exact_pref_label"
            }
        ],
        "relation_mappings": [
            {
                "extraction_label": "CONTAINS",
                "iri": HAS_CHEMICAL_SPECIES,
                "upstream_pref_label": "hasChemicalSpecies",
                "rationale": "Direct Substance-to-ChemicalSpecies relation matching PRISM's material-to-element edge. The artifact materialises the OWL-entailed ChemicalElement rdfs:subClassOf ChemicalSpecies conclusion required for range compatibility."
            },
            {
                "extraction_label": "HAS_PROPERTY",
                "iri": HAS_PROPERTY,
                "upstream_pref_label": "hasProperty",
                "rationale": "Direct upstream semiotic relation from an object to a property."
            },
            {
                "extraction_label": "PROCESSED_BY",
                "iri": MANUFACTURED_WITH,
                "upstream_pref_label": "manufacturedWith",
                "rationale": "Upstream relation from a manufactured product to the manufacturing process used to make it. It implies the PRISM source is a manufactured product."
            },
            {
                "extraction_label": "PART_OF",
                "iri": IS_PART_OF,
                "upstream_pref_label": "isPartOf",
                "rationale": "Exact direction and general mereological meaning."
            },
            {
                "extraction_label": "HAS_PHASE",
                "iri": HAS_CONSTITUENT,
                "upstream_pref_label": "hasConstituent",
                "resolution": "broader_alias",
                "rationale": "EMMO 1.0.3 declares no hasPhase property. hasConstituent is a generic physical object-to-spatial-part relation and is valid only when the phase is modeled as a constituent region."
            }
        ],
        "unresolved": [
            {
                "kind": "class",
                "extraction_label": "Author",
                "rationale": "No Author class is declared locally. foaf:Person is referenced and given an RDFS parent but is not locally asserted owl:Class and does not capture the author role."
            },
            {
                "kind": "relation",
                "extraction_label": "OBSERVED_IN",
                "rationale": "The current extraction direction and intended observation bearer are underspecified; no exact local object property is declared."
            },
            {
                "kind": "relation",
                "extraction_label": "PUBLISHED_IN",
                "rationale": "No exact local object property or publication-venue class is declared."
            },
            {
                "kind": "relation",
                "extraction_label": "AUTHORED_BY",
                "rationale": "dcterms:creator is used as metadata but is not locally declared owl:ObjectProperty; no local authorship object property is available."
            },
            {
                "kind": "relation",
                "extraction_label": "CITES",
                "rationale": "Neither bibo:cites nor dcterms:references is declared or used in the local EMMO source."
            }
        ],
        "derivation": {
            "tool": "prism-ontology materialise-emmo",
            "command": "cargo run -p prism-ontology --bin materialise-emmo -- assets/ontology/upstream/emmo-1.0.3 assets/ontology/emmo-1.0.3.materialised.ttl assets/ontology/emmo-1.0.3.provenance.json",
            "semantics": "Complete explicit named rdfs:subClassOf ancestry plus the documented named-member conclusion from ChemicalSpecies' owl:equivalentClass/owl:unionOf axiom. No other OWL restrictions, equivalences, unions, or property chains are inferred.",
            "materialised_conclusions": [
                {
                    "conclusion": format!("<{CHEMICAL_ELEMENT}> rdfs:subClassOf <{CHEMICAL_SPECIES}>"),
                    "basis": format!("<{CHEMICAL_SPECIES}> owl:equivalentClass [ owl:unionOf (... <{CHEMICAL_ELEMENT}> ...) ]"),
                    "purpose": "Makes the CONTAINS/hasChemicalSpecies object range compatible with PRISM Element nodes without a runtime OWL 2 DL reasoner."
                }
            ],
            "source_compatibility_repairs": [
                {
                    "file": "disciplines/units/units.ttl",
                    "line": 22,
                    "change": "Replace the erroneous predicate-list terminator after the otherunits import with a semicolon; line 23 continues the same ontology subject with dcterms:abstract.",
                    "reason": "The pinned upstream byte is a period, making line 23 invalid Turtle; Sophia correctly rejects it. The source scratch file is not modified."
                }
            ]
        }
    });
    let mut output = serde_json::to_string_pretty(&value).expect("JSON value is serializable");
    output.push('\n');
    output
}

/// Verify the human-auditable manifest was rendered from the same mapping
/// contract that selected artifact declarations. The manifest intentionally
/// carries rationale fields that are awkward to encode in the selection map;
/// this check makes any accidental label/IRI drift between the two a hard
/// generation failure.
fn validate_rendered_manifest_mappings(
    manifest: &str,
    selected_classes: &BTreeMap<&str, &str>,
    selected_properties: &BTreeMap<&str, &str>,
) -> Result<(), IoError> {
    let value: serde_json::Value = serde_json::from_str(manifest)
        .map_err(|error| invalid_data(format!("generated manifest is invalid JSON: {error}")))?;
    let classes = manifest_mapping_map(&value, "class_mappings")?;
    let properties = manifest_mapping_map(&value, "relation_mappings")?;
    let expected_classes = selected_classes
        .iter()
        .map(|(label, iri)| ((*label).to_owned(), (*iri).to_owned()))
        .collect::<BTreeMap<_, _>>();
    let expected_properties = selected_properties
        .iter()
        .map(|(label, iri)| ((*label).to_owned(), (*iri).to_owned()))
        .collect::<BTreeMap<_, _>>();
    if classes != expected_classes {
        return Err(invalid_data(format!(
            "manifest class mappings differ from artifact selection: expected {expected_classes:?}, got {classes:?}"
        )));
    }
    if properties != expected_properties {
        return Err(invalid_data(format!(
            "manifest relation mappings differ from artifact selection: expected {expected_properties:?}, got {properties:?}"
        )));
    }
    Ok(())
}

fn manifest_mapping_map(
    manifest: &serde_json::Value,
    field: &str,
) -> Result<BTreeMap<String, String>, IoError> {
    let mappings = manifest
        .get(field)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid_data(format!("generated manifest has no {field} array")))?;
    let mut output = BTreeMap::new();
    for mapping in mappings {
        let label = mapping
            .get("extraction_label")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid_data(format!("{field} entry has no extraction_label")))?;
        let iri = mapping
            .get("iri")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid_data(format!("{field} entry has no iri")))?;
        if output.insert(label.to_owned(), iri.to_owned()).is_some() {
            return Err(invalid_data(format!(
                "generated manifest repeats {field} label {label:?}"
            )));
        }
    }
    Ok(output)
}

/// Return the curated PRISM class aliases and canonical source IRIs.
///
/// @req REQ-OWL-1.1 - Mapping inputs shall be reviewable constants.
fn selected_classes() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("Alloy", METALLIC_MATERIAL),
        ("Dataset", DATASET),
        ("Element", CHEMICAL_ELEMENT),
        ("Material", MATERIAL),
        ("Paper", DOCUMENT),
        ("Phase", PHASE_OF_MATTER),
        ("Process", PROCESS),
        ("Property", PROPERTY),
    ])
}

/// Return the curated PRISM relation aliases and canonical source IRIs.
///
/// @req REQ-OWL-1.1 - Mapping inputs shall be reviewable constants.
fn selected_properties() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("CONTAINS", HAS_CHEMICAL_SPECIES),
        ("HAS_PHASE", HAS_CONSTITUENT),
        ("HAS_PROPERTY", HAS_PROPERTY),
        ("PART_OF", IS_PART_OF),
        ("PROCESSED_BY", MANUFACTURED_WITH),
    ])
}

/// Extract an absolute IRI string from a Sophia term.
///
/// @req REQ-OWL-1.1 - Only actual RDF IRI terms may become canonical identity.
fn term_iri(term: &SimpleTerm<'_>) -> Option<String> {
    term.iri().map(|iri| iri.as_str().to_owned())
}

/// Compute a canonical lowercase SHA-256 string.
///
/// @req REQ-OWL-1.1 - Artifact identity shall use SHA-256.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

/// Construct a deterministic invalid-input diagnostic.
///
/// @req REQ-OWL-1.1 - Invalid derivation inputs shall fail explicitly.
fn invalid_data(message: impl Into<String>) -> IoError {
    IoError::new(ErrorKind::InvalidData, message.into())
}
