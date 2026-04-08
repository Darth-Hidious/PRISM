# PRISM Workflow Guide

How to write, run, and secure PRISM workflows.

## Quick Start

Drop a YAML file in `~/.prism/workflows/` and it becomes a CLI command:

```yaml
# ~/.prism/workflows/screen.yaml
api_version: prism/v1
kind: workflow
name: screen
command_name: screen
description: Screen candidate alloys against property thresholds.
arguments:
  - name: alloy
    type: string
    required: true
    help: Alloy system, e.g. Ni-Cr-Co
steps:
  - id: greet
    action: message
    text: "Screening {{ alloy }}..."
```

```bash
prism screen --alloy "Ni-Cr-Co"
# → screen  dry_run
#   greet  message  planned  Screening Ni-Cr-Co...
```

No compilation, no registration. Discovered automatically.

---

## Full Example: Materials Exploration Workflow

This workflow searches the knowledge graph, submits a compute job, and reports results.

```yaml
# ~/.prism/workflows/explore.yaml
api_version: prism/v1
kind: workflow
name: explore
command_name: explore
description: Explore diverse materials using GFlowNet sampling.
default_mode: dry_run

arguments:
  - name: space
    type: string
    required: true
    help: Element space, e.g. Ni-Cr-Co-Al-Ti
  - name: target
    type: string
    required: true
    help: Property target, e.g. "yield_strength > 900"
  - name: depth
    type: string
    default: "100"
    help: Number of candidates to sample
  - name: model
    type: string
    default: gflownet-composition-v1
    help: GFlowNet model slug from marketplace
  - name: gpu
    type: string
    default: RTX-4090
    help: GPU type for compute job
  - name: platform_api_base
    type: string
    default: https://api.marc27.com/api/v1
    help: MARC27 API base URL
  - name: auth_token
    type: string
    env: MARC27_API_KEY
    help: Platform auth token (reads from MARC27_API_KEY env var)

steps:
  # ──────────────────────────────────────────────────────
  # Step 1: Set up exploration parameters
  # ──────────────────────────────────────────────────────
  - id: setup
    action: set
    values:
      space: "{{ space }}"
      target: "{{ target }}"
      depth: "{{ depth }}"
      model: "{{ model }}"
      gpu: "{{ gpu }}"
      method: gflownet

  # ──────────────────────────────────────────────────────
  # Step 2: Search knowledge graph for existing materials
  # in the target composition space
  # ──────────────────────────────────────────────────────
  - id: kg_search
    action: http
    method: GET
    url: "{{ platform_api_base }}/knowledge/graph/search?q={{ space }}&limit=10"
    headers:
      Authorization: "Bearer {{ auth_token }}"
    expect_status: [200]

  # ──────────────────────────────────────────────────────
  # Step 3: Log what we found
  # ──────────────────────────────────────────────────────
  - id: kg_report
    action: message
    text: "Found existing materials in {{ space }} space. Launching GFlowNet to explore beyond known candidates."

  # ──────────────────────────────────────────────────────
  # Step 4: Submit GFlowNet compute job
  # ──────────────────────────────────────────────────────
  - id: submit_job
    action: http
    method: POST
    url: "{{ platform_api_base }}/compute/submit"
    headers:
      Authorization: "Bearer {{ auth_token }}"
      Content-Type: application/json
    body:
      image: "marc27/gflownet:latest"
      name: "explore-{{ space }}"
      gpu_type: "{{ gpu }}"
      inputs:
        space: "{{ space }}"
        target: "{{ target }}"
        depth: "{{ depth }}"
        model: "{{ model }}"
        seed_materials: "{{ kg_search.body }}"
    expect_status: [200, 201, 202]

  # ──────────────────────────────────────────────────────
  # Step 5: Report job submission
  # ──────────────────────────────────────────────────────
  - id: report
    action: message
    text: "Submitted job {{ submit_job.body.job_id }} on {{ gpu }}. Track with: prism job-status {{ submit_job.body.job_id }}"
```

### Running it

```bash
# Dry run (default) — shows plan without executing
prism explore --space "Ni-Cr-Co-Al-Ti" --target "yield_strength > 900"

# Execute for real
prism explore --space "Ni-Cr-Co-Al-Ti" --target "yield_strength > 900" --execute

# Override defaults
prism explore --space "Fe-Mn-Al-C" --target "elongation > 30" --depth 500 --gpu A100 --execute

# Via the workflow subcommand (equivalent)
prism workflow run explore --set space=Ni-Cr-Co --set target="hardness > 400" --execute
```

### Output (dry run)

```
explore  dry_run
Explore diverse materials using GFlowNet sampling.
setup          set      planned  set 6 value(s)
kg_search      http     planned  GET https://api.marc27.com/api/v1/knowledge/graph/search?q=Ni-Cr-Co-Al-Ti&limit=10
kg_report      message  planned  Found existing materials in Ni-Cr-Co-Al-Ti space...
submit_job     http     planned  POST https://api.marc27.com/api/v1/compute/submit
report         message  planned  Submitted job {{ submit_job.body.job_id }} on RTX-4090...
```

### Output (execute)

```
explore  execute
setup          set      completed  set 6 value(s)
kg_search      http     completed  HTTP 200 count=10
kg_report      message  completed  Found existing materials in Ni-Cr-Co-Al-Ti space...
submit_job     http     completed  HTTP 202 id=f8a3b2c1-...
report         message  completed  Submitted job f8a3b2c1-... on RTX-4090. Track with: prism job-status f8a3b2c1-...
```

---

## Workflow YAML Reference

### Top-level fields

```yaml
api_version: prism/v1        # Always prism/v1
kind: workflow               # Always "workflow"
name: my-workflow            # Internal name (used for lookup)
command_name: my-cmd         # CLI alias: prism my-cmd
description: What it does.   # Shown in prism workflow list
default_mode: dry_run        # "dry_run" or "execute"
arguments: [...]             # CLI arguments
steps: [...]                 # DAG of execution steps
hooks:                       # Optional lifecycle hooks
  on_start: [...]            # Steps to run before the workflow
  on_complete: [...]         # Steps to run after success
  on_error: [...]            # Steps to run on failure
```

### Arguments

```yaml
arguments:
  - name: space              # Argument name (used as --space in CLI)
    type: string             # "string" (only type currently)
    required: true           # Fails if not provided
    help: Description text   # Shown in prism workflow show
    default: "value"         # Default if not provided
    env: MY_ENV_VAR          # Read from env var if not provided
    is_flag: false           # If true, presence = "true" (no value needed)
```

**Resolution order:** CLI flag → env var → default → error if required.

### Step types

#### `set` — Set context variables

```yaml
- id: config
  action: set
  values:
    key1: "{{ argument_name }}"
    key2: "literal value"
    key3: "combined {{ a }} and {{ b }}"
```

Sets variables in the workflow context. Available to all subsequent steps via `{{ key }}`.

#### `message` — Display text

```yaml
- id: status
  action: message
  text: "Processing {{ input }} with {{ model }}..."
```

Logs a message. In dry run mode shows `planned`, in execute mode shows `completed`.

#### `http` — Call any API

```yaml
- id: api_call
  action: http
  method: POST                          # GET, POST, PUT, DELETE
  url: "https://api.example.com/endpoint"
  headers:
    Authorization: "Bearer {{ token }}"
    Content-Type: application/json
  body:                                 # JSON body (POST/PUT)
    field: "{{ value }}"
    nested:
      key: "{{ other_value }}"
  expect_status: [200, 201, 202]        # Fail if status not in list
```

Response stored in context as `{{ step_id.body }}`, `{{ step_id.status_code }}`, `{{ step_id.headers }}`.

#### `tool` — Call a PRISM tool

```yaml
- id: predict
  action: tool
  name: predict_properties              # Tool name from prism tools
  command: train                        # Optional sub-command
  inputs:
    dataset: "{{ data }}"
    property: hardness
```

Calls `POST http://127.0.0.1:7327/api/tools/{name}/run` on the local node. Requires `prism node up` to be running. Response stored as `{{ step_id.output }}`.

**OPA policy is checked per tool step** — see Security section below.

#### `if` — Conditional branching

```yaml
- id: check_results
  action: if
  condition: "{{ search.body.count }}"    # Truthy check
  then:
    - id: found
      action: message
      text: "Found {{ search.body.count }} materials"
    - id: process
      action: tool
      name: predict_properties
      inputs:
        dataset: "{{ search.body }}"
  else:
    - id: not_found
      action: message
      text: "No materials found — trying broader search"
```

**Truthy values:** non-empty string (except `"false"`, `"0"`, `"null"`), non-zero number, non-empty array/object, `true`.

**Falsy values:** empty string, `"false"`, `"0"`, `"null"`, `0`, `null`, empty array/object.

Sub-steps in `then`/`else` can be any step type (`set`, `message`, `http`, `tool`). Context set by sub-steps is available to subsequent workflow steps.

Stored in context as `{{ step_id.branch }}` ("then" or "else") and `{{ step_id.condition }}`.

#### `parallel` — Concurrent execution

```yaml
- id: multi_search
  action: parallel
  steps:
    - id: search_mp
      action: http
      method: GET
      url: "https://api.materialsproject.org/materials?formula={{ formula }}"
    - id: search_nomad
      action: http
      method: GET
      url: "https://nomad-lab.eu/api/v1/entries?formula={{ formula }}"
    - id: search_graph
      action: http
      method: GET
      url: "{{ platform_api_base }}/knowledge/graph/search?q={{ formula }}"
```

All sub-steps execute concurrently via `tokio::spawn`. Each sub-step's context is merged back into the parent. In dry run, shows the plan without executing.

Stored in context as `{{ step_id.completed }}` (count) and `{{ step_id.steps }}` (list of sub-step IDs).

**Note:** Sub-steps run independently — they cannot reference each other's output. Use `parallel` for fan-out queries, not for dependent chains.

#### `workflow` — Call a sub-workflow

```yaml
- id: train_model
  action: workflow
  name: forge                              # Must exist in discovery paths
  inputs:
    paper: "{{ paper }}"
    dataset: "{{ candidates }}"
    target: "local"
```

Recursively executes another workflow with its own arguments, steps, and policy checks. The child workflow's full result (context + steps) is stored under the step ID.

Access child results: `{{ train_model.workflow }}`, `{{ train_model.steps }}`, `{{ train_model.context.variable }}`.

**OPA policy:** The child workflow gets its own `workflow.execute` policy check. If the child is denied, the parent aborts.

#### Retries — on any step

Any step can have retry configuration:

```yaml
- id: flaky_api
  action: http
  method: GET
  url: "https://unreliable-api.example.com/data"
  retries: 3                  # Retry up to 3 times on failure
  retry_delay_secs: 2         # Base delay (multiplied by attempt number)
  expect_status: [200]
```

Retry behavior:
- Attempt 0: immediate
- Attempt 1: wait `retry_delay_secs * 1` seconds
- Attempt 2: wait `retry_delay_secs * 2` seconds
- If all attempts fail, the workflow aborts with the last error
- Dry run mode never retries

Works on all step types: `set`, `message`, `http`, `tool`, `if`, `parallel`, `workflow`.

---

## Hooks

Hooks run before and after the main workflow steps. They're defined at the top level of the YAML:

```yaml
hooks:
  on_start:
    - id: notify_start
      action: http
      method: POST
      url: "https://slack.example.com/webhook"
      body:
        text: "Workflow {{ workflow_name }} starting"
    - id: log_start
      action: message
      text: "Starting {{ workflow_name }} at {{ now_iso }}"

  on_complete:
    - id: notify_done
      action: http
      method: POST
      url: "https://slack.example.com/webhook"
      body:
        text: "Workflow {{ workflow_name }} completed"

  on_error:
    - id: notify_fail
      action: message
      text: "Workflow {{ workflow_name }} failed"
```

| Hook | When it runs | Context available |
|------|-------------|-------------------|
| `on_start` | Before any step, after argument resolution | Arguments + builtins only |
| `on_complete` | After all steps succeed | Full context including all step outputs |
| `on_error` | When any step fails (planned, not yet wired) | Context up to the failed step |

Hook steps can be `set`, `message`, or `http`. Hook failures are logged but don't abort the workflow.

---

## OPA Obligations

When OPA policy allows a workflow, it may also return **obligations** — things the system must do as a side effect.

### Built-in obligations

| Obligation | Trigger | Effect |
|------------|---------|--------|
| `audit_log` | Any `workflow.execute` action | Logs workflow start/complete via `tracing::info` with workflow name, principal, and timestamps |
| `notify_admin` | Agent role executing a workflow | Emits `tracing::warn` so admin dashboards/alerting can pick it up |

### Custom obligations

Add custom obligations in your `.rego` policy:

```rego
package prism.policy

# Require cost approval for expensive workflows
obligations contains "cost_approval" if {
    input.action == "workflow.execute"
    input.context.step_count > 10
}

# Require audit for any agent action
obligations contains "audit_log" if {
    input.principal == "agent"
}
```

Obligations are returned in the `PolicyDecision.obligations` field and logged/acted on by the workflow engine. Custom obligation handlers can be added in the Rust workflow engine as needed.

---

## Template Engine

Templates use `{{ path }}` syntax with dot-path resolution.

```yaml
# Simple variable
text: "Hello {{ name }}"

# Step output (from http step)
url: "{{ previous_step.body.job_id }}"

# Nested access
text: "Status: {{ api_call.body.result.status }}"

# Built-in variables (injected automatically)
text: "Workflow {{ workflow_name }} started at {{ now_iso }}"
```

### Built-in context variables

| Variable | Value |
|----------|-------|
| `{{ workflow_name }}` | Workflow `name` field |
| `{{ command_name }}` | Workflow `command_name` field |
| `{{ now_iso }}` | Current UTC timestamp (RFC 3339) |
| `{{ node_port }}` | Local node port (default 7327) |

### Context chaining

Each step's output is stored under its `id`. Subsequent steps can reference it:

```yaml
steps:
  - id: search
    action: http
    url: "https://api.marc27.com/api/v1/knowledge/graph/search?q=titanium"

  - id: report
    action: message
    text: "Found {{ search.body }} results"
    #                ^^^^^^ references the search step's response body
```

For `http` steps, the stored context is:
```json
{
  "status_code": 200,
  "headers": { "content-type": "application/json" },
  "body": { ... }
}
```

For `tool` steps:
```json
{
  "status_code": 200,
  "output": { ... }
}
```

---

## Security: OPA Policy

Every workflow execution is checked by the OPA/Rego policy engine. There are two levels of checks:

### 1. Workflow-level check

Before any step runs, the engine evaluates:

```json
{
  "action": "workflow.execute",
  "principal": "user-id-or-agent",
  "role": "operator",
  "resource": "explore",
  "context": { "execute": true, "step_count": 5 }
}
```

**Default policy rules:**
- `admin` — can execute any workflow
- `operator` — can execute any workflow
- `agent` — can only execute workflows in the `agent_approved_workflows` set
- `viewer` — denied

### 2. Per-tool-step check

Each `tool` step triggers an additional check:

```json
{
  "action": "tool.call",
  "principal": "user-id-or-agent",
  "role": "operator",
  "resource": "predict_properties",
  "context": { "dataset": "alloys", "property": "hardness" }
}
```

**Default policy rules:**
- Destructive tools (`knowledge_ingest`, `data_delete`, `node_restart`, `config_update`, `user_manage`) require `admin`
- Tools with `context.mode == "delete"` or `context.mode == "write"` require `admin`
- All other tools allowed for `operator` and above
- `agent` role can call non-destructive tools

### 3. Custom policies

Drop `.rego` files in `~/.prism/policies/` or `.prism/policies/` to override:

```rego
# ~/.prism/policies/restrict-explore.rego
package prism.policy

# Only admins can run explore with depth > 1000
deny contains msg if {
    input.action == "workflow.execute"
    input.resource == "explore"
    to_number(input.context.depth) > 1000
    input.role != "admin"
    msg := "explore with depth > 1000 requires admin role"
}

# Add explore to agent-approved workflows
agent_approved_workflows := {
    "train-indexer", "forge", "search", "predict",
    "data-export", "explore"
}
```

### Policy decisions

| Decision | Meaning |
|----------|---------|
| `allow` | Proceed with execution |
| `deny` | Block with error message (collected as violations) |
| `obligations` | Side effects required (e.g., `audit_log`, `notify_admin`) |
| `reason` | Human-readable explanation |

---

## Discovery Paths

Workflows are loaded from these directories (in order):

1. `.prism/workflows/` in the current project
2. `~/.prism/workflows/` in the user's home
3. Built-in workflows (compiled into the binary, e.g., `forge`)

Later definitions override earlier ones (project > user > builtin).

```bash
# See all discovered workflows
prism workflow list

# See details of one
prism workflow show explore

# Run via alias
prism explore --space "Ni-Cr-Co" --target "hardness > 400"

# Run via workflow subcommand
prism workflow run explore --set space=Ni-Cr-Co --set target="hardness > 400"

# Execute (not dry run)
prism explore --space "Ni-Cr-Co" --target "hardness > 400" --execute
```

---

## Step Execution Details

### Dry run vs Execute

| Mode | `set` | `message` | `http` | `tool` | `if` | `parallel` | `workflow` |
|------|-------|-----------|--------|--------|------|------------|------------|
| `dry_run` | Context updated, `planned` | Text rendered, `planned` | URL shown, **not called** | Tool shown, **not called** | Condition evaluated, branch shown | Steps listed, **not called** | Child shown, **not called** |
| `execute` | Context updated, `completed` | Text rendered, `completed` | HTTP called, response stored | Tool called via node API | Branch executed, sub-steps run | All sub-steps run concurrently | Child workflow fully executed |

### Error handling

- If an `http` step returns a status not in `expect_status`, the workflow **aborts** (unless `retries` is set)
- If a `tool` step returns HTTP 4xx/5xx, the workflow **aborts** (unless `retries` is set)
- If a template variable doesn't exist in context, the workflow **aborts** with `unknown workflow context path`
- If a required argument is missing, the workflow **aborts** before any step runs
- OPA deny → workflow **aborts** with the deny message
- `retries` → retries with exponential backoff before aborting
- `parallel` → if any sub-step fails, the entire parallel step fails
- `workflow` → if the child workflow fails, the parent aborts
- Hook failures are **logged but do not abort** the workflow

### Context lifetime

Context lives for the duration of the workflow run. Each step adds to it:

```
on_start hooks (can set context)
  └─ step 1 output added
       └─ step 2 output added (if/parallel/workflow sub-steps also add)
            └─ step 3 can read from step 1, step 2, all args, and hook context
on_complete hooks (can read full context)
```

For `parallel` steps, each sub-step runs with a snapshot of the current context. Sub-step outputs are merged back — if two sub-steps set the same key, last-to-finish wins.

For `workflow` (nesting), the child gets its own context built from `inputs`. The child's full result is stored under the parent step ID — access with `{{ step_id.context.variable }}`.

---

## Putting It Together: GFlowNet Exploration

The complete flow for adding a new GFlowNet exploration capability:

1. **Publish the model** to marketplace:
   ```bash
   prism publish ./gflownet-checkpoint --to marc27 --repo marc27/gflownet-composition-v1
   ```

2. **Write the workflow** at `~/.prism/workflows/explore.yaml` (see full example above)

3. **Add OPA policy** (optional) at `~/.prism/policies/explore.rego`:
   ```rego
   package prism.policy
   agent_approved_workflows := {
       "forge", "explore", "search", "predict", "data-export"
   }
   ```

4. **Run it**:
   ```bash
   prism explore --space "Ni-Cr-Co-Al-Ti" --target "yield_strength > 900" --execute
   ```

No Rust code changes. No CLI modifications. No compilation. The workflow engine handles discovery, argument parsing, template rendering, OPA policy, HTTP calls, tool dispatch, and result reporting.

---

## Complete Example: All Features

A workflow that uses every engine feature:

```yaml
api_version: prism/v1
kind: workflow
name: full-pipeline
command_name: pipeline
description: End-to-end materials discovery with all engine features.

arguments:
  - name: formula
    type: string
    required: true
    help: Chemical formula to investigate, e.g. NiCrCoAlTi
  - name: target
    type: string
    default: yield_strength
    help: Property to optimize

hooks:
  on_start:
    - id: h_start
      action: message
      text: "Pipeline starting for {{ formula }} targeting {{ target }}"
  on_complete:
    - id: h_done
      action: http
      method: POST
      url: "https://hooks.example.com/notify"
      body:
        text: "Pipeline for {{ formula }} completed"

steps:
  # 1. Search multiple databases in parallel
  - id: search
    action: parallel
    steps:
      - id: graph
        action: http
        method: GET
        url: "https://api.marc27.com/api/v1/knowledge/graph/search?q={{ formula }}"
      - id: semantic
        action: http
        method: POST
        url: "https://api.marc27.com/api/v1/knowledge/search"
        body:
          query: "{{ formula }} {{ target }}"

  # 2. Check if we found anything
  - id: check
    action: if
    condition: "{{ graph.body }}"
    then:
      - id: found
        action: message
        text: "Found existing data — enriching with predictions"
    else:
      - id: not_found
        action: message
        text: "No existing data — starting from scratch"

  # 3. Run GFlowNet exploration (retries for flaky GPU)
  - id: explore
    action: http
    method: POST
    url: "https://api.marc27.com/api/v1/compute/submit"
    retries: 2
    retry_delay_secs: 5
    body:
      image: "marc27/gflownet:latest"
      name: "explore-{{ formula }}"
      inputs:
        formula: "{{ formula }}"
        target: "{{ target }}"

  # 4. Run the forge sub-workflow to train a model
  - id: train
    action: workflow
    name: forge
    inputs:
      paper: "gflownet-output"
      dataset: "{{ formula }}"
      target: "local"

  # 5. Report
  - id: report
    action: message
    text: "Pipeline complete: explored {{ formula }}, job={{ explore.body.job_id }}, model trained"
```

```bash
prism pipeline --formula NiCrCoAlTi --target yield_strength --execute
```

---

## Step Type Summary

| Step | Purpose | OPA check | Context output |
|------|---------|-----------|---------------|
| `set` | Set variables | No | `{{ step_id.key }}` |
| `message` | Display text | No | `{{ step_id.message }}` |
| `http` | Call any API | No | `{{ step_id.body }}`, `{{ step_id.status_code }}` |
| `tool` | Call PRISM tool | **Yes** (per-tool) | `{{ step_id.output }}` |
| `if` | Branch on condition | No | `{{ step_id.branch }}`, `{{ step_id.condition }}` |
| `parallel` | Fan-out concurrent | No | `{{ step_id.completed }}`, `{{ step_id.steps }}` |
| `workflow` | Call sub-workflow | **Yes** (child gets own check) | `{{ step_id.context.* }}`, `{{ step_id.steps }}` |
| `retries` | Retry any step | Inherited | Same as wrapped step |
