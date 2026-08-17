# PLUGIN AUDIT — PASS 1: What PRISM actually has today, from the inside

Date: 2026-08-16. Branch: `feat/annotate-not-refuse`. Method: code reading of
this workspace plus two empirical runs on this machine (targeted `cargo test`
of the ontology plane; a standalone German-pharma-ontology probe compiled
against `crates/ingest`'s public API). DeepSeek's harness was read only enough
to name the yardstick; Pass 2 owns that side. Every claim below carries the
file:line that decides it. Nothing in this repo was modified except this file.

---

## 1. The verdict in one paragraph

The ontology vocabulary plane is a real, working, runtime plugin point — the
only one in the Rust workspace that needs zero recompilation — and the
empirical section below proves a second, non-EMMO ontology loads from a
project artifact and governs the extraction prompt, the constrained-decoding
schema, validation, and storage isolation today. But "the ontology governs
extraction" is true only for the *vocabulary* half of extraction. The
*measurement* half — typed values, units, measurement facts, sign checks, the
typed store shapes — is EMMO's private property, reached through hardcoded
string literals and a store enum no artifact can extend. A German pharma
ontology promotes cleanly and extracts real facts with zero Rust edits; its
numeric measurements arrive as untyped generic edges, its text-path facts land
in the wrong tenant, and the one doc comment that promises it sign-domain
support with "zero Rust edits" describes machinery that does not exist. The
rest of the plugin story splits cleanly: four Rust trait registries that
require recompilation (honestly documented as such), five genuine
runtime/data-driven extension points (MCP config, skills, workflows, Rego
policies, Python plugins), and two traits that only look like plugin points —
one of which the codebase itself admits is dead.

---

## 2. Empirical: a second ontology CAN govern extraction today (with measured limits)

### 2.1 What was run

**Run 1 — the repo's own plugin-plane tests** (27 tests, all pass, 0.16s,
this machine, toolchain 1.95.0):

```
cargo test -p prism-ingest --lib -- ontologies::tests induction::register::tests \
  pipeline::tests::extraction_schema_follows_the_active_ontology_not_a_hardcoded_list \
  pipeline::tests::two_ontologies_coexist_in_one_store_without_blending
→ test result: ok. 27 passed; 0 failed
```

The decisive ones, and what each proves:

- `induction::register::tests::promoted_artifact_registers_into_the_production_registry`
  (`crates/ingest/src/induction/register.rs:336`): a draft TTL artifact →
  `promote_artifact` → registers into the SAME process-wide registry
  `ontologies::active` resolves from, with real minted IRIs, honest
  version-IRI/SHA-256, working subsumption, and a derived extraction prompt
  carrying its vocabulary.
- `promoted_project_artifact_reloads_in_fresh_registries` (register.rs:427):
  promotion is durable state — a *later process* resolves the id from
  `.prism/ontologies/<id>.ttl` alone.
- `pipeline::tests::extraction_schema_follows_the_active_ontology_not_a_hardcoded_list`
  (`crates/ingest/src/pipeline.rs:3259`): the wire request recorded by a mock
  LLM server carries the chem ontology's enums (`Molecule`, `REACTS_WITH`)
  and none of EMMO's (`Alloy`, `CONTAINS`) — constrained decoding follows the
  active ontology, not a frozen list.
- `pipeline::tests::two_ontologies_coexist_in_one_store_without_blending`
  (pipeline.rs:4205): EMMO under tenant `local`, the second ontology under
  `local@chem-coexist`, disjoint on every read, both discoverable.

**Run 2 — the German pharma probe** (standalone crate against
`crates/ingest`'s public API, i.e. exactly what a customer-facing embedder or
the CLI itself calls — no test-only backdoors):

An `InducedOntology` with German classes (`Wirkstoff`, `Löslichkeit`,
`Darreichungsform`) and a German relation (`hat Löslichkeit`) was written as
a TTL artifact, promoted, loaded via `load_induced_from_path`, and registered
into a fresh `OntologyRegistry::builtin()`. Results: see §2.3 below.

### 2.2 How a customer does it (the zero-Rust-edit path, end to end)

1. `prism ontology induce --corpus <dir> --domain pharma-de` — LLM induces a
   draft TTL (`crates/cli/src/ontology_cmd.rs:23`), or hand-author the TTL in
   the artifact format (`crates/ingest/src/induction/ttl.rs`).
2. `prism ontology promote <artifact.ttl>` — flips `prism:status` to
   accepted AND installs it at `.prism/ontologies/pharma-de.ttl`
   (`ontology_cmd.rs:105-133`, `install_promoted_artifact` at 143).
3. `[ontology] id = "pharma-de"` in `prism.toml`
   (`crates/core/src/config.rs:92`).
4. Every local ingest path resolves it: `active_ontology_from_config`
   (`crates/cli/src/main.rs:7015-7027`) calls
   `ontologies::active_from_project`, which lazily registers the project
   artifact (`crates/ingest/src/ontologies.rs:1085-1118`). Tabular:
   main.rs:7047 → `PipelineConfig.ontology` → pipeline.rs:260. Text/papers:
   main.rs:7400-7401, 8044; `crates/cli/src/papers.rs:328-329`. A missing or
   wrong id is a loud refusal naming what IS registered (ontologies.rs:1065-1076)
   — never a silent EMMO fallback.

What the customer supplies: **one TTL file + one config line.** No recompile.
This is genuinely the strongest surface in the codebase, and the registry
discipline around it (register refuses taken ids, replace refuses free ids,
draft artifacts refused until promoted, malformed declarations refused with
the specific violation — ontologies.rs:913-957, register.rs:213-235) is
better engineering than the "plugin system" label usually gets.

### 2.3 What the probe measured — the limits, exactly

Probe source: `/tmp/pharma-de-probe` (rerunnable:
`CARGO_TARGET_DIR=<repo>/target cargo run`). Output, verbatim highlights,
exit 0:

**What works, measured:**

- The artifact with umlaut labels promotes, loads, and registers beside EMMO
  in one registry. IRI minting accepts non-ASCII local names (RFC 3987 via
  `sophia`): `https://prism.marc27.com/ontology/pharma-de#Löslichkeit`.
- The derived extraction prompt instructs exactly the German vocabulary:
  `Every entity "type" MUST be one of: Darreichungsform, Löslichkeit,
  Wirkstoff.` / `Every relationship "rel" MUST be one of: HAT_LÖSLICHKEIT.`
  — including the uppercased umlaut in the minted relation token.
- The constrained-decoding JSON schema's enum is
  `["Darreichungsform","Löslichkeit","Wirkstoff"]` with zero EMMO leakage
  (asserted: no `Alloy` anywhere in the schema).
- `storage_label("Wirkstoff") = Some("Wirkstoff")` (identity, total —
  enforced at registration) and the storage tenant composes to
  `local@pharma-de`, disjoint from EMMO's `local`.

**What the same probe measured as absent** (the promoted ontology's answers,
not mine):

```
quantitative_labels: []
measurement_relations: []
quantity_sign_domain("Dissoziationskonstante"): None
```

So the promoted German ontology governs vocabulary, prompt, schema,
validation and tenancy — and carries **no** typed-measurement declaration, no
measured-edge schema variants, and no sign domains, because the artifact
format cannot express them and the induced adapter never overrides those
trait methods (§4.2, §4.3). Its numeric values ride as free-form entity
properties and untyped generic edges.

### 2.4 Owner's test, applied literally, per surface

"Could a customer promote a German-language pharma ontology and have THIS
surface behave correctly with zero Rust edits?"

| Surface | Verdict | Decided by |
|---|---|---|
| Tabular extraction prompt | **YES** | derived from declaration, ontologies.rs:341-380; pipeline.rs:260 |
| Constrained-decoding schema | **YES** | extraction_schema.rs:94,132 reads the active ontology; proven on the wire (pipeline.rs:3259 test) |
| Graph validation (type membership) | **YES** | graph_validation reads the SAME declaration (graph_validation.rs:13-18) |
| Storage labels / entity keys | **YES** | ontology-declared, enforced total at registration (ontologies.rs:829-836; pipeline.rs:664) |
| Tenant isolation (tabular) | **YES** | `storage_tenant` composed per ontology, pipeline.rs:309; proven by coexist test |
| Tenant isolation (text/papers) | **NO** | `tenant: "local"` hardcoded, main.rs:7556 and 8110 — pharma text facts share EMMO's keyspace |
| Paper agent (tool-loop reader) | **YES** for vocabulary | workspace is `&dyn Ontology` (paper_agent.rs:728-741); system prompt is affordances-only, no domain prose (paper_agent.rs:500-530) |
| Typed measurement extraction (value+unit variants) | **NO** | `quantitative_labels`/`measurement_relations` default empty (ontologies.rs:288, 323); the induced adapter never overrides them (register.rs:49-95); the artifact format has no slot (induction/mod.rs:102-131) |
| Measurement FACTS in the store | **NO** | local_facts.rs:138-212 string-matches `HAS_PROPERTY`/`HAS_PHASE`/`PROCESSED_BY`/`CONTAINS`; everything else → generic edge, value dropped |
| Quantity sign checks | **NO** | triple-dead; see §4.2 |
| Domain checks (`validate_domain`) | **silent-YES** | promoted ontology gets none (trait default, ontologies.rs:384); EMMO's checks fire only on EMMO relation names so they cannot misfire on pharma — silence, not wrongness |
| Prompt language | **partial** | German *labels* flow everywhere; the prompt *scaffold* ("You are a data analyst…", the rules) is English Rust strings (ontologies.rs:328-380), overridable only by a Rust `Ontology` impl, not by the artifact |
| Store typed shapes | **NO by admitted design** | 7 hardcoded kinds, provenance/src/emmo.rs:40-43, 3246-3366; admitted non-pluggable in ontologies.rs:23-29 |
| Reads/query | **YES** | `default_read_tenants` discovers `local@pharma-de` (proven in coexist test, pipeline.rs:4283-4289) |

Net: the pharma customer gets a working extract-validate-store-query loop in
their own vocabulary with zero Rust edits. What they silently lose relative
to the EMMO experience: typed numeric measurements, unit-carrying facts,
measurement-grounding guards, and (on the text path) tenant isolation. Nobody
tells them; every surface degrades without an error.

---

## 3. Inventory: every extension point, what you supply, and whether you recompile

### 3.1 Genuine, runtime, zero-recompile (data/config-driven)

| # | Extension point | You supply | Decided by |
|---|---|---|---|
| 1 | **Ontology vocabulary** | a promoted TTL artifact at `.prism/ontologies/<id>.ttl` + `[ontology] id` | §2.2; the flagship |
| 2 | **MCP tool servers (client side)** | an entry in `~/.prism/mcp.json` + any stdio MCP server binary | crates/agent/src/mcp.rs:2-25; tools land namespaced `mcp__<server>__<tool>`, admission via `ToolCatalog::extend_untrusted` (tool_catalog.rs:278), always approval-gated. stdio transport only; other transports skipped with a warning |
| 3 | **Skills** | `~/.prism/skills/<name>.md` (human procedures, frontmatter policy) or agent-authored verified JSON snippets | crates/agent/src/skills.rs:1-18; implicit invocation off by default (skills.rs:66-76) |
| 4 | **Workflows** | YAML in `.prism/workflows/` or `~/.prism/workflows/` | crates/workflows/src/lib.rs:244-256. Caveat: the step-action vocabulary is a closed Rust set (`KNOWN_ACTIONS`, lib.rs:2241: set/message/http/tool/llm/provenance/if/loop/parallel/workflow) — you compose actions, you cannot add one. The generic `tool` action does reach the open tool plane |
| 5 | **Rego policies** | `.rego` files in `~/.prism/policies/` or `.prism/policies/` (later overrides earlier, built-ins embedded) | crates/policy/src/lib.rs:19-23 |
| 6 | **Python plugins** | pip entry point in group `prism.plugins`, or `~/.prism/plugins/*.py` exposing `register(registry)` | app/plugins/loader.py:14-64, invoked at bootstrap (app/plugins/bootstrap.py:358-373). The facade hands 5 sub-registries: tools, skills, collectors, ML algorithms, search providers (app/plugins/registry.py:11-19) |
| 7 | **Search providers** (OPTIMADE, MP, …) | a Python `ProviderFactory` via `register_provider_factory` (through a plugin) | app/tools/search_engine/providers/registry.py:22,33; built-ins are swappable by id |
| 8 | **LLM endpoint + model** | a base URL + model name: `prism use`, `~/.prism/providers.toml` (overrides the shipped one, crates/core/chat_config.rs:518), `~/.prism/models.toml` for model configs augmenting the compiled seed (crates/agent/src/models.rs:6, 100, 311) | OpenAI-compatible HTTP only, plus embedded GGUF via the `gguf://local` sentinel (crates/llm/src/lib.rs:454-467). A non-OpenAI wire protocol = Rust work |
| 9 | **Embedding backend** | config choice `native` (ONNX from `~/.prism/models/embed/`) or `openai` (any compatible endpoint) | crates/embed/src/lib.rs:26, 64-71, 90 |
| 10 | **Column-mapping rules** (tabular) | a YAML `--mapping` file (regex column patterns → entity types/relations/aliases) | crates/ingest/src/mapping.rs:1-35. Note: the *example* in the doc is materials vocabulary, but the mechanism is domain-free — patterns and type names are the user's |
| 11 | **PRISM as a plugin for others** | nothing — `prism mcp-server-native` (Rust tools) and `python -m app.mcp_server` (Python tools) serve MCP to any host | crates/cli/src/mcp_server_native.rs:1-13 |

Caveat on #6: `discover_entry_point_plugins`/`discover_local_plugins` wrap
every plugin load in `except Exception: pass` (loader.py:31-34, 60-63). A
broken customer plugin vanishes silently — the exact "silent drop" this
codebase spends thousands of lines refusing everywhere else.

### 3.2 Genuine, but compile-time (Rust trait + registration call; third party must build)

These four share one deliberately uniform two-call contract (`register`
refuses a taken id, `replace` refuses a free one, declarations captured
outside the lock) — it is a real internal plugin architecture, consistently
executed. But there is **no dynamic code loading in the Rust workspace** (no
libloading/dlopen, no WASM runtime — verified by grep over `crates/`), and
none of these four registries has a subprocess or data-artifact route the way
#1 (TTL) and #2 (MCP) do. So "plugin" here means: an embedder or fork writes
a Rust impl and recompiles.
A customer cannot use these; a partner integrating PRISM as a library can.

| # | Registry | Built-ins | Decided by |
|---|---|---|---|
| 12 | `Ontology` trait impls (beyond the artifact path) | `EmmoOntology`, `MatKgOntology` | ontologies.rs:126-387, 900-907. The artifact path (#1) is the runtime door; a Rust impl is how you'd get prompt overrides, `validate_domain`, `quantitative_labels` etc. |
| 13 | `Connector` (file formats) | CSV, Parquet | connectors/connector.rs:42-58, 133-140; extension claims exclusive; pipeline has no extension `match` (verified: pipeline tests register/replace connectors through the registry, pipeline.rs:2099-2205) |
| 14 | `DocumentUnderstanding` (bytes→text) | text-layer, vision | document/understanding.rs:1-26; claims deliberately NON-exclusive (text layer and VLM both read `pdf`), selection is policy. The vision *endpoint/model* is config; a new *kind* of reader is Rust |
| 15 | Agent in-process hooks | provenance recorder etc. | crates/agent/src/hooks.rs:1-40 — pre/post tool callbacks, per-session, code-level only; no config-file surface |

### 3.3 Closed surfaces (not extension points, stated for completeness)

- **Rust command tools**: one static `const COMMAND_TOOLS` array (~90 specs),
  command_tools.rs:255 — all self-invocations of the `prism` binary with
  per-flag policies. New tools reach the agent via MCP (#2), the Python plane
  (#6), or skills (#3), not here.
- **Paper-agent tool loop**: fixed 8-tool surface in `paper_tools()`
  (paper_agent.rs:560-715) with a match dispatch (1143, 1189). Closed on
  purpose (bounded reader); the *ontology behind* the tools is the plugin.
- **Slash commands**: static `BUILTIN_COMMANDS` (crates/agent/src/commands.rs).
- **Store fact kinds**: 7 literals (`measurement|phase|composition|contains|
  processing|structure|application`), provenance/src/emmo.rs:40-43.
- **Workflow actions**: closed set, see #4.
- **UI (TUI/GPUI/VS Code)**: not pluggable in any form.

---

## 4. The interfaces that only LOOK pluggable — name and shame

These are the most misleading things in the codebase, because they read as
extensible and are not.

### 4.1 `OntologyConstructor` — a hope, not a plug point (admitted)

`crates/ingest/src/lib.rs:66-77` declares the trait ("Pluggable ontology
construction — ships with LLM impl, DMMS slots in later"); it has exactly one
impl (`LlmOntologyConstructor`, ontology.rs:326) and the pipeline does not
even call it through the trait — it constructs the concrete type and calls an
*inherent* method (pipeline.rs:324, 395). To the repo's credit, lib.rs:18-23
says this out loud: "nothing consumes it yet… Implementing the trait alone
will not put a new engine on the ingest path." Verdict: honest dead interface.
Either wire a `dyn` holder or delete the trait; today it is documentation
debt wearing a trait's clothes.

### 4.2 `Ontology::quantity_sign_domain` — the doc promises what the code cannot do

ontologies.rs:299-306 (doc comment): *"This is where the sign constraint
lives, not in Rust… A promoted German-language pharma ontology supplies the
sign of a dissociation constant here with zero Rust edits."*

That sentence is false today, three independent ways:

1. **The artifact has no slot.** `InducedClass`/`InducedRelation`
   (induction/mod.rs:102-131) carry label/definition/parent/domain/range and
   nothing else; the TTL writer/parser (induction/ttl.rs) round-trips exactly
   that. No sign annotation can be expressed.
2. **The adapter never answers.** `InducedVocabulary` (register.rs:49-95)
   does not override `quantity_sign_domain`, so a promoted ontology inherits
   the `None` default (ontologies.rs:304-306).
3. **The consumer asks the wrong ontology anyway.**
   `quantity_sign_for_fact` (text_extract.rs:905-913) resolves
   `ontologies::active(None)` — the *default id* — not the run's selected
   ontology, even though every surrounding extraction function carries an
   explicit `&dyn Ontology`. A Rust-supplied pharma adapter registered under
   its own id would *still* not be consulted unless it `replace`d "emmo".

Since bundled EMMO also declares nothing (proven by
`a_builtin_ontology_that_declares_no_sign_domain_says_so`, ontologies.rs:1248),
the sign-guard machinery in `prism_retrieval::claims` currently runs with the
check permanently inert **for everyone**. The de-hardcoding was real (no
compiled materials table survives — good); the promised replacement input
channel was never built.

### 4.3 `measurement_relations()` / `quantitative_labels()` — declared, then ignored where it counts

The trait methods exist, EMMO derives them from its artifact
(ontologies.rs:548-573), and the extraction schema honours them
(extraction_schema.rs:94,132) — so the *model* can be instructed and
constrained per declaration. But the fact mapper that decides what becomes a
`measurement` in the store ignores the declaration entirely and string-matches
EMMO's literals: `match rel.rel_type.as_str() { "HAS_PROPERTY" => …,
"HAS_PHASE" => …, "PROCESSED_BY" => …, "CONTAINS" => …, _ => generic }`
(local_facts.rs:138-212). Consequence, stated concretely:

- A pharma ontology with relation label "has property" (English) mints the
  token `HAS_PROPERTY` (register.rs test, line 380) and **accidentally**
  inherits measurement mapping by lexical collision.
- The same ontology in German (`hat Löslichkeit` → `HAT_LÖSLICHKEIT`) falls
  to `_` and every numeric value on those edges is silently not a
  measurement. Same declaration, same semantics, different language, different
  storage — the exact opposite of "domain knowledge never in Rust."

The fix direction is already half-built: the mapper should ask the active
ontology `measurement_relations().contains(rel)` instead of matching bytes.

### 4.4 Smaller instances

- **Text-path tenancy**: `tenant: "local".into()` with the comment "no
  per-run tenancy (yet)" (main.rs:7556, repair pass at 8110) — while
  ontologies.rs:389-408 presents tenant composition as the isolation
  contract. True on the tabular path only (pipeline.rs:309). A pharma paper
  ingest writes `Wirkstoff` nodes into the same keyspace as EMMO's `Matter`.
- **MatKG's "refused honestly" claim**: ontologies.rs:685-686 says selecting
  MatKG for text ingest "is refused honestly by that path." No such refusal
  exists — grep finds no matkg special-casing on any ingest path; `[ontology]
  id = "matkg"` would simply run text extraction under MatKG's 7 classes +
  `COOCCURS_WITH`. Stale doc claim.
- **Stale comment in ontology.rs:504-507**: promises the model "is
  structurally unable to emit… a non-QUDT unit" — the schema's unit is an
  open non-empty string by deliberate design (pipeline.rs:3236-3245 test
  pins `enum` absent). The code is right; the comment describes the old world.

---

## 5. Hardcoded-but-should-be-pluggable, ranked by damage to "new domain, zero Rust edits"

1. **The measurement pipeline behind the vocabulary** (§4.3 + §4.2 + the
   artifact format's missing slots). This is the #1 gap because it makes the
   flagship claim quietly half-true: the demo works, facts flow, and the
   customer only discovers months later that none of their numbers are typed,
   unit-carried, or sign-checked. Concretely three pieces, smallest first:
   (a) local_facts consults `measurement_relations()` instead of literals;
   (b) `quantity_sign_for_fact` takes the run's ontology instead of
   `active(None)`; (c) the induced-TTL format gains optional
   `prism:quantitative`, `prism:measurementRelation`, `prism:signDomain`
   annotations and `InducedVocabulary` serves them. None of these add domain
   vocabulary to Rust; all three make existing trait methods real.
2. **Text-path tenant composition** (§4.4). One-line shape
   (`storage_tenant(LOCAL_TENANT, ontology.id())` at main.rs:7556/8110), but
   it guards the isolation story PRISM sells; today two ontologies blend on
   the path customers will actually use most (papers).
3. **Store fact kinds** (provenance/src/emmo.rs:3246-3366). Admitted
   non-pluggable (ontologies.rs:23-29) and defensible short-term — the
   generic-edge fallback plus `write_ontology_bound_fact_with_citation`
   (emmo.rs:2899-2928, which deliberately forces the generic shape with
   classified nodes) means nothing is *lost*. But "typed rows for EMMO,
   generic edges for everyone else" is a two-tier product the moment a second
   ontology matters commercially.
4. **Prompt scaffold language** (ontologies.rs:328-380). The extraction
   preamble/instructions are English Rust strings; the artifact cannot
   localise or restyle them. Low urgency (models cope with mixed-language
   prompts) but it is the last Rust-owned prose between a customer and a
   fully artifact-defined extraction surface. An optional
   `prism:extractionPreamble` in the artifact would close it.
5. **Workflow action set** (workflows/lib.rs:2241). Closed enum of 10; the
   `tool` action escapes to the open tool plane, so this mostly bites when a
   step needs new *control flow*, which is rare. Leave it until it hurts.
6. **Python plugin failure silence** (loader.py:31-34, 60-63). Not a
   hardcoding, but it turns the healthiest runtime plugin door into a
   debugging trap. Log loudly per failed plugin; it is a two-line change and
   matches the house style everywhere else.

Not on the list on purpose: connectors, document understanding, embed/LLM
backends. Their compile-time nature is fine — file formats and OCR engines
are integrator concerns, not customer-domain concerns, and each is honestly
documented (connector.rs:14-15 even names the compilation-unit fact).

---

## 6. The DeepSeek yardstick, briefly (Pass 2 owns the full comparison)

Read: package READMEs plus the `extensions/` (cordis) package layout — noted
per the brief's rule that README claims are intent, not verified behavior.
What their architecture *states* that PRISM measurably lacks:

- **One registration substrate.** Every subsystem registers under a context
  key on a shared runtime ("cordis"): `ctx.tools`, `ctx.sessions`,
  `ctx.agents`, `ctx.systemPrompt` — and the *agent loop itself* is a default
  implementation behind a seam (`core/agent` contract vs `core/agent-loop`
  impl). PRISM's equivalents are five hand-rolled Rust registries with an
  admirably uniform two-call contract, but the loop, prompt assembly, and
  session store are concrete code with no seam. If PRISM ever wants "loop is
  the product, models swappable" to be structural rather than aspirational,
  the seam-around-the-loop is the design to steal — the TS code itself is
  worthless to a Rust workspace.
- **Runtime-loadable plugins, sandboxed.** Their `extensions/` runs
  dynamically defined packages in a `node:vm` sandbox, including
  model-authored ones. PRISM's only runtime doors are data files (TTL, YAML,
  Rego, Markdown) and out-of-process code (MCP servers, Python plugins).
  Notably, PRISM's *shape* here is defensible — out-of-process extension is a
  sounder sandbox than `node:vm` — but PRISM should say so on purpose instead
  of by accident.
- What PRISM has that their factoring does not obviously cover: the
  ontology-artifact plane itself (a data-driven *domain* plugin, not a code
  plugin) — there is no `ontology` among their 47 packages. That is PRISM's
  actual differentiator; nothing in the DeepSeek tree replaces it.

What NOT to import, from this pass's vantage: their 47-package factoring.
PRISM is one Rust workspace with ~25 crates and a Python tool plane; the
missing piece is three trait-method wirings and one tenant line (§5), not a
package explosion. Copying the cordis substrate wholesale would replace a
working, loud-refusal registry discipline with a port of someone else's
runtime for zero customer-visible gain.

---

## 7. What this pass did not determine

- Whether `prism ontology induce` produces *good* ontologies from a real
  German pharma corpus (only that the artifact contract round-trips and
  registers; induction quality is an LLM question, not a plumbing one).
- The cloud/platform ingest path (`/knowledge/ingest-job`, main.rs:6990) —
  whether the hosted side honours `[ontology] id` was not traced; the audit
  above is the local spine.
- Whether the paper agent's grounding guards behave well on German sentence
  structure end-to-end (the span logic is punctuation/number-based and
  language-neutral by construction — text_extract.rs:1324 — but this was not
  exercised with a live model).
