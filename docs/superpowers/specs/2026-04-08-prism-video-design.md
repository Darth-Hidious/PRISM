# PRISM Product Video — Design Spec

## Overview

A 90-second cinematic product video showcasing PRISM (Platform for Research in Intelligent Synthesis of Materials), built with Remotion (React-based programmatic video framework). Hybrid cinematic + technical demo approach: dark theme with dramatic reveals, combined with split-screen CLI demonstrations.

**Format:** 1920x1080, 30fps, MP4
**Duration:** ~90 seconds (2700 frames)
**Tool:** Remotion (React + TypeScript)
**Output:** `video/out/prism-showcase.mp4`

## Assets

### Logos
- MARC27 glowing logo: `~/Downloads/KC-UKF/marc27_glow_logo_transparent.png`
- PRISM banner: `docs/assets/prism-banner.png`
- PRISM desktop icons: `data/viking/default/resources/prism-desktop/icons/stable/`

### Color Palette (from PRISM TUI theme + dashboard)
| Token | Hex | Usage in video |
|-------|-----|----------------|
| Background | `#0a0d12` | All scene backgrounds |
| Panel | `#161b22` | CLI terminal backgrounds |
| Warm orange (brand) | `#fab283` | Brand accent, borders, highlights |
| Blue accent | `#5c9cf5` | Links, secondary accent |
| Gold | `#fbbf24` | PRISM title text, primary CTA |
| Success teal | `#4fd6be` | Checkmarks, success states, ingest |
| Lavender | `#c4a7e7` | Query scenes, emphasis |
| Amber | `#f0a058` | Workflow, warm accents |
| Purple | `#8b5cf6` | Stats, tech stack |
| Text primary | `#e6edf3` | Body text |
| Text muted | `#8b98a5` | Secondary text, timestamps |
| Text dim | `#5d6875` | Subtle elements |
| Border | `#30363d` | Borders, dividers |

### Typography
- Titles: Inter (or system sans-serif), weight 700
- CLI text: JetBrains Mono (or system monospace)
- Labels: Inter, weight 400, letter-spacing 2px

## Scene Breakdown

### Scene 1: Cold Open (0–8s, frames 0–240)
- **Visual:** Dark void. Subtle particles drift, then coalesce into a crystal lattice structure (hexagonal/FCC pattern). Particles use warm orange (`#fab283`) with soft glow.
- **Text:** Fades in at frame 90: "MATERIALS DISCOVERY TAKES YEARS." in muted gray (`#8b98a5`). At frame 180: "WHAT IF IT DIDN'T?" in warm orange (`#fab283`).
- **Transition:** Lattice particles scatter outward, revealing Scene 2.

### Scene 2: Title Reveal (8–20s, frames 240–600)
- **Visual:** PRISM banner image scales up from center with a subtle glow bloom effect. MARC27 glowing logo pulses in bottom-right corner.
- **Text sequence (typing effect):**
  1. "PRISM" in gold (`#fbbf24`), large, letter-spaced
  2. "Platform for Research in Intelligent Synthesis of Materials" types out below in muted text
  3. Tags fade in: "AI-Native · Autonomous · Materials Discovery" in blue (`#5c9cf5`)
- **Transition:** Slide left, CLI terminal slides in from right.

### Scene 3: Ingest — "Feed It Everything" (20–35s, frames 600–1050)
- **Layout:** Split screen. Left 50%: animated terminal. Right 50%: knowledge graph visualization.
- **Left panel (CLI):**
  ```
  $ prism ingest --corpus superalloys papers/*.pdf
  > Parsing 847 documents...
  > Extracted 12,431 entities (MAT, PRO, ELM)
  > Ingested to knowledge graph
  ```
  Characters type at ~30 chars/second. Output lines appear with 500ms delays. Checkmark in teal (`#4fd6be`).
- **Right panel (visualization):** Nodes appear as colored dots (MAT=orange, PRO=purple, ELM=teal) and edges draw between them. Counter animates: "211K nodes · 44 corpora".
- **Header text:** "FEED IT EVERYTHING" in teal, top-center.
- **Transition:** Graph expands to fill screen briefly, then contracts to right side for Scene 4.

### Scene 4: Query — "Ask It Anything" (35–48s, frames 1050–1440)
- **Layout:** Split screen, same structure as Scene 3.
- **Left panel (CLI):**
  ```
  $ prism query "high-entropy alloys with oxidation resistance above 1200°C"
  > Searching 20+ federated providers...
  > OPTIMADE · Materials Project · AFLOW · ICSD
  > Found 23 candidates across 4 databases
  ```
- **Right panel (visualization):** Knowledge graph from Scene 3 is still visible. Query triggers paths to light up — nodes along matching routes glow lavender (`#c4a7e7`). Provider logos/names flash briefly as they're queried.
- **Header text:** "ASK IT ANYTHING" in lavender.
- **Transition:** Fade to black, 500ms pause.

### Scene 5: Mesh — "No One Else Has This" (48–60s, frames 1440–1800)
- **Visual:** Dark map/grid. Three glowing points appear at different positions (representing labs). mDNS discovery animation: concentric rings pulse outward from each point. When rings overlap, a connection line draws between peers with a flash.
- **Left panel (CLI):**
  ```
  $ prism mesh discover
  > Scanning local network...
  > Found 3 peers:
    lab-oxford (211K nodes)
    lab-munich (89K nodes)
    lab-tokyo (156K nodes)
  ```
- **Key moment:** After peers connect, a federated query animation shows data flowing between all three nodes simultaneously.
- **Header text:** "FEDERATED MESH" in gold (`#fbbf24`), with subtitle "No one else has this." in muted text.
- **Transition:** Mesh lines converge to center point, morph into pipeline.

### Scene 6: Workflow — "End-to-End Pipelines" (60–72s, frames 1800–2160)
- **Layout:** Split screen.
- **Left panel:** YAML workflow definition scrolls upward slowly:
  ```yaml
  workflow: discovery
  steps:
    - search_materials
    - predict_property
    - calphad_simulate
    - validate_candidate
  ```
- **Right panel:** Horizontal pipeline visualization. Four nodes connected by animated flowing particles:
  - Search (teal `#4fd6be`)
  - Predict (lavender `#c4a7e7`)
  - Simulate (gold `#fbbf24`)
  - Validate (orange `#f0a058`)
  Each node lights up in sequence as if executing. Particles flow from left to right along connection lines.
- **Header text:** "END-TO-END PIPELINES" in orange (`#f0a058`).
- **Transition:** Pipeline nodes scatter outward, numbers fly in.

### Scene 7: The Stack (72–82s, frames 2160–2460)
- **Visual:** Four stat blocks arranged in a 2x2 grid, centered. Each number counts up from 0 using an eased counter animation.
  - 211K — Graph Nodes (orange)
  - 49 — AI Tools (blue)
  - 20+ — Federated Databases (teal)
  - 5 — GPU Providers (lavender)
- **Below stats:** A layered stack diagram fades in showing architecture: Rust CLI → Python Tools → Knowledge Graph → Mesh Network → Cloud Compute.
- **Transition:** Stats and layers fade, logos emerge.

### Scene 8: End Card (82–90s, frames 2460–2700)
- **Visual:** Clean dark background. Elements appear in sequence:
  1. PRISM logo/text in gold, centered, with subtle glow bloom
  2. "by" in muted text
  3. MARC27 glowing logo below, pulsing gently
  4. GitHub URL: `github.com/Darth-Hidious/PRISM` in blue (`#5c9cf5`)
  5. Tagline: "ESA SPARK Prime Contractor · ITER Supplier" in dim text
- **Hold:** Static for final 3 seconds. Music resolves.

## Project Structure

```
video/
  package.json
  tsconfig.json
  remotion.config.ts
  src/
    index.ts              # Remotion entry point
    Root.tsx              # Root composition
    Video.tsx             # Main composition wiring all scenes
    scenes/
      ColdOpen.tsx        # Scene 1
      TitleReveal.tsx     # Scene 2
      Ingest.tsx          # Scene 3
      Query.tsx           # Scene 4
      Mesh.tsx            # Scene 5
      Workflow.tsx        # Scene 6
      TheStack.tsx        # Scene 7
      EndCard.tsx         # Scene 8
    components/
      Terminal.tsx        # Reusable animated CLI terminal
      TypeWriter.tsx      # Typing animation component
      GraphViz.tsx        # Knowledge graph node/edge animation
      ParticleField.tsx   # Background particle system
      Counter.tsx         # Animated number counter
      GlowText.tsx        # Text with glow bloom effect
      Pipeline.tsx        # Workflow pipeline visualization
    theme.ts              # Colors, fonts, shared constants
    assets/               # Copied logos (symlinked or copied at build)
```

## Animations

### Terminal typing
- Characters appear at ~30 chars/second (1 char per frame at 30fps)
- Cursor blinks at 500ms intervals using `interpolate()` with step easing
- Output lines appear with staggered 500ms delays
- Green checkmark animates with scale spring

### Knowledge graph
- Nodes: circles with colored fills, appear with scale-up spring animation
- Edges: lines that draw from source to target using `strokeDashoffset` animation
- Query highlighting: nodes along path pulse with increased opacity + glow

### Particle system
- 50-100 particles per scene, using `useCurrentFrame()` for position updates
- Each particle has: position, velocity, size, color, opacity
- Crystal lattice formation: particles interpolate from random positions to lattice coordinates

### Counters
- Numbers count from 0 to target using `interpolate()` with `Easing.out(Easing.cubic)`
- Format with locale string (commas for thousands)

### Transitions
- Between scenes: 15-frame (500ms) crossfades using opacity interpolation
- Split screen entry: panels slide in from their respective sides
- Scatter effects: elements move outward with random velocities + opacity fade

## Music

Placeholder audio track slot. The composition will include an `<Audio>` component with a configurable source path. User to provide a royalty-free ambient/electronic track. Recommended: 90 seconds, building intensity through middle sections, resolving at end.

## Rendering

```bash
cd video
npm install
npx remotion preview   # Live preview in browser
npx remotion render src/index.ts PrismShowcase out/prism-showcase.mp4
```

## Dependencies

- `remotion` + `@remotion/cli` — core framework
- `@remotion/media-utils` — audio integration
- React 18 + TypeScript
- FFmpeg (auto-downloaded by Remotion on first render)

## End Card CTA

- GitHub: `github.com/Darth-Hidious/PRISM`
- Attribution: "by MARC27"
- Credentials: "ESA SPARK Prime Contractor · ITER Supplier"
