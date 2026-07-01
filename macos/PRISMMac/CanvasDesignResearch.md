# PRISM Canvas Design Research

Date: 2026-05-24
Research tool: Exa MCP

## Sources

- OpenAI Codex app features: https://developers.openai.com/codex/app/features
- Introducing the Codex app: https://openai.com/index/introducing-the-codex-app/
- tldraw AI integrations: https://tldraw.dev/docs/ai
- tldraw Workflow starter kit: https://tldraw.dev/starter-kits/workflow
- Miro Intelligent Canvas: https://miro.com/intelligent-canvas/

## Takeaways For PRISM Mac

### Codex App Pattern

Codex is not an IDE clone. It is a command center for supervising parallel
agent work. The relevant patterns for PRISM are:

- project/thread-level supervision;
- collapsible task/sidebar chrome;
- plans, sources, artifacts, and summaries in a side panel;
- permission and sandbox state visible before action;
- terminal/browser/artifact previews as supporting panes;
- long-running work represented as something the user can leave and return to.

For PRISM, the equivalent is supervising materials-research agents, compute
jobs, discourse reviews, workflow runs, and evidence bundles.

### tldraw Pattern

tldraw frames AI canvas apps in three patterns:

- canvas as AI output;
- visual workflows made of connected nodes;
- agents that can read and manipulate the canvas using screenshots plus
  structured shape data.

The Workflow starter kit is especially important: nodes have input/output ports,
connections stay attached as nodes move, and an execution engine resolves
dependencies before running nodes.

For PRISM, every widget should become an executable node with ports, inputs,
outputs, and typed execution state.

### Miro Pattern

Miro's strongest idea is "use your canvas as the prompt." Selected canvas
objects become context for AI generation. Miro also treats docs, diagrams,
tables, prototypes, slides, and widgets as first-class canvas formats rather
than panels hidden elsewhere.

For PRISM, selected widgets should become the prompt context for research,
simulation, discourse, and export actions.

## Design Rules

1. Chat owns the default product experience.
2. Sidebars are collapsible supervision chrome.
3. Widget/node boards are for workflow, simulation, discourse, and evidence
   screens, not the global home.
4. Widgets are not cards; they are executable objects when a board is open.
5. Every executable widget shows state, inputs, outputs, command contract, and
   limits.
6. Connections are provenance and data flow, not decoration.
7. Selected canvas objects become AI prompt context in board mode.
8. LLM limits, credits, approvals, and idempotency are visible before action.
9. ESA demo flow should be readable without opening a terminal.
