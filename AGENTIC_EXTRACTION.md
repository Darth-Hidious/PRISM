# Agentic Paper Extraction and Exact-Source Re-verification

This change implements two separate jobs. Population is a bounded model-driven
reading loop over a paper and an active ontology. Retrieval is an exact-source
re-read of a stored assertion followed by a separate affirmation. They share
the persisted citation format; neither calls the other.

## 1. Population: tool surface

The implementation is in `crates/ingest/src/paper_agent.rs`. The initial
messages contain a short instruction plus document metadata: title, raw line
count, source SHA-256, active ontology id, ontology version IRI, and ontology
artifact SHA-256. They do not contain the paper body, a domain tutorial, a
fact-kind roster, unit advice, or examples.

The model can call exactly eight tools:

| Tool | Input | Returned/recorded evidence | Enforced bound or invariant |
|---|---|---|---|
| `search_ontology` | query | matching class/property labels and canonical IRIs from the active `Ontology` adapter | query 256 characters; 50 results |
| `read_ontology` | canonical or resolvable IRI | class parents/ancestors/descendants and linked declared relations, or property parents/domains/ranges | each neighborhood 100 entries with total/truncated metadata |
| `search_paper` | term | matching raw lines with one-based line numbers | 100 matches; Unicode lowercase substring search |
| `read_paper` | inclusive `from_line`, `to_line` | raw paper text with one-based line numbers | 200 lines per call; bounds checked |
| `propose_fact` | domain-neutral fact shape, optional class/property bindings, inclusive line range | fact, canonical ontology bindings, exact cited text, line range, source SHA-256 | citation must have been returned by a paper-reading tool in an earlier model turn |
| `propose_class` | label, optional proposed IRI/parents/description, line range | an ontology-extension proposal with its exact paper citation | existing IRIs and undeclared parents are refused as retryable tool errors |
| `propose_relation` | label, optional proposed IRI/source/target/description, line range | an object-property extension proposal with its exact paper citation | existing IRIs and undeclared endpoint classes are refused as retryable tool errors |
| `finish` | empty object | explicit normal stop | the rest of the same tool-call batch is processed before stopping |

`propose_class` and `propose_relation` are recorded products. They do not
silently mutate or install an ontology. Their suggested IRIs cannot be
misrepresented as active fact bindings until a governed ontology artifact
actually declares them.

## 2. Population: loop and budget

`run_paper_agent_sample` calls `LlmClient::chat_with_tools_streaming` once per
turn. The default budget is 12 turns and the hard maximum is 64, even if a
caller asks for more. A response without tools is followed by a short nudge to
use a tool or explicitly finish; it is not treated as completion. Invalid tool
arguments and invalid proposals become structured tool errors that the model
can correct on a later turn.

Paper reads in one assistant tool-call batch are siblings. Their results are
not visible to a proposal in that same batch. The loop therefore accepts a
citation only if `search_paper` or `read_paper` returned the complete range in
an earlier turn. This gives the sequence a concrete invariant:

1. search or read the paper;
2. inspect the returned numbered lines;
3. optionally navigate the ontology and re-read either source;
4. propose a cited fact or ontology extension;
5. continue until `finish` or budget exhaustion.

Every turn records every tool name, call id, parsed arguments, success/error
outcome, provider usage, effective budget, and stop reason. Usage is summed
with saturating arithmetic. Budget exhaustion keeps every valid proposal
already recorded; it does not erase the last turn.

`extract_facts_from_chunk_sampled` now gives each independent sample the
complete raw document, not a prompt-sized chunk. The local CLI consequently
runs one whole-document loop. The literature CLI concatenates all selected
body/table/caption blocks into one source workspace and also runs one loop.
Both paths report traces, turns, tool-call totals, and ontology-extension
proposals.

## 3. Active and promoted ontology data

No parallel vocabulary was introduced. The ingest `Ontology` adapter delegates
to the existing `prism_ontology::OntologyGraph` APIs for classes, properties,
labels, hierarchy, version, artifact hash, and prefixes. `PropDecl` now retains
named `rdfs:subPropertyOf`, `rdfs:domain`, and `rdfs:range` declarations, so
`read_ontology` can return declared relation neighborhoods rather than asking
the model to guess them. This is named-RDFS navigation, not arbitrary OWL
restriction inference.

Promotion now installs an accepted artifact at
`.prism/ontologies/<validated-id>.ttl`. A fresh `OntologyRegistry` can load the
configured id from that project catalog through `active_from_project`; paper
population passes that exact adapter into the tools and stamps stored facts
with its version IRI and artifact SHA-256. A custom non-English ontology can
therefore be promoted, selected in configuration, searched, navigated, bound
to stored subject/object classes and a predicate IRI, and used without adding
a Rust vocabulary.

## 4. Hardcoded knowledge removed

The following replaced knowledge was deleted rather than copied into the new
loop:

| Deleted source | What was removed | Replacement |
|---|---|---|
| `text_extract.rs::build_extraction_prompt` and its parser/review path | the whole-paper single-shot JSON call | metadata-only bounded tool loop |
| old extraction prompt | materials-science role instruction | none; interpretation belongs to the model and selected ontology |
| old extraction prompt | Ti-6Al-4V/UTS worked example | none |
| old extraction prompt/schema | closed `measurement|phase|composition|processing|structure|application` kind list | domain-neutral subject/predicate/object/value/unit/conditions shape; legacy `kind` input is ignored for agentic writes |
| old extraction prompt | MPa/GPa/density unit glossary | ontology navigation plus exact opaque `UnitTerm` chosen by the reader |
| old extraction prompt | dimensionless-quantity list | none; Rust does not define quantities |
| old extraction prompt | earlier-chunk `known_entities` block | on-demand ontology and paper search |
| `CONTINUATION_WORDS` | 30 English function words used to rewrite PDF line breaks | raw one-based lines are preserved; only unambiguous mid-token hyphenation remains as a non-citation helper |
| `alias.rs` | second one-shot alias prompt, materials examples, English definition keywords | module deleted; aliases must be explicit ontology/fact decisions |
| `classify.rs` | second one-shot entity classifier and fixed materials prior | prompt path deleted; only the storage data type remains |
| EMMO tabular prompt overrides | bespoke materials instructions, examples, and unit roster | active-ontology-derived generic labels and typed fields |
| deleted `crates/ingest/src/qudt_units.rs` | closed schema-unit enum plus quantity-kind and property-name dispatch | schema accepts any nonempty term selected from the source/ontology; absence is neutral |
| rewritten `crates/provenance/src/units.rs` | MPa/GPa/density aliases, QUDT normalization table, unit connector roster, and quantity-context word checks | two vocabulary-neutral exact-term boundary helpers only |
| `graph_validation.rs`, `local_facts.rs`, and `pipeline.rs` unit gates | English property-name classification and wrong-kind/drop verdicts | structural mapping preserves exact terms and records only explicit malformed shapes |
| `repair.rs`/`repair_worker.rs` deterministic unit repair | Rust vocabulary menus, kind filters, and alias convergence | legacy repair prompt can inspect source text but receives no Rust-authored unit vocabulary |

Unknown nonempty unit terms, including customer ontology IRIs, prefixed names,
and paper spellings, are preserved exactly. An absent/null unit is semantically
neutral: Rust does not infer that the active ontology requires one. An
explicitly blank term is a structural defect; the fresh adapters preserve the
fact with a `unit_unresolved` annotation rather than silently dropping it. The
low-level `UnitTerm` constructor itself refuses blank strings so an invalid term
cannot enter storage disguised as a real identifier. The former `QudtUnit`
name remains only as a source-compatibility alias for this open `UnitTerm`; it
has no QUDT roster or normalization behavior.

Fresh paper-agent proposals do not enter the old materials-specific repair
queue. Invalid proposal shapes are visible in the tool trace and retryable
inside the loop; only shapes the storage model cannot represent are reported
as malformed after the loop.

## 5. Citation persistence

`SourceCitation::new` validates:

- one-based inclusive line bounds;
- a nonempty exact evidence span;
- a lowercase 64-hex SHA-256 of the complete UTF-8 source workspace;
- valid JSON when a source locator is present.

Citation fields are stored per source contribution on
`prov_assertion_evidence`: `source_revision_id`, `evidence_span`, `line_start`,
`line_end`, and `locator_json`. They are intentionally not placed only on the
aggregate `prov_assertion`, because two papers supporting one assertion must
not share or overwrite one another's witness.

The cited write APIs are
`write_fact_with_classification_and_citation`,
`write_classified_fact_with_evidence_and_citation`, and
`write_ontology_bound_fact_with_citation`. Citation upgrades of legacy rows are
atomic: revision, span, both bounds, locator, and source locator move together
only when the old citation core is entirely absent. A later mismatched witness
cannot combine its verification status with an older citation.

Local files and extracted PDFs are stored as SHA-addressed UTF-8 snapshots
under `.prism/source-text/`; the original file path remains the independent
origin identity. Stored literature claims use the same snapshot strategy for
the exact concatenated text workspace and retain the fetched document URL as
origin. Re-verification therefore reopens the representation whose bytes were
actually numbered and hashed, not binary PDF bytes.

The old numeric-tolerance API no longer erases
`SupportRefusal { guard, span }` with `.ok()`: its public contract is now
`Result<String, SupportRefusal>`. Legacy repair acceptance also carries the
successful exact span in its disposition. Fresh population uses the stronger
agent-selected `SourceCitation` directly and does not rerun that lexical gate.

## 6. Retrieval: exact re-read and affirmation

`crates/retrieval/src/reverify.rs` is independent of population. Its flow is:

1. `assertion_by_id` loads the exact stored semantic assertion, including
   conditions, without a default-trust filter.
2. `assertion_evidence_by_id` loads each independent source contribution.
3. `reread_local_source` reopens a local path or `file:` URL; callers with a
   cache/remote source use the pure `reread_from_text` seam.
4. Retrieval hashes the complete UTF-8 text, selects only the stored inclusive
   line range, and verifies that the stored span is still in those exact lines.
   It never searches nearby text or relocates a duplicate quote.
5. Only an unchanged `Ready` context can reach `affirm_reread_context`.
6. The affirmation prompt contains only subject, predicate, object, value,
   unit, conditions, and the exact numbered cited lines. It excludes prior
   confidence, corroboration count, verification status/reason, and all other
   prose that could anchor the answer. Paper/assertion fields are marked as
   untrusted data through the end of the message.
7. The model returns `affirmed`, `denied`, or `uncertain` with a nonempty
   reason. `reverify_local_assertion` returns that structured result; it does
   not silently write a new provenance verdict.

`reverify_local_assertion` is exported as the retrieval crate's public
load-to-affirm library entry point. This patch does not add a CLI command or
persist the returned affirmation automatically.

Deterministic non-ready outcomes distinguish a missing/non-local source, a
legacy/incomplete citation, a changed whole-source revision whose cited lines
remain intact, and changed/incomplete exact cited lines. A changed source is
never passed to affirmation merely because the old quote exists elsewhere.

## 7. Test contract changes

Each renamed or retargeted test contains an in-body `CONTRACT CHANGE` note.
Tests that existed solely for deleted one-shot prompts/parsers were deleted
with those paths.

### Population loop and ontology navigation

- `paper_agent.rs`: `initial_prompt_is_domain_neutral_and_does_not_embed_the_paper`, `tool_surface_is_small_and_has_no_closed_fact_kind_schema`, `custom_non_english_ontology_supports_search_and_navigation`, `ontology_navigation_bounds_large_descendant_sets`, `paper_search_and_read_preserve_raw_one_based_lines`, `multiple_turns_record_proposals_citations_usage_and_same_turn_finish`, `ontology_bindings_survive_a_fact_proposal_as_canonical_iris`, `an_extension_iri_is_not_misrepresented_as_an_active_property_binding`, `a_proposal_cannot_cite_a_sibling_read_call`, `budget_exhaustion_keeps_already_recorded_proposals`, and `invalid_proposal_is_returned_as_a_tool_error_for_retry` replace the single-completion/prompt-body contract with tool-loop, budget, citation, and binding contracts.
- `ontology/src/lib.rs`: `object_property_navigation_retains_named_rdf_declarations` and `undeclared_object_property_parent_is_rejected` pin navigable relation declarations.
- `induction/register.rs`: `promoted_project_artifact_reloads_in_fresh_registries` replaces process-local-only registration.
- `cli/ontology_cmd.rs`: `promotion_persists_for_a_fresh_registry_and_configured_id` pins the project artifact catalog.
- `cli/main.rs`: `text_ingest_loads_a_promoted_non_default_ontology_from_project` replaces the expected non-EMMO refusal; `text_ingest_reads_the_whole_document_in_one_agent_loop` replaces prompt windowing; `text_ingest_reports_a_failed_document_loop_without_partial_facts` replaces late-window partial success; `agentic_population_does_not_enqueue_a_representable_relation` replaces fresh repair-queue routing.

### Extraction/storage adapter

- `text_extract.rs`: `agent_selected_citation_is_preserved_without_a_lexical_rewrite`, `canonical_property_binding_becomes_the_stored_predicate`, `rust_does_not_second_guess_the_agents_selected_citation_by_word_matching`, `a_value_less_agent_proposal_keeps_its_exact_source_lines`, `agentic_extraction_has_no_document_wide_parse_error`, `a_value_less_agent_decision_is_not_replaced_by_an_english_name_gate`, `a_model_selected_span_is_persisted_without_a_numeric_word_gate`, and `facts_the_document_states_survive` replace post-extraction lexical/review verdicts with exact cited proposals and active-ontology predicate identity.
- Unit contracts changed to exact-term preservation in `grounding_does_not_alias_unit_terms`, `an_explicit_ontology_unit_identifier_is_storable_without_alias_tables`, `plain_unit_spellings_are_preserved_without_a_rust_glossary`, `condition_unit_spellings_are_preserved_too`, `an_unfamiliar_unit_term_is_not_a_rust_vocabulary_verdict`, `a_customer_ontology_unit_iri_is_preserved_exactly`, and `a_customer_ontology_condition_unit_is_preserved_exactly`. `fresh_numeric_value_without_unit_is_preserved_without_semantic_annotation` and `fresh_numeric_condition_without_unit_is_preserved_without_semantic_annotation` pin neutral absence; `strict_conversion_refuses_an_explicitly_blank_unit_term` pins the structural boundary.
- Raw-coordinate behavior is pinned by `unwrap_preserves_language_agnostic_line_boundaries` and `an_ambiguous_soft_wrap_is_not_rewritten_by_language_rules`.
- `cli/main.rs`: `text_ingest_preserves_selected_unit_terms_and_annotates_a_blank_one`, `a_valueless_legacy_kind_hint_is_stored_as_a_generic_cited_fact`, `paper_population_does_not_run_a_second_alias_prompt`, and `exact_source_text_snapshot_is_hash_addressed_and_reopenable` replace unit normalization/drop, closed-kind dispatch, alias post-processing, and original-file-only provenance.
- `cli/papers.rs`: `claim_endpoints_stay_generic_without_an_ontology_class_iri`, `customer_unit_terms_are_preserved_exactly`, `absent_unit_terms_are_semantic_and_blank_terms_are_annotated`, `an_uncited_legacy_claim_is_stored_with_a_citation_warning`, and `a_valueless_legacy_kind_hint_is_stored_as_a_generic_edge` pin neutral storage and citation compatibility.
- `provenance/emmo.rs`: `measurement_unit_terms_are_nonempty_and_preserved`, `an_absent_unit_is_not_a_store_level_semantic_verdict`, `a_valueless_measurement_hint_is_stored_as_a_generic_edge`, and `ontology_bound_paper_fact_keeps_partial_class_identity_and_generic_edge` replace QUDT-only, missing-unit, and closed-kind storage behavior.

Deleted single-shot-only tests:

- all ten `alias.rs` tests, with the deleted module;
- all nine prompt/classifier tests formerly in `classify.rs`;
- `parse_extraction_valid_json`, `parse_extraction_fenced_json`, `parse_extraction_garbage_returns_empty`, `unparseable_output_reports_why_it_found_nothing`, `a_document_with_no_facts_is_not_reported_as_an_error`, `literature_extractor_cannot_claim_green`, `prompt_frames_text_as_data_and_carries_all_of_it`, `known_entities_block_reaches_the_prompt_frequency_ordered`, `known_entities_block_is_capped_in_names_and_bytes`, and `a_hallucinated_name_never_enters_the_registry` from `text_extract.rs`.

The fresh-path post-gate tests were also deleted with the post-gate, rather
than left exercising test-only shims:

- `extraction_itself_refuses_facts_the_document_never_stated`, `extraction_drops_a_unit_absent_from_the_document`, `extraction_requires_the_unit_in_the_values_supporting_span`, `extraction_binds_units_to_their_numeric_values`, `extraction_never_borrows_a_unit_from_a_refused_equal_value`, `extraction_requires_a_complete_subject_mention`, and `extraction_rejects_unit_homographs_and_compound_prefixes`;
- `extraction_accepts_equivalent_numeric_formatting`, `extraction_uses_the_callers_numeric_tolerance`, `extraction_keeps_retrievals_numeric_refusal_guards_connected`, `extraction_keeps_an_implicitly_unitless_measurement`, `extraction_drops_an_unsupported_condition`, `extraction_never_joins_lowercase_source_records_for_grounding`, `extraction_drops_an_unsupported_condition_on_an_assertion`, and `extraction_keeps_conditions_grounded_in_the_value_span`;
- `the_document_supplies_the_unit_when_the_model_invents_one`, `a_value_that_changes_between_passes_is_not_believed`, `a_unit_the_page_does_not_print_is_still_refused`, `invented_numbers_are_withdrawn_by_code_with_zero_model_calls`, `a_number_from_another_section_cannot_support_this_chunks_fact`, `a_chunk_local_number_grounds_with_a_document_named_subject`, and `grounding_consults_the_document_not_the_chunk`.

Vocabulary-neutral tabular and compatibility contracts changed as follows;
each replacement test explains the changed meaning inside its body:

- `extraction_schema.rs`: `unit_enum_is_the_shared_qudt_declaration` became `unit_terms_are_nonempty_and_vocabulary_neutral`; `quantitative_types_require_typed_value_and_unit` became `quantitative_types_require_value_but_leave_unit_absence_neutral`; `measured_edges_couple_value_to_unit_and_offer_no_numeric_escape` became `measured_edges_keep_units_optional_and_vocabulary_neutral`.
- `graph_validation.rs`: the four Rust-semantic tests `density_with_a_pressure_unit_is_an_error`, `quantity_kind_check_does_not_over_fire`, `property_named_after_a_measurement_is_an_error`, and `measurement_in_name_does_not_over_fire` were replaced by `graph_validation_does_not_embed_unit_or_property_semantics`.
- `local_facts.rs`: `per_edge_units_of_the_wrong_quantity_kind_are_dropped_with_reason`, `per_edge_values_obey_the_same_unit_rule`, `numeric_value_with_missing_or_unresolvable_unit_is_dropped_with_reason`, and `string_values_resolve_through_the_same_unit_rule` became `per_edge_unit_terms_are_preserved_without_property_word_dispatch`, `per_edge_values_preserve_terms_leave_absence_neutral_and_annotate_blank`, `numeric_values_preserve_exact_terms_and_leave_absence_neutral`, and `string_values_preserve_explicit_or_inline_unit_terms`.
- `pipeline.rs`: `validate_before_graph_write_drops_and_reports_measurement_named_properties`, `tabular_numeric_facts_resolve_units_or_are_dropped_and_reported`, and `a_density_with_a_pressure_unit_is_dropped_and_reported_not_stored` became `validate_before_graph_write_does_not_apply_unit_word_heuristics`, `tabular_numeric_facts_preserve_terms_and_leave_absence_neutral`, and `unit_semantics_are_not_hardcoded_at_production_dispatch`.
- `repair.rs`/`repair_worker`: the deterministic vocabulary tests `repair_vocabulary_is_closed_and_kind_filtered`, `every_declared_unit_resolves_to_its_declared_kind`, `a_pore_number_density_is_not_a_mass_density`, `the_lpbf_refused_quantities_have_matching_property_and_unit_kinds`, `a_count_is_not_a_fraction`, `non_schema_units_resolve_kinds_without_entering_the_schema`, `undeclared_units_get_no_kind_claim`, `property_names_map_to_defensible_kinds_only`, `different_invented_identifiers_converge_on_the_documents_identifier`, `a_span_naming_only_the_subject_cannot_repair_a_different_property`, `an_unknown_property_kind_is_never_silently_accepted`, and `a_resolvable_but_wrong_adjacent_token_is_never_accepted` were deleted with that vocabulary. `an_unresolved_unit_on_an_invented_value_is_withdrawn_not_queued`, `a_queued_unresolved_unit_with_a_resolvable_printed_unit_is_accepted`, `a_known_kind_filters_the_offered_vocabulary`, and `a_minted_unit_is_withdrawn_as_exceeding_the_mandate` became `unresolved_units_are_deferred_without_a_rust_vocabulary`, `a_queued_unresolved_unit_with_a_printed_term_is_accepted`, `unresolved_unit_prompt_has_no_rust_vocabulary`, and `an_unsupported_term_is_withdrawn_by_source_grounding`.
- `provenance/units.rs`: the old table-integrity, alias-resolution, QUDT-pass-through, canonicalization, and unknown-spelling tests were deleted with the table. `unit_term_accepts_exact_nonempty_terms_without_a_vocabulary` and `exact_source_matching_has_no_alias_table` now pin the only two remaining structural behaviors.

### Citation persistence and retrieval

- `provenance/emmo.rs`: `source_citation_rejects_coordinates_hashes_and_locators_it_cannot_address`, `distinct_sources_keep_distinct_citations_and_duplicates_do_not_overwrite`, `uncited_evidence_is_explicit_none_and_a_reread_fills_missing_citation`, `a_complete_citation_atomically_upgrades_a_legacy_source_locator`, `a_partial_citation_is_never_hybridized_with_a_later_citation`, `a_mismatched_later_citation_cannot_upgrade_verification`, `pre_citation_evidence_schema_migrates_existing_rows_to_explicit_none`, and `assertion_by_id_loads_the_complete_conditioned_fact` pin schema migration, exact selection, and witness atomicity.
- Existing annotate-not-refuse storage semantics are pinned by `weak_facts_are_stored_findable_and_excluded_from_the_default_read`, `verification_upgrades_on_a_grounding_witness_and_never_downgrades`, `an_absent_unit_is_not_a_store_level_semantic_verdict`, `the_verification_status_rides_the_graph_edge_props`, and `verification_rank_trust_and_ratchet_are_consistent`.
- `retrieval/claims.rs`: numeric-tolerant quote tests now assert a lossless `Result`, including every named refusal guard and examined span.
- `retrieval/reverify.rs`: `exact_cited_lines_and_revision_are_ready_for_affirmation`, `changed_revision_is_distinct_when_the_cited_lines_are_intact`, `changed_exact_cited_lines_are_reported_before_revision_change`, `missing_local_source_is_explicitly_unavailable`, `duplicate_span_elsewhere_is_never_used_as_a_relocation`, `conditioned_fact_is_loaded_by_its_exact_assertion_id`, `affirmation_prompt_contains_only_semantic_identity_and_exact_numbered_lines`, `affirmation_revalidates_revision_lines_and_span_before_a_model_call`, `affirmation_parser_accepts_each_structured_verdict`, and `affirmation_parser_rejects_an_unstructured_or_unexplained_answer` cover the complete re-read seam.

## 8. Gate output

The repository-required commands were run with:

```text
CARGO_TARGET_DIR=/Users/siddharthakovid/Downloads/prism-unmuzzle/target
```

### Required format gate

```text
$ CARGO_TARGET_DIR=/Users/siddharthakovid/Downloads/prism-unmuzzle/target \
  cargo fmt -p prism-ingest -p prism-provenance -p prism-ontology
(no output; exit 0)
```

### Required test gate

```text
$ CARGO_TARGET_DIR=/Users/siddharthakovid/Downloads/prism-unmuzzle/target \
  cargo test -p prism-ingest -p prism-provenance -p prism-ontology

running 339 tests
...
thread 'pipeline::tests::unit_semantics_are_not_hardcoded_at_production_dispatch'
panicked at wiremock-0.6.5/src/mock_server/builder.rs:107:46:
Failed to bind an OS port for a mock server.: Os { code: 1,
kind: PermissionDenied, message: "Operation not permitted" }
...
test result: FAILED. 292 passed; 45 failed; 2 ignored; 0 measured;
0 filtered out; finished in 1.33s

error: test failed, to rerun pass `-p prism-ingest --lib`
(exit 101)
```

All 45 failures in that run failed at the same Wiremock local-listener bind,
before their test bodies. They span existing vision, tabular pipeline, legacy
repair, and socket-backed paper-extraction fixtures. An escalation to permit
loopback-only test servers was requested and rejected because this repository's
agent policy forbids network calls; no bypass was attempted. Cargo stops after
the failing ingest test binary, so the exact combined command did not execute
the provenance and ontology binaries. The same final sources compile all three
test binaries without running listeners:

```text
$ CARGO_TARGET_DIR=/Users/siddharthakovid/Downloads/prism-unmuzzle/target \
  cargo test -p prism-ingest -p prism-provenance -p prism-ontology --no-run
Finished `test` profile [unoptimized + debuginfo] target(s) in 0.21s
Executable unittests src/lib.rs (.../prism_ingest-...)
Executable tests/ontology_induction.rs (.../ontology_induction-...)
Executable unittests src/lib.rs (.../prism_ontology-...)
Executable unittests src/bin/materialise-emmo.rs (.../materialise_emmo-...)
Executable unittests src/lib.rs (.../prism_provenance-...)
(exit 0)
```

The two network-free package suites that the combined command could not reach
were run independently:

```text
$ cargo test -p prism-ontology
test result: ok. 14 passed; 0 failed; 0 ignored; finished in 0.04s

$ cargo test -p prism-provenance
test result: ok. 127 passed; 0 failed; 1 ignored; finished in 5.52s
```

The provenance run initially exposed a pre-existing numeric boundary leak:
the documented cosine similarity range returned `-1.0001636`. The conversion
now clamps the approximate database result to its public `[-1, 1]` contract;
the existing scale test then passed without weakening its assertion.

### Required clippy gate

```text
$ CARGO_TARGET_DIR=/Users/siddharthakovid/Downloads/prism-unmuzzle/target \
  cargo clippy -p prism-ingest -p prism-provenance --all-targets -- -D warnings
Checking prism-ingest v1.0.0 (.../crates/ingest)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.44s
(exit 0)
```

### Additional retrieval/CLI and language gates

```text
$ cargo test -p prism-ingest paper_agent --lib
test result: ok. 11 passed; 0 failed; 328 filtered out

$ cargo test -p prism-ingest canonical_property_binding_becomes_the_stored_predicate --lib
test result: ok. 1 passed; 0 failed; 338 filtered out

$ cargo test -p prism-ingest local_facts --lib
test result: ok. 15 passed; 0 failed; 322 filtered out

$ cargo test -p prism-ingest extraction_schema::tests:: --lib -- \
    --skip pipeline::tests::extraction_schema_follows_the_active_ontology_not_a_hardcoded_list
test result: ok. 7 passed; 0 failed; 330 filtered out

$ cargo test -p prism-ingest grounding_does_not_alias_unit_terms --lib
test result: ok. 1 passed; 0 failed; 336 filtered out

$ cargo test -p prism-retrieval --lib
test result: ok. 127 passed; 0 failed; 0 ignored; finished in 1.12s

$ cargo test -p prism-cli papers::store_tests
test result: ok. 7 passed; 0 failed; 216 filtered out

$ cargo test -p prism-cli promotion_persists_for_a_fresh_registry_and_configured_id
test result: ok. 1 passed; 0 failed; 222 filtered out

$ cargo test -p prism-cli exact_source_text_snapshot_is_hash_addressed_and_reopenable
test result: ok. 1 passed; 0 failed; 222 filtered out

$ cargo check -p prism-cli --all-targets
Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.83s

$ cargo clippy -p prism-cli -p prism-retrieval --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.95s

$ python3 scripts/check_no_cjk_in_agent_artifacts.py
OK: no CJK language drift detected in agent artifacts

$ git diff --check
(no output; exit 0)
```
