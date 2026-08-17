# PRISM — A Map of the System

This document explains what PRISM is, what each piece is for, and how a
document becomes stored, checkable knowledge. It is written for someone who
does not read Rust. Where a component's purpose could not be determined from
the code, this document says so and names the file.

---

## 1. The one-sentence version

PRISM reads scientific documents (papers and tables), uses a language model to
pull out factual claims ("this alloy's tensile strength is 950 MPa"), checks
each claim against the words it was read from, and stores the survivors in a
local database together with a receipt that says exactly where each fact came
from. A separately-authored **ontology** — a controlled vocabulary of concepts
and relations — decides what counts as a concept, what relations are allowed,
and how things are named in storage. PRISM itself holds **no list of materials
words, units, or properties**; all of that knowledge lives in the ontology, so
the same code serves any domain (metals, pharma, semiconductors) in any
language.

---

## 2. The pieces, in the order data flows through them

Each crate (a Rust library) has one job. Listed in the order a document meets
them, not alphabetically.

### `retrieval` — get the document and find claims in it
Fetches a paper (from an open repository or a PDF you supply) and turns it
into clean text with stable line numbers (`fulltext.rs`). It also contains the
**claims engine** (`claims.rs`): given a claim like "value 950, unit MPa,
subject Alloy X", it searches the exact source text for a sentence that really
says that. This file is the most heavily defended code in the system — it has
dozens of "guards" that refuse false matches (a number inside a citation like
`[1140]`, the `718` inside the name `Inconel 718`, one end of a range like
`950–1100`). It deliberately contains no domain vocabulary.

### `ingest` — turn document text into typed facts
Two different doors into the same store:

- **The paper door** (`paper_agent.rs`, `text_extract.rs`): an agent loop that
  reads the paper and the ontology through small, bounded tools, then proposes
  facts one at a time, each carrying the exact line numbers it was read from.
  This is the path a PDF uses.
- **The table door** (`pipeline.rs`, `local_facts.rs`): for spreadsheets and
  tabular data. Detects the schema, asks the model for entities and
  relationships, then maps each relationship to a typed fact.

Both doors hand their facts to the store. `ingest` also owns the **ontology
adapters** (`ontologies.rs`) — the registry that resolves "which ontology is
active for this run".

### `provenance` — the store and the receipt
The database layer (`emmo.rs`, `lib.rs`). Every fact is stored twice, in two
coordinated shapes:

- as a **graph node/edge** (the thing you query), and
- as a **PROV-O assertion** (a formal "who did what, based on which evidence"
  record) so every fact has an audit trail.

It is backed by a local **Turso/libSQL** database (a SQLite-compatible file) at
`~/.prism/provenance.db`. This crate is the system's memory.

### `llm` — the talk-to-the-model client
A single client for calling language models (OpenAI-compatible endpoints, or
the platform proxy). Handles streaming, tool-calling, and token counting. Every
crate that needs a model goes through this.

### `embed` — turn text into numbers for search
Turns names and sentences into vectors so the store can answer "find me things
similar to *this*". Uses a local model by default; can be swapped for a hosted
endpoint. Used by the store for semantic recall.

### `agent`, `tui`, `frontend`, `ipc`, `server` — the interactive system
A general-purpose assistant loop (`agent/agent_loop.rs`) that runs tools, asks
for human approval before risky ones, tracks cost, and avoids repeating itself.
The terminal UI (`tui`), a shared driver (`frontend`), a JSON-RPC surface for
editors (`ipc`), and a web/HTTP server (`server`) are the ways a human talks to
it. These are about the *assistant*, not about materials ingestion specifically.

### Everything else — the platform around it
`cli` (the `prism` command), `core` (config/sessions/permissions), `node`
(run daemon), `compute` (run jobs on Docker/cloud), `policy` (rule engine),
`mesh` (find other nodes), `campaign` (long autonomous runs), `workflows`
(YAML automation), `orch` (containers), `client` (platform API), `audit`
(signed records), `proto`/`runtime`/`python-bridge` (glue). None of these are
on the core "document → fact" path; they are the platform the discovery product
runs on.

---

## 3. What happens when a PDF is ingested, step by step

Command: `prism papers ingest …` (implemented in `crates/cli/src/papers.rs`).

```
 PDF / JATS XML
      |
      |  1. fetch + parse to clean text with line numbers
      v
 retrieval/fulltext.rs            (blocks: body / table / caption)
      |
      |  2. concatenate the chosen blocks into one text, tracking lines
      v
 cli/papers.rs                    (paper_text + per-block line ranges)
      |
      |  3. run the bounded reading agent against the active ontology
      v
 ingest/text_extract.rs  ->  ingest/paper_agent.rs
      |                         agent uses tools: search_paper, read_paper,
      |                         search_ontology, read_ontology, propose_fact
      |  4. each proposed fact carries exact cited line numbers
      v
 ingest/text_extract.rs           (materialize_proposal -> typed MaterialFact
      |                            + SourceCitation; deterministic checks become
      |                            annotations, not deletions)
      |  5. resolve the ontology class for each endpoint
      v
 ingest/paper_agent.rs            (resolve_class_binding)
      |
      |  6. write the fact + citation + provenance receipt
      v
 provenance/emmo.rs               (ProvenanceStore on Turso/libSQL)
      |
      v
 ~/.prism/provenance.db
```

Key idea at step 4 — **"annotate, don't refuse"**: once the agent has read the
paper and cited exact lines, PRISM does not over-rule it with an English word
matcher. Deterministic checks still run, but they *stamp* a fact with a status
("grounded", "model-asserted", "unit-unresolved", …) instead of silently
dropping it. Weak facts are stored but excluded from default reads. The only
things truly refused are shapes the database cannot represent at all (e.g.
unparseable JSON).

---

## 4. Where the ontology comes in, and what "the ontology governs
extraction" concretely means

The ontology is selected **once** at the start of a run and passed to every
step, so the prompt, the validator, and the storage layer all read the *same*
declaration and cannot disagree.

Concretely, the ontology decides:

| Question                                  | Answered by (no Rust word-list) |
|-------------------------------------------|---------------------------------|
| Which concepts exist, and their hierarchy | the ontology's classes          |
| Which labels the model may emit           | each class's `extraction_labels`|
| Which relations carry a measurement       | `Ontology::measurement_relations` |
| Which relations mean phase / processing / containment | `phase_relations`, etc. |
| How an entity type is named in storage    | `Ontology::storage_label`       |
| Whether a quantity can be negative        | `Ontology::quantity_sign_domain`|

So "the ontology governs extraction" means: **the code never guesses domain
meaning from a word's spelling.** If the active ontology does not declare that
a certain relation carries measurements, the value is *reported as unstorable
as a measurement* rather than silently guessed into one. If the ontology is
silent about whether a quantity can be negative, the sign check simply does not
apply — silence is never replaced by a guess. A customer brings a new domain by
authoring a new ontology and registering it (`register_ontology`); the Rust does
not change.

The built-in default ontology is **EMMO** (a materials ontology); a second
built-in (`MatKg`) and any number of customer ontologies can be registered. See
`crates/ingest/src/ontologies.rs`.

---

## 5. What is stored, where, and how to check it by hand

**Where:** one local database file, by default `~/.prism/provenance.db`
(Turso/libSQL, SQLite-compatible).

**What (the important tables):**

| Table                          | Holds                                          |
|--------------------------------|------------------------------------------------|
| `emmo_entity`                  | Graph nodes (materials, properties, …)         |
| `emmo_edge`                    | Graph edges (relations between nodes)          |
| `emmo_embedding`               | Vectors for semantic search                    |
| `prov_assertion`               | Each fact reified as an auditable assertion    |
| `prov_assertion_evidence`      | The evidence/citation backing each assertion   |
| `prov_activity` / `prov_agent` | Who/what produced the facts (the run, the model)|
| `provenance_records`           | The assistant's session memory                 |
| `agent_runs`                   | A ledger of agent runs (status, cost)          |
| `repair_queue` / `repair_disposition` | Older pre-agent items awaiting re-review |

**How to check by hand** (any SQLite browser works):

```sh
sqlite3 ~/.prism/provenance.db \
  "SELECT subject, predicate, object, value_num, unit FROM emmo_edge LIMIT 20;"
sqlite3 ~/.prism/provenance.db \
  "SELECT COUNT(*) FROM prov_assertion;"
```

Every stored fact should be traceable: from the edge/assertion, to its evidence
row, to the activity that created it, to the source document and line numbers.

---

## 6. Which parts are load-bearing, which are scaffolding

**Load-bearing (the system breaks without them):**
- `provenance/emmo.rs` + `lib.rs` — the store; everything ends here.
- `ingest/paper_agent.rs` + `text_extract.rs` — the paper-reading path.
- `ingest/local_facts.rs` + `pipeline.rs` — the table path.
- `retrieval/claims.rs` — the evidence matcher that keeps facts honest.
- `ingest/ontologies.rs` — ontology resolution; nothing is typed without it.
- `llm` — nothing reads a document without a model.

**Scaffolding / support (real, but not the core path):**
- The interactive assistant (`agent`, `tui`, `frontend`, `ipc`, `server`).
- The platform crates (`node`, `mesh`, `campaign`, `compute`, `workflows`,
  `policy`, `orch`, `client`, `audit`). These make PRISM a product/platform;
  you could remove them and still ingest a PDF into the store.

**Former gaps — recorded as resolved (they were open when first surveyed):**

- `crates/ingest/src/text_extract.rs::unwrap_soft_line_breaks` was fully
  built and tested but called by no production path. It is now wired into
  the repair tier: `repair.rs` joins soft-wrapped lines as part of subject
  normalization before the lexical re-check, and `repair_worker.rs` joins
  them before re-reading queued items. The fresh paper path still does not
  call it — by design, since that path runs no lexical checks.
- `crates/agent/src/agent_loop.rs` kept a `result_store` that oversized
  tool results were written into but never read back. It is deleted. The
  truncation message's promise — "the FULL result is in durable memory;
  call `recall(...)`" — is fulfilled by the provenance store (the post-hook
  records every tool call's complete output before truncation runs) and
  the `recall` meta-tool, which serves it by record id or query. An
  in-memory map dropped at turn end could never have served "durable
  memory" across sessions anyway.

---

## 7. The two ideas that hold the whole design together

1. **Every fact carries its receipt — or an honest annotation that it does
   not.** A fact whose citation was read but never checked against its span
   is stored with the `cited_by_reader` status; a fact a deterministic check
   could not ground is stored with the failing status and reason. Annotate,
   don't refuse: a weak fact stays stored and findable for re-reading (the
   `reverify` surface) instead of being dropped, and nothing unverified is
   promoted into default reads.
2. **Domain knowledge lives in the ontology, never in the code.** PRISM is a
   harness. Swap the ontology and you change the domain; the Rust is
   deliberately vocabulary-free.
