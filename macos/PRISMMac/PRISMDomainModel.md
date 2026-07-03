# PRISM Mac Product Domain Model

Date: 2026-05-24
Research tool: Exa MCP

## Why This Exists

PRISM is not "ChatGPT plus a graph view." It is a materials-research command
surface over MARC27, PRISM CLI, model providers, workflows, compute, billing,
and provenance.

The default UX can feel like ChatGPT on Mac, but the product domain comes from
computational materials science: calculations, workflows, schedulers, provenance
graphs, evidence, datasets, reusable plugins, and reproducibility.

## External Domain References

- AiiDA: workflow manager for computational science with strong focus on
  provenance, scalable execution, extensibility, HPC schedulers, and exportable
  provenance graphs.
- atomate2: computational materials-science workflows built on pymatgen,
  custodian, jobflow, and FireWorks; standard workflows for band structures,
  elastic/dielectric tensors, phonons, defects, transport, and bonding analysis.
- FireWorks: high-throughput workflow management for computational materials
  science.
- AiiDA-VASP: bridge between VASP electronic-structure calculations and a
  provenance-aware workflow framework.

## PRISM Domain Objects

### Conversation

Natural-language work surface. It should feel like ChatGPT for Mac.

Owns:

- prompt;
- app/context attachments;
- selected files/artifacts/screenshots;
- model choice;
- limits before send;
- follow-up branches.

### Research Question

The goal object. It should preserve intent and constraints.

Fields:

- objective;
- material family;
- operating environment;
- property targets;
- evidence depth;
- budget/credit cap;
- allowed models/providers;
- export target.

### Knowledge Query

Graph or semantic retrieval.

Fields:

- query text;
- corpus/tenant;
- entity filters;
- result limit;
- provenance requirement;
- linked citations;
- confidence/score.

### Material Candidate

Candidate row or object, not just prose.

Fields:

- formula/system;
- phase/structure;
- properties;
- uncertainty;
- evidence count;
- source papers;
- generated calculations;
- rejected/shortlisted status.

### Workflow

Executable domain recipe.

Examples:

- DFT relaxation;
- band structure;
- phonon calculation;
- defect formation;
- molecular dynamics;
- CALPHAD/phase diagram;
- multi-agent discourse review;
- evidence bundle export.

Fields:

- workflow spec;
- parameters;
- dependencies;
- dry-run result;
- approvals;
- idempotency key;
- execution state.

### Calculation / Job

Concrete execution of a workflow step.

Fields:

- engine/image/executable;
- input files;
- scheduler/backend;
- CPU/GPU/memory/time;
- budget cap;
- logs;
- stdout/stderr;
- artifacts;
- status;
- retry policy.

### Provenance Graph

The heart of ESA-grade trust.

Edges:

- question used constraints;
- query produced candidates;
- paper supports claim;
- workflow consumed material;
- calculation produced artifact;
- discourse challenged verdict;
- bundle contains evidence.

### Evidence Bundle

Reviewable export.

Contains:

- final answer;
- citations;
- source hashes where available;
- model/LLM limits;
- prompts and settings;
- workflow specs;
- job IDs/log references;
- artifacts;
- provenance graph snapshot;
- billing/usage summary.

### Model Limit

Visible guardrail, not backend trivia.

Fields:

- model/provider;
- context window;
- max output;
- tool-call limit;
- project/session spend cap;
- rate limit;
- allowed data policy;
- reason a model was selected.

### Billing / Governance

Read-only in demo until Stripe hardening is complete.

Fields:

- credit balance;
- per-project usage;
- estimated cost before execution;
- approval gates;
- payment state;
- unsafe/disabled actions.

## UI Mapping

### Default Home: Chat

Use for:

- asking research questions;
- reading synthesis;
- attaching app/project context;
- checking limits;
- deciding what to branch into.

Should not look like:

- a grid dashboard;
- a node editor;
- a CLI command catalog.

### Workflow Board

Use only when execution/provenance is spatial:

- workflow plans;
- simulation pipelines;
- discourse/critic loops;
- evidence-bundle assembly;
- compute dependency graphs.

The board should show executable nodes with ports because this maps to the
workflow/provenance domain.

### Inspector

Inspector contents depend on mode:

- Chat mode: context, limits, artifacts, app links.
- Workflow mode: node state, inputs, outputs, command contract, provenance.

## Product Rule

If the task is conversational, keep it in chat.

If the task creates dependencies, jobs, artifacts, or audit claims, branch it
into a workflow board.
