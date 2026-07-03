# PRISM Mac Canvas Widget Architecture

Date: 2026-05-24
Status: first implementation slice

## Decision

PRISM Mac is chat-first. The primary surface should feel closer to ChatGPT on
macOS: calm conversation, launcher-style composer, visible attached context,
and collapsible supervision chrome.

Canvas widgets still matter, but they belong where spatial execution is useful:
workflow, simulation, discourse, and evidence-bundle screens. Sidebars,
inspectors, settings, and command runners are supporting chrome around the
current task, not the main product shape.

## Canvas Objects

- `Question`: user intent and top-level research prompt.
- `Constraints`: mission, material, cost, model, and approval bounds.
- `Knowledge`: MARC27 graph and semantic search.
- `Papers`: DOI/corpus retrieval and evidence extraction.
- `Materials`: candidate comparison table.
- `Model Limits`: visible LLM context, output, tool-call, quota, and budget caps.
- `Discourse`: multi-agent critique and open-question generation.
- `Workflow`: parameterized YAML/PRISM workflow run.
- `Simulation`: compute job with budget and approval guardrails.
- `Evidence Bundle`: final export surface for ESA review.
- `Billing Guardrail`: read-only usage and top-up gating.
- `Fabric Node`: local node, mesh, and federation status.

## UI Shape

The default Chat mode has three regions:

1. Threads sidebar: recent work and pinned context.
2. Chat surface: conversation, context banner, and bottom composer.
3. Context inspector: app connections, limits, artifacts, and board handoff.

The Workflow Board mode has three regions:

1. Widget palette: add new object types to the board.
2. Infinite-workbench placeholder: draggable widgets and provenance edges.
3. Inspector: selected-widget state, inputs, outputs, command contract, and
   actions.

Side panels are collapsible supervision chrome. The chat surface must remain the
visual center of gravity until the user explicitly enters a board/workflow
screen.

This first slice is a native SwiftUI canvas prototype. A later slice can replace
or augment the canvas engine with tldraw/WebKit if collaboration, multiplayer,
or more advanced geometry becomes the priority.

## Visual References

- Codex app: agent command center pattern; supervise parallel agent work without
  turning the whole product into an IDE.
- tldraw Computer: connected components that generate and transform data.
- tldraw AI integrations: three useful AI canvas modes — generated outputs,
  visual workflows, and agents that read/write the canvas.
- Miro AI Canvas: AI creation controls live directly on the board, producing
  docs, diagrams, tables, prototypes, slides, and media as canvas objects.

See `CanvasDesignResearch.md` for the Exa-sourced research notes.
See `PRISMDomainModel.md` for the materials-science/product domain model.

## ESA Demo Goal

The reviewer should see PRISM assemble an auditable research workflow spatially:

`Question -> Knowledge/Papers -> Candidate Table -> Discourse -> Workflow ->
Simulation -> Evidence Bundle`

The important part is not that PRISM has a menu item for every CLI feature. The
important part is that each capability becomes a visible, connected, inspectable
object with inputs, outputs, costs, limits, and provenance.
