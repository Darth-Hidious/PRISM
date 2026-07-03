# PRISM Workflows — Authoring Guide

**Status:** Active. Canonical reference for writing PRISM workflows.
**Audience:** Humans writing workflows in YAML, AND LLMs asked to generate workflows on a user's behalf. The schema is **deliberately permissive** so LLM-generated YAML works on first try.

Workflows are YAML files anyone can write. Drop one into `~/.prism/workflows/` (global) or `.prism/workflows/` (per-project), and it becomes a top-level command — `prism <workflow-name>` — and a slash command in chat. Workflows can call tools, can call other workflows (no depth limit), and are governed by OPA policy.

---

## TL;DR — minimum viable workflow

Save as `~/.prism/workflows/hello.yaml`:

```yaml
name: hello
description: Say hello with a tool call.

arguments:
  - name: topic
    required: true

steps:
  - action: tool
    name: web
    inputs:
      action: search
      query: "{{ args.topic }}"
```

Run with `prism hello --topic "titanium alloys"`. It works.

`name` + `steps` is enough. Everything else has sensible defaults.

---

## Two dialects, both supported

PRISM accepts two YAML dialects. The parser auto-detects based on `kind:`.

### Dialect 1: `workflow` (deterministic)

The agent runs the steps you wrote, in order, with exactly the inputs you wrote. **Use when you know exactly what should happen.**

```yaml
api_version: prism/v1     # optional
kind: workflow             # optional, default
name: my_workflow
command_name: my-workflow  # optional, default = name with _ → -
description: What this does.
default_mode: dry_run      # dry_run | execute

arguments:
  - name: input
    type: string
    required: true
    help: "What to process"

steps:
  - id: step1               # optional, auto-generated if missing
    action: tool
    name: web
    inputs:
      action: search
      query: "{{ args.input }}"
```

### Dialect 2: `skill_workflow` (agent-orchestrated)

You give the agent a procedural plan; the agent picks tools and parameters at each step using its own judgment. **Use for open-ended research workflows where the right tool depends on intermediate results.**

```yaml
kind: skill_workflow
name: discover_alloys
description: Find materials matching a query and analyze them.
version: "1.0.0"
author: my_name

inputs:
  - name: query
    type: string
    description: Natural-language description of what to find.
    required: true

steps:
  - name: search
    skill: materials_search
    description: >
      Search materials databases for things matching the query.
      Use the user's query to pick elements + property ranges.
    inputs:
      query: "$query"
    outputs:
      materials: "$materials"
```

**The key difference:** in `workflow`, you specify exact tool inputs; in `skill_workflow`, you describe intent and the agent fills in the inputs.

**When in doubt: use `workflow`.** It's predictable and easier to debug.

---

## Schema — `workflow` dialect

### Top level

| Field | Type | Required? | Default | Notes |
|---|---|---|---|---|
| `kind` | string | no | `"workflow"` | `"workflow"` or omit; `skill_workflow` triggers the other dialect |
| `api_version` | string | no | `"prism/v1"` | Reserved for future versioning; ignored today |
| `name` | string | recommended | source filename | Logical name; lookup key |
| `command_name` | string | no | `name.replace("_", "-")` | The CLI command (`prism <command_name>`) |
| `description` | string | no | `"Run workflow '<name>'"` | One-liner shown in `--help` |
| `default_mode` | string | no | `"dry_run"` | `"dry_run"` or `"execute"`; `--execute` overrides |
| `arguments` | array | no | `[]` | CLI flags this workflow accepts |
| `steps` | array | no | `[]` | Ordered execution steps |
| `hooks` | object | no | `{}` | `on_start: [...]` runs before any step |

### `arguments[]`

| Field | Type | Required? | Default | Notes |
|---|---|---|---|---|
| `name` | string | yes | — | Becomes `--<name>` flag. PRISM accepts both `--my_arg` and `--my-arg`. |
| `type` | string | no | `"string"` | `"string"`, `"integer"`, `"number"`, `"boolean"`, `"list"` |
| `required` | boolean | no | `false` | If true and missing → error before any step runs |
| `help` | string | no | `""` | Shown in `prism <command> --help`. Aliases: `description` |
| `description` | string | no | `""` | Alias for `help` — both accepted |
| `default` | any | no | `null` | Default value if neither flag nor env is set |
| `env` | string | no | `""` | Falls back to this env var when flag is missing |
| `is_flag` | boolean | no | `false` | Treat as boolean flag (no value required on command line) |

### `steps[]` — common fields

Every step has `action` + step-specific config. The parser is permissive — extra fields are kept in the step's config map.

| Field | Type | Required? | Default | Notes |
|---|---|---|---|---|
| `id` | string | no | auto-generated (`step_0`, `step_1`, …) | Stable identifier; used in retry/fallback references |
| `name` | string | no | falls back to `id` | Human-readable label |
| `action` | string | yes | — | One of: `set`, `message`, `http`, `tool`, `if`, `parallel`, `workflow` |
| `description` | string | no | `""` | Free-text doc for the step |
| `if` | string (template) | no | — | Template that must resolve truthy for this step to run |
| `retry` | object | no | `null` | `{max: N, delay_ms: N, on: ["error_pattern"]}` |

### Step types

#### `action: tool` — call a registered PRISM tool

```yaml
- action: tool
  name: knowledge          # aliases: tool, tool_name
  inputs:                  # aliases: args, params, parameters
    action: search
    term: "{{ args.term }}"
```

#### `action: workflow` — invoke another workflow

**No depth limit.** Workflows can call workflows can call workflows. By design.

```yaml
- action: workflow
  name: forge              # workflow `name` OR `command_name`
  inputs:
    paper: "{{ args.paper }}"
    dataset: "{{ candidates }}"
```

#### `action: set` — set a context variable

```yaml
- action: set
  key: counter             # aliases: var
  value: 0                 # aliases: to
```

#### `action: message` — emit text

```yaml
- action: message
  text: "Processing {{ args.dataset }}..."  # aliases: body, content
```

#### `action: http` — make an HTTP call

```yaml
- action: http
  method: GET            # default: GET
  url: "https://api.example.com/data?q={{ args.query }}"
  headers:               # optional
    Authorization: "Bearer {{ args.token }}"
  body: null             # optional, JSON object or string
  store_as: response     # optional, name to bind the result to
```

#### `action: if` — conditional branch

```yaml
- action: if
  condition: "{{ args.dry_run }}"
  then:
    - action: message
      text: "Would run: ..."
  else:
    - action: tool
      name: compute_submit
      inputs: { ... }
```

#### `action: parallel` — concurrent steps

```yaml
- action: parallel
  steps:
    - action: tool
      name: web
      inputs: { action: search, query: "X" }
    - action: tool
      name: web
      inputs: { action: search, query: "Y" }
```

---

## Templates — `{{ args.name }}` syntax

| Pattern | Effect |
|---|---|
| `{{ args.input }}` | Look up `input` in the workflow context |
| `{{ args.results.0.formula }}` | Array indexing + nested object access |
| `{{ workflow_name }}`, `{{ command_name }}`, `{{ now_iso }}` | Built-in vars |
| `"{{ args.results }}"` (whole-value) | If the value is a list/dict, the entire string is replaced with the structured value |

**The `args.` prefix is a convention, not required.** `{{ input }}` and `{{ args.input }}` resolve the same way (input is in the top-level context).

**Filters NOT supported** — `{{ var | upper }}` won't work. Compute transformations in a `set` or `tool` step instead.

---

## Schema — `skill_workflow` dialect

Same top-level shape, with these differences:

- `kind: skill_workflow`
- `inputs:` instead of `arguments:` (both accepted)
- Steps use `name` + `skill` (not `id` + `action`)
- Templates use `$variable` (or `{{ }}` — both accepted)
- Each step has a `description:` that the LLM reads as natural-language guidance
- `inputs:` and `outputs:` describe the agent's contract — the agent picks the actual tool call

```yaml
kind: skill_workflow
name: my_skill_workflow
description: What this workflow does in plain English.

inputs:
  - name: query
    type: string
    required: true

steps:
  - name: search
    skill: materials_search   # or `tool`, `agent_action`, etc.
    description: >
      Search the federated material databases for things matching $query.
      Use the user's query to choose elements + property ranges.
    inputs:
      query: "$query"
    outputs:
      materials: "$materials"
    llm_can_modify_inputs: true  # optional: agent may override inputs
```

The agent runtime executes `skill_workflow` files. The agent reads each step's description, picks the right tool, and decides parameters.

---

## LLM authoring guide — write a workflow that runs first try

If you're an LLM writing a PRISM workflow, follow these rules:

### 1. Use `workflow` unless the user explicitly asked for `skill_workflow`

The deterministic dialect is easier to validate and debug.

### 2. Minimum viable shape

```yaml
name: <verb_noun>
description: <one-liner>

arguments:
  - name: <arg>
    required: true

steps:
  - action: tool
    name: <tool_name>
    inputs:
      <key>: "{{ args.<arg> }}"
```

Omit `kind`, `api_version`, `command_name`, `default_mode`, step `id`, step `description`. They have safe defaults.

### 3. Tool names — pick from the registered list

| Tool | Use for |
|---|---|
| `web` | `action='read'\|'search'` — open-web access |
| `knowledge` | `action='search'\|'entity'\|'paths'\|'stats'\|'semantic'\|'list_corpora'\|'ingest'\|'promote_artifact'` — MARC27 KG |
| `materials_search` | federated materials database search |
| `query_materials_project` | MP-specific deep property query |
| `prior_art_search` | papers/patents (`source='papers'\|'patents'\|'both'`) |
| `predict` | `target='formula'\|'structure'` — ML prediction |
| `plot` | `kind='materials_comparison'\|'property_distribution'\|'correlation_matrix'` |
| `dataset` | `action='import'\|'export'\|'validate'\|'review'\|'visualize'` |
| `file` | `action='read'\|'write'\|'edit'` — local files |
| `compute` | `action='list_gpus'\|'estimate'\|'status'\|'cancel'\|'list_providers'` |
| `compute_submit` | dispatch a real GPU/CPU job (approval-gated) |
| `research` | RLM deep research (approval-gated) |
| `recall` / `fetch_artifact` / `list_artifacts` | stateful memory recall |
| `structure` | `action='create'\|'modify'\|'info'` (atomistic; needs pyiron) |
| `sim_run` | run a simulation (atomistic; needs pyiron, approval-gated) |
| `sim_job` | `action='status'\|'results'\|'list'\|'delete'` simulation jobs |
| `calphad` / `calphad_compute` | thermodynamic calculations (needs pycalphad) |
| `bash_task` | `action='list'\|'read'` background bash tasks |
| `execute_bash`, `execute_python` | run shell or python (approval-gated) |

If you don't know whether a tool exists, write the workflow anyway and the parser will give you a "did you mean" suggestion.

### 4. Templates — keep them simple

✅ `{{ args.input }}`, `{{ args.results.0.formula }}`, `{{ now_iso }}`
❌ `{{ args.input | upper }}` — filters not supported
❌ `{{ if args.foo then 1 else 2 }}` — use `action: if` step
❌ `{{ "literal string" }}` — just write the literal

### 5. Don't worry about step IDs

The parser auto-generates `step_0`, `step_1`, etc. You only need an `id:` if a later step references this one (e.g., for retry).

### 6. Aliases — pick whichever feels natural

These all work:

```yaml
# All equivalent:
- action: tool
  name: web         # canonical
- action: tool
  tool: web         # alias
- action: tool
  tool_name: web    # alias

# All equivalent:
inputs: { ... }       # canonical
args: { ... }         # alias
params: { ... }       # alias
parameters: { ... }   # alias
```

### 7. When unsure, use `action: message`

If a step is incomplete, just emit a message and stop:

```yaml
- action: message
  text: "TODO: implement this step properly"
```

The workflow runs cleanly in dry-run mode and you can iterate.

### 8. Validate before running

`prism workflow validate <file>` reports problems without executing.

---

## Validation rules — what's accepted vs rejected

The parser is **permissive on shape** and **aggressive on safety**. Specifically:

### Accepted (won't fail validation)

- Extra fields at any level (logged as a warning, kept in `raw`)
- Misnamed top-level fields with known aliases (`inputs` ≡ `arguments`)
- Steps without `id` (auto-generated)
- Steps without `description`
- Mixed dialects (don't do this on purpose; if a `skill_workflow` file has a `workflow`-dialect step, the agent runtime tolerates it)
- Templates that reference unknown variables (fail at execution, not parse)
- Workflows that call themselves recursively — **no depth limit**

### Rejected (parser error before execution)

- YAML that doesn't parse (syntax error)
- Top-level not a map
- `kind:` set to something other than `workflow`/`skill_workflow`/empty
- A step with no `action` (and not in `skill_workflow` dialect)
- `action:` set to an unknown value — error includes "did you mean?" suggestions
- Required `arguments[].name` missing
- Negative or non-numeric `retry.max`

### Runtime errors (run-time, not parse-time)

- Tool not found
- Tool inputs malformed
- Template references unknown variable
- HTTP step gets 4xx/5xx (subject to `retry`)
- OPA policy denies the action
- Workflow recursion runs longer than your cost cap (no artificial depth limit, but `audit_log` obligations make runaway loops visible; federation manifest cost caps stop real-money runaway)

---

## Governance — workflows are policy-gated

Every workflow execution and every tool step inside a workflow is checked against the OPA policy engine (`crates/policy/`, regorus-based, pure Rust). Policy can:

- **Deny** the workflow at the top — `policy denied workflow 'x': not in agent_approved_workflows`
- **Deny** a specific tool step — `policy denied tool 'compute_submit' in workflow step 'train': destructive tool requires admin role`
- **Require obligations** — the runtime fulfills these (e.g., `audit_log` causes `tracing::info!("AUDIT: ...")` and adds `_audit` to context)

User-installed workflows from `~/.prism/workflows/` are NOT auto-trusted — the agent role must explicitly include them in the policy's `agent_approved_workflows` set, OR the user runs with `--role operator` (or higher).

To override the default policy, drop a `.rego` file in `~/.prism/policies/` (global) or `.prism/policies/` (per-project). See `crates/policy/src/default.rego`.

---

## Examples — copy and modify

### Search and report

```yaml
name: search_report
description: Search the web and write a markdown summary.

arguments:
  - name: query
    required: true

steps:
  - action: tool
    id: search
    name: web
    inputs:
      action: search
      query: "{{ args.query }}"
      limit: 10

  - action: tool
    name: file
    inputs:
      action: write
      path: "./search_results.md"
      content: "# Search results for {{ args.query }}\n\n{{ search.results }}"
```

### Nested workflow (no depth limit)

```yaml
name: full_pipeline
description: Search, then forge a model from the results.

arguments:
  - name: topic
    required: true

steps:
  - action: workflow
    name: search_report
    inputs:
      query: "{{ args.topic }}"

  - action: workflow
    name: forge
    inputs:
      paper: "arxiv:..."
      dataset: "{{ args.topic }}"
      target: "runpod:A100"
```

### Conditional + parallel

```yaml
name: batch_process
description: Process N items, in parallel where safe.

arguments:
  - name: items
    type: list
    required: true
  - name: parallel
    type: boolean
    default: true

steps:
  - action: if
    condition: "{{ args.parallel }}"
    then:
      - action: parallel
        steps:
          - action: tool
            name: predict
            inputs: { target: formula, formula: "{{ args.items.0 }}" }
          - action: tool
            name: predict
            inputs: { target: formula, formula: "{{ args.items.1 }}" }
    else:
      - action: tool
        name: predict
        inputs: { target: formula, formula: "{{ args.items.0 }}" }
```

### Skill workflow (agent-orchestrated)

```yaml
kind: skill_workflow
name: full_research
description: Open-ended research on a topic.

inputs:
  - name: topic
    required: true

steps:
  - name: discover
    skill: research
    description: >
      Run a deep research session on $topic. Call research(question="...")
      with depth >= 1.
    inputs:
      topic: "$topic"

  - name: synthesize
    skill: write_report
    description: >
      Read the discover step's output and produce a markdown report
      with citations. Pick the right file path.
```

---

## Discovery + precedence

Workflows load from these directories in order (later overrides earlier on name conflict):

1. **Built-in** — compiled into the binary (currently: `forge`)
2. **`.prism/workflows/`** — per-project workflows (relative to the directory `prism` is invoked from)
3. **`~/.prism/workflows/`** — global user workflows

Two workflows with the same name → the later one wins. `prism workflow list` shows all discovered. `prism --help` lists each as a top-level command.

---

## Out of scope (for now)

- **Cron / scheduled execution** — workflows run on demand only.
- **Cross-machine workflow sync** — local-only. (PRISM Fabric will add federated workflow execution.)
- **Workflow versioning beyond `version:` field** — the file is the source of truth.
- **Variable scoping beyond top-level context** — all variables are flat.
- **User-defined functions / macros** — keep it simple.

---

## Migration notes — older PRISM releases

Pre-1.0 workflows are tolerated by the alias-tolerant parser:

- `tasks:` → `steps:` (alias)
- `kind: prism_workflow` → `kind: workflow` (alias)
- `with:` → `inputs:` (alias)

You don't need to migrate proactively.
