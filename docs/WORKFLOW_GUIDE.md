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

| Mode | `set` steps | `message` steps | `http` steps | `tool` steps |
|------|-------------|-----------------|--------------|--------------|
| `dry_run` | Context updated, status=`planned` | Text rendered, status=`planned` | URL shown but **not called**, status=`planned` | Tool shown but **not called**, status=`planned` |
| `execute` | Context updated, status=`completed` | Text rendered, status=`completed` | HTTP call made, response stored, status=`completed` | Tool called via node API, response stored, status=`completed` |

### Error handling

- If an `http` step returns a status not in `expect_status`, the workflow **aborts**
- If a `tool` step returns HTTP 4xx/5xx, the workflow **aborts**
- If a template variable doesn't exist in context, the workflow **aborts** with `unknown workflow context path`
- If a required argument is missing, the workflow **aborts** before any step runs
- OPA deny → workflow **aborts** with the deny message

### Context lifetime

Context lives for the duration of the workflow run. Each step adds to it:

```
Initial context (args + env + defaults + builtins)
  └─ step 1 output added
       └─ step 2 output added
            └─ step 3 can read from step 1, step 2, and all args
```

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
