# PRISM Mac Design Language

Date: 2026-05-24
Status: active product direction

## References

- Apple HIG - Sidebars: https://developer.apple.com/design/Human-Interface-Guidelines/sidebars
- Apple HIG - Toolbars: https://developer.apple.com/design/human-interface-guidelines/toolbars
- Apple HIG - Materials: https://developer.apple.com/design/human-interface-guidelines/materials
- Apple HIG - Typography: https://developer.apple.com/design/human-interface-guidelines/typography
- Apple HIG - Color: https://developer.apple.com/design/Human-Interface-Guidelines/color
- Apple Developer - Adopting Liquid Glass: https://developer.apple.com/documentation/TechnologyOverviews/adopting-liquid-glass
- Apple Developer - SwiftUI glassEffect: https://developer.apple.com/documentation/swiftui/view/glasseffect(_:in:)
- Apple Newsroom - Liquid Glass announcement: https://www.apple.com/newsroom/2025/06/apple-introduces-a-delightful-and-elegant-new-software-design/
- ChatGPT macOS - Chat Bar: https://help.openai.com/en/articles/9295241-accessing-the-launcher-chatgpt-macos-app
- ChatGPT macOS - Work with Apps: https://help.openai.com/en/articles/10119604-work-with-apps-on-macos
- Raycast - Search Bar: https://manual.raycast.com/search-bar
- Linear - Conceptual model/actions: https://linear.app/docs/conceptual-model

## North Star

PRISM Mac should feel like a native Mac scientific command surface:

- calm like ChatGPT on Mac;
- keyboard-first like Raycast and Linear;
- system-native like Apple HIG;
- Liquid Glass as a functional control layer, not decoration;
- evidence/provenance-aware like PRISM/MARC27;
- spatial only when a workflow actually needs spatial execution.

## Principles

### 1. Chat Is The Home

The main state is a centered conversation with a launcher-like composer. It
should never look like a dashboard grid by default.

Default chrome:

- collapsed sidebars;
- centered chat column;
- bottom composer;
- visible context banner;
- minimal toolbar.

### 2. Sidebars Are Optional Chrome

Apple's sidebar guidance: sidebars need space and should collapse when the app
needs to devote room to content. PRISM follows that. Sidebars are for threads,
context, and workflow tools, not decoration.

### 3. One Input Runs The App

Raycast's search bar pattern matters: everything starts from one input. PRISM's
composer should be the equivalent surface for:

- asking;
- attaching files/screenshots;
- selecting app context;
- invoking workflow branch actions;
- running command-style tasks.

### 4. Actions Are Consistent Everywhere

Linear's model is strong: the same action should be available by visible button,
context menu, shortcut, and command palette. PRISM should use that pattern for:

- Run;
- Branch to workflow;
- Export bundle;
- Add evidence;
- Approve;
- Reject;
- Open artifact.

### 5. Color Is Semantic And Sparse

Use system colors, semantic foregrounds, and one quiet PRISM accent. Avoid
rainbow dashboards. Node type colors are allowed only in workflow-board mode
where they encode data-flow types.

### 6. Typography Carries Hierarchy

Apple's typography guidance is the baseline: system font, readable sizes,
Regular/Medium/Semibold, few type styles, no decorative type.

### 7. Boards Are For Execution

Node boards belong to workflow, simulation, discourse, and evidence screens.
They are not the home screen.

## Visual Tokens

- Window background: system window background.
- Panel material: system sidebar/toolbars first, `glassEffect` for custom floating controls on macOS 26, `.regularMaterial` fallback.
- Composer material: Liquid Glass, interactive, rounded like a command surface.
- Radius: 8 maximum for cards and widgets; capsule-like radius only for command surfaces.
- Main column max width: 820.
- Sidebar width: 252.
- Inspector width: 300.
- Accent: system accent / blue, used sparingly.
- Status: green success, orange waiting, red gated/error.

## Anti-Patterns

- No default dashboard grid.
- No decorative gradient/orb backgrounds.
- No fake wallpaper just to make glass visible.
- No oversized cards for every concept.
- No rainbow widgets in chat mode.
- No node editor unless the user is actually editing/running a workflow.
- No visible credentials or secret-looking strings.
- No Stripe top-up controls in the ESA demo flow until billing is hardened.
