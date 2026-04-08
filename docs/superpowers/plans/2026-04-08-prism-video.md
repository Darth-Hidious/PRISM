# PRISM Product Video Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a 90-second cinematic product video showcasing PRISM, rendered with Remotion (React).

**Architecture:** 8 scene components composed via `<Sequence>` in a root `Video.tsx`. Shared animation components (Terminal, TypeWriter, ParticleField, etc.) are reused across scenes. All styling inline — no CSS framework needed.

**Tech Stack:** Remotion 4, React 18, TypeScript, FFmpeg (auto-downloaded by Remotion)

**Spec:** `docs/superpowers/specs/2026-04-08-prism-video-design.md`

---

### Task 1: Scaffold Remotion Project

**Files:**
- Create: `video/package.json`
- Create: `video/tsconfig.json`
- Create: `video/remotion.config.ts`
- Create: `video/src/index.ts`
- Create: `video/src/Root.tsx`
- Create: `video/src/Video.tsx`
- Create: `video/src/theme.ts`

- [ ] **Step 1: Create package.json**

```json
{
  "name": "prism-video",
  "version": "1.0.0",
  "private": true,
  "scripts": {
    "preview": "remotion studio",
    "render": "remotion render src/index.ts PrismShowcase out/prism-showcase.mp4",
    "render:fast": "remotion render src/index.ts PrismShowcase out/prism-showcase.mp4 --concurrency=4"
  },
  "dependencies": {
    "react": "^18.3.1",
    "react-dom": "^18.3.1",
    "remotion": "^4.0.0",
    "@remotion/cli": "^4.0.0"
  },
  "devDependencies": {
    "typescript": "^5.5.0",
    "@types/react": "^18.3.0"
  }
}
```

- [ ] **Step 2: Create tsconfig.json**

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ES2022",
    "moduleResolution": "bundler",
    "jsx": "react-jsx",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "outDir": "dist",
    "rootDir": "src"
  },
  "include": ["src"]
}
```

- [ ] **Step 3: Create remotion.config.ts**

```ts
import {Config} from '@remotion/cli/config';

Config.setVideoImageFormat('jpeg');
Config.setOverwriteOutput(true);
```

- [ ] **Step 4: Create theme.ts**

```ts
export const COLORS = {
  bg: '#0a0d12',
  panel: '#161b22',
  brand: '#fab283',
  blue: '#5c9cf5',
  gold: '#fbbf24',
  teal: '#4fd6be',
  lavender: '#c4a7e7',
  amber: '#f0a058',
  purple: '#8b5cf6',
  text: '#e6edf3',
  textMuted: '#8b98a5',
  textDim: '#5d6875',
  border: '#30363d',
} as const;

export const FONTS = {
  title: 'Inter, system-ui, -apple-system, sans-serif',
  mono: '"JetBrains Mono", "Fira Code", "SF Mono", monospace',
} as const;

// Scene frame boundaries (at 30fps)
export const SCENES = {
  coldOpen:     { from: 0,    duration: 240  },  // 0-8s
  titleReveal:  { from: 240,  duration: 360  },  // 8-20s
  ingest:       { from: 600,  duration: 450  },  // 20-35s
  query:        { from: 1050, duration: 390  },  // 35-48s
  mesh:         { from: 1440, duration: 360  },  // 48-60s
  workflow:     { from: 1800, duration: 360  },  // 60-72s
  theStack:     { from: 2160, duration: 300  },  // 72-82s
  endCard:      { from: 2460, duration: 240  },  // 82-90s
} as const;

export const VIDEO = {
  width: 1920,
  height: 1080,
  fps: 30,
  totalFrames: 2700,
} as const;
```

- [ ] **Step 5: Create Root.tsx**

```tsx
import {Composition} from 'remotion';
import {PrismShowcase} from './Video';
import {VIDEO} from './theme';

export const RemotionRoot: React.FC = () => {
  return (
    <Composition
      id="PrismShowcase"
      component={PrismShowcase}
      durationInFrames={VIDEO.totalFrames}
      fps={VIDEO.fps}
      width={VIDEO.width}
      height={VIDEO.height}
    />
  );
};
```

- [ ] **Step 6: Create index.ts**

```ts
import {registerRoot} from 'remotion';
import {RemotionRoot} from './Root';

registerRoot(RemotionRoot);
```

- [ ] **Step 7: Create placeholder Video.tsx**

```tsx
import {AbsoluteFill} from 'remotion';
import {COLORS, FONTS} from './theme';

export const PrismShowcase: React.FC = () => {
  return (
    <AbsoluteFill
      style={{
        backgroundColor: COLORS.bg,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
      }}
    >
      <div style={{color: COLORS.gold, fontSize: 72, fontFamily: FONTS.title, fontWeight: 700, letterSpacing: 8}}>
        PRISM
      </div>
    </AbsoluteFill>
  );
};
```

- [ ] **Step 8: Copy logo assets to public/**

```bash
mkdir -p video/public
cp ~/Downloads/KC-UKF/marc27_glow_logo_transparent.png video/public/marc27-logo.png
cp docs/assets/prism-banner.png video/public/prism-banner.png
```

- [ ] **Step 9: Install and verify preview**

```bash
cd video && npm install
npx remotion studio
```

Expected: Browser opens showing "PRISM" in gold on dark background.

- [ ] **Step 10: Commit**

```bash
git add video/
git commit -m "feat(video): scaffold Remotion project with theme and assets"
```

---

### Task 2: Core Animation Components

**Files:**
- Create: `video/src/components/GlowText.tsx`
- Create: `video/src/components/TypeWriter.tsx`
- Create: `video/src/components/Counter.tsx`
- Create: `video/src/components/ParticleField.tsx`

- [ ] **Step 1: Create GlowText component**

```tsx
import React from 'react';
import {interpolate, useCurrentFrame} from 'remotion';
import {FONTS} from '../theme';

interface GlowTextProps {
  text: string;
  color: string;
  fontSize?: number;
  fontFamily?: string;
  letterSpacing?: number;
  fadeInStart?: number;
  fadeInDuration?: number;
  glowRadius?: number;
}

export const GlowText: React.FC<GlowTextProps> = ({
  text,
  color,
  fontSize = 48,
  fontFamily = FONTS.title,
  letterSpacing = 4,
  fadeInStart = 0,
  fadeInDuration = 20,
  glowRadius = 30,
}) => {
  const frame = useCurrentFrame();
  const opacity = interpolate(frame, [fadeInStart, fadeInStart + fadeInDuration], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  return (
    <div
      style={{
        color,
        fontSize,
        fontFamily,
        fontWeight: 700,
        letterSpacing,
        opacity,
        textShadow: `0 0 ${glowRadius}px ${color}40, 0 0 ${glowRadius * 2}px ${color}20`,
      }}
    >
      {text}
    </div>
  );
};
```

- [ ] **Step 2: Create TypeWriter component**

```tsx
import React from 'react';
import {interpolate, useCurrentFrame} from 'remotion';
import {COLORS, FONTS} from '../theme';

interface TypeWriterProps {
  text: string;
  startFrame?: number;
  charsPerFrame?: number;
  color?: string;
  fontSize?: number;
  showCursor?: boolean;
}

export const TypeWriter: React.FC<TypeWriterProps> = ({
  text,
  startFrame = 0,
  charsPerFrame = 1,
  color = COLORS.text,
  fontSize = 18,
  showCursor = true,
}) => {
  const frame = useCurrentFrame();
  const elapsed = Math.max(0, frame - startFrame);
  const charsToShow = Math.min(Math.floor(elapsed * charsPerFrame), text.length);
  const displayText = text.slice(0, charsToShow);
  const isComplete = charsToShow >= text.length;

  // Cursor blinks every 15 frames (500ms at 30fps)
  const cursorOpacity = showCursor
    ? isComplete
      ? Math.floor(frame / 15) % 2 === 0 ? 1 : 0
      : 1
    : 0;

  return (
    <span style={{fontFamily: FONTS.mono, fontSize, color}}>
      {displayText}
      <span style={{opacity: cursorOpacity, color: COLORS.brand}}>▌</span>
    </span>
  );
};
```

- [ ] **Step 3: Create Counter component**

```tsx
import React from 'react';
import {Easing, interpolate, useCurrentFrame} from 'remotion';

interface CounterProps {
  target: number;
  suffix?: string;
  color: string;
  fontSize?: number;
  startFrame?: number;
  duration?: number;
  fontFamily?: string;
}

export const Counter: React.FC<CounterProps> = ({
  target,
  suffix = '',
  color,
  fontSize = 64,
  startFrame = 0,
  duration = 60,
  fontFamily = 'Inter, system-ui, sans-serif',
}) => {
  const frame = useCurrentFrame();
  const progress = interpolate(frame, [startFrame, startFrame + duration], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
    easing: Easing.out(Easing.cubic),
  });
  const value = Math.round(target * progress);
  const display = target >= 1000 ? `${Math.round(value / 1000)}K` : `${value}`;

  return (
    <span style={{color, fontSize, fontWeight: 700, fontFamily}}>
      {display}{suffix}
    </span>
  );
};
```

- [ ] **Step 4: Create ParticleField component**

```tsx
import React, {useMemo} from 'react';
import {AbsoluteFill, interpolate, useCurrentFrame} from 'remotion';

interface Particle {
  id: number;
  x: number;
  y: number;
  vx: number;
  vy: number;
  size: number;
  opacity: number;
}

// Deterministic pseudo-random for consistent renders
function seededRandom(seed: number): number {
  const x = Math.sin(seed * 127.1 + 311.7) * 43758.5453;
  return x - Math.floor(x);
}

interface ParticleFieldProps {
  count?: number;
  color?: string;
  seed?: number;
  speed?: number;
  converge?: boolean;
  convergeFrame?: number;
  convergeDuration?: number;
}

export const ParticleField: React.FC<ParticleFieldProps> = ({
  count = 60,
  color = '#fab283',
  seed = 42,
  speed = 0.5,
  converge = false,
  convergeFrame = 60,
  convergeDuration = 60,
}) => {
  const frame = useCurrentFrame();

  const particles = useMemo(() => {
    const p: Particle[] = [];
    for (let i = 0; i < count; i++) {
      p.push({
        id: i,
        x: seededRandom(seed + i * 3) * 1920,
        y: seededRandom(seed + i * 3 + 1) * 1080,
        vx: (seededRandom(seed + i * 3 + 2) - 0.5) * speed,
        vy: (seededRandom(seed + i * 3 + 3) - 0.5) * speed,
        size: 2 + seededRandom(seed + i * 3 + 4) * 4,
        opacity: 0.3 + seededRandom(seed + i * 3 + 5) * 0.7,
      });
    }
    return p;
  }, [count, seed, speed]);

  // Hexagonal lattice target positions (for crystal formation)
  const latticeTargets = useMemo(() => {
    const targets: {x: number; y: number}[] = [];
    const cols = Math.ceil(Math.sqrt(count * 1.5));
    const rows = Math.ceil(count / cols);
    const spacingX = 40;
    const spacingY = 35;
    const offsetX = 960 - (cols * spacingX) / 2;
    const offsetY = 540 - (rows * spacingY) / 2;
    for (let i = 0; i < count; i++) {
      const row = Math.floor(i / cols);
      const col = i % cols;
      targets.push({
        x: offsetX + col * spacingX + (row % 2 === 1 ? spacingX / 2 : 0),
        y: offsetY + row * spacingY,
      });
    }
    return targets;
  }, [count]);

  return (
    <AbsoluteFill>
      {particles.map((p, i) => {
        let x = p.x + p.vx * frame;
        let y = p.y + p.vy * frame;

        if (converge) {
          const t = interpolate(frame, [convergeFrame, convergeFrame + convergeDuration], [0, 1], {
            extrapolateLeft: 'clamp',
            extrapolateRight: 'clamp',
          });
          const target = latticeTargets[i] || {x: 960, y: 540};
          x = x * (1 - t) + target.x * t;
          y = y * (1 - t) + target.y * t;
        }

        // Wrap around screen
        x = ((x % 1920) + 1920) % 1920;
        y = ((y % 1080) + 1080) % 1080;

        return (
          <div
            key={p.id}
            style={{
              position: 'absolute',
              left: x,
              top: y,
              width: p.size,
              height: p.size,
              borderRadius: '50%',
              backgroundColor: color,
              opacity: p.opacity,
              boxShadow: `0 0 ${p.size * 2}px ${color}80`,
            }}
          />
        );
      })}
    </AbsoluteFill>
  );
};
```

- [ ] **Step 5: Preview components in Video.tsx**

Update `video/src/Video.tsx` temporarily to verify components render:

```tsx
import {AbsoluteFill, Sequence} from 'remotion';
import {COLORS} from './theme';
import {GlowText} from './components/GlowText';
import {TypeWriter} from './components/TypeWriter';
import {Counter} from './components/Counter';
import {ParticleField} from './components/ParticleField';

export const PrismShowcase: React.FC = () => {
  return (
    <AbsoluteFill style={{backgroundColor: COLORS.bg}}>
      <ParticleField converge convergeFrame={30} convergeDuration={60} />
      <AbsoluteFill style={{display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', gap: 20}}>
        <GlowText text="PRISM" color={COLORS.gold} fontSize={72} />
        <Sequence from={30}>
          <TypeWriter text="Platform for Research in Intelligent Synthesis of Materials" color={COLORS.textMuted} />
        </Sequence>
        <Sequence from={90}>
          <Counter target={211000} color={COLORS.brand} suffix="" startFrame={0} duration={60} />
        </Sequence>
      </AbsoluteFill>
    </AbsoluteFill>
  );
};
```

Run: `cd video && npx remotion studio`

Expected: Dark background with particles converging into lattice, "PRISM" glowing in gold, tagline typing out, counter animating to 211K.

- [ ] **Step 6: Commit**

```bash
git add video/src/components/
git commit -m "feat(video): add core animation components — GlowText, TypeWriter, Counter, ParticleField"
```

---

### Task 3: Terminal Component

**Files:**
- Create: `video/src/components/Terminal.tsx`

- [ ] **Step 1: Create Terminal component**

```tsx
import React from 'react';
import {interpolate, spring, useCurrentFrame, useVideoConfig} from 'remotion';
import {COLORS, FONTS} from '../theme';

interface TerminalLine {
  text: string;
  color?: string;
  prefix?: string;
  delay?: number; // frames after previous line completes
}

interface TerminalProps {
  command: string;
  output: TerminalLine[];
  startFrame?: number;
  typingSpeed?: number; // chars per frame
  width?: number | string;
}

export const Terminal: React.FC<TerminalProps> = ({
  command,
  output,
  startFrame = 0,
  typingSpeed = 1,
  width = '100%',
}) => {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();
  const elapsed = Math.max(0, frame - startFrame);

  // Phase 1: Type the command
  const commandChars = Math.min(Math.floor(elapsed * typingSpeed), command.length);
  const commandDone = commandChars >= command.length;
  const commandFrames = Math.ceil(command.length / typingSpeed);

  // Phase 2: Output lines appear with delays
  let outputFrameOffset = commandFrames + 15; // 500ms pause after command
  const visibleLines: {text: string; color: string; opacity: number}[] = [];

  for (const line of output) {
    const lineDelay = line.delay ?? 15;
    const lineStart = outputFrameOffset;
    const lineOpacity = interpolate(elapsed, [lineStart, lineStart + 8], [0, 1], {
      extrapolateLeft: 'clamp',
      extrapolateRight: 'clamp',
    });

    if (elapsed >= lineStart) {
      visibleLines.push({
        text: `${line.prefix ?? '▸'} ${line.text}`,
        color: line.color ?? COLORS.textMuted,
        opacity: lineOpacity,
      });
    }

    outputFrameOffset = lineStart + lineDelay;
  }

  // Cursor blink
  const cursorOpacity = commandDone
    ? Math.floor(frame / 15) % 2 === 0 ? 0.8 : 0
    : 1;

  // Entry animation
  const scale = spring({frame: elapsed, fps, config: {damping: 15, stiffness: 80}});

  return (
    <div
      style={{
        width,
        transform: `scale(${scale})`,
        backgroundColor: COLORS.panel,
        borderRadius: 12,
        border: `1px solid ${COLORS.border}`,
        overflow: 'hidden',
        fontFamily: FONTS.mono,
        fontSize: 16,
      }}
    >
      {/* Title bar */}
      <div style={{
        display: 'flex',
        alignItems: 'center',
        gap: 8,
        padding: '10px 16px',
        borderBottom: `1px solid ${COLORS.border}`,
      }}>
        <div style={{width: 12, height: 12, borderRadius: '50%', backgroundColor: '#ff5f57'}} />
        <div style={{width: 12, height: 12, borderRadius: '50%', backgroundColor: '#febc2e'}} />
        <div style={{width: 12, height: 12, borderRadius: '50%', backgroundColor: '#28c840'}} />
        <span style={{marginLeft: 12, color: COLORS.textDim, fontSize: 13}}>prism</span>
      </div>
      {/* Content */}
      <div style={{padding: '16px 20px', lineHeight: 1.8}}>
        {/* Command line */}
        <div>
          <span style={{color: COLORS.teal}}>$ </span>
          <span style={{color: COLORS.text}}>{command.slice(0, commandChars)}</span>
          <span style={{opacity: cursorOpacity, color: COLORS.brand}}>▌</span>
        </div>
        {/* Output */}
        {visibleLines.map((line, i) => (
          <div key={i} style={{color: line.color, opacity: line.opacity}}>
            {line.text}
          </div>
        ))}
      </div>
    </div>
  );
};
```

- [ ] **Step 2: Verify in preview**

Temporarily add to Video.tsx:

```tsx
<Terminal
  command='prism ingest --corpus superalloys papers/*.pdf'
  output={[
    {text: 'Parsing 847 documents...'},
    {text: 'Extracted 12,431 entities (MAT, PRO, ELM)'},
    {text: 'Ingested to knowledge graph', color: COLORS.teal, prefix: '✓'},
  ]}
/>
```

Run: `cd video && npx remotion studio`

Expected: Terminal window appears with typing animation, output lines fade in sequentially.

- [ ] **Step 3: Commit**

```bash
git add video/src/components/Terminal.tsx
git commit -m "feat(video): add animated Terminal component"
```

---

### Task 4: GraphViz and Pipeline Components

**Files:**
- Create: `video/src/components/GraphViz.tsx`
- Create: `video/src/components/Pipeline.tsx`

- [ ] **Step 1: Create GraphViz component**

```tsx
import React, {useMemo} from 'react';
import {interpolate, spring, useCurrentFrame, useVideoConfig} from 'remotion';
import {COLORS} from '../theme';

interface GraphNode {
  id: string;
  x: number;
  y: number;
  color: string;
  label?: string;
  delay: number; // frame delay before appearing
}

interface GraphEdge {
  from: string;
  to: string;
  delay: number;
}

function seededRandom(seed: number): number {
  const x = Math.sin(seed * 127.1 + 311.7) * 43758.5453;
  return x - Math.floor(x);
}

interface GraphVizProps {
  nodeCount?: number;
  edgeCount?: number;
  highlightStart?: number; // frame to start highlighting paths
  highlightColor?: string;
  seed?: number;
  width?: number;
  height?: number;
}

export const GraphViz: React.FC<GraphVizProps> = ({
  nodeCount = 30,
  edgeCount = 40,
  highlightStart,
  highlightColor = COLORS.lavender,
  seed = 42,
  width = 800,
  height = 600,
}) => {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();

  const nodes = useMemo(() => {
    const n: GraphNode[] = [];
    const colors = [COLORS.brand, COLORS.purple, COLORS.teal, COLORS.blue, COLORS.lavender];
    for (let i = 0; i < nodeCount; i++) {
      n.push({
        id: `n${i}`,
        x: 60 + seededRandom(seed + i * 2) * (width - 120),
        y: 60 + seededRandom(seed + i * 2 + 1) * (height - 120),
        color: colors[i % colors.length],
        delay: Math.floor(i * 2),
      });
    }
    return n;
  }, [nodeCount, seed, width, height]);

  const edges = useMemo(() => {
    const e: GraphEdge[] = [];
    for (let i = 0; i < edgeCount; i++) {
      const fromIdx = Math.floor(seededRandom(seed + 100 + i * 2) * nodeCount);
      const toIdx = Math.floor(seededRandom(seed + 100 + i * 2 + 1) * nodeCount);
      if (fromIdx !== toIdx) {
        e.push({
          from: `n${fromIdx}`,
          to: `n${toIdx}`,
          delay: Math.max(nodes[fromIdx].delay, nodes[toIdx].delay) + 5,
        });
      }
    }
    return e;
  }, [edgeCount, seed, nodeCount, nodes]);

  const nodeMap = useMemo(() => {
    const m: Record<string, GraphNode> = {};
    for (const n of nodes) m[n.id] = n;
    return m;
  }, [nodes]);

  return (
    <div style={{position: 'relative', width, height}}>
      {/* Edges */}
      <svg style={{position: 'absolute', inset: 0}} viewBox={`0 0 ${width} ${height}`}>
        {edges.map((e, i) => {
          const fromNode = nodeMap[e.from];
          const toNode = nodeMap[e.to];
          if (!fromNode || !toNode) return null;

          const progress = interpolate(frame, [e.delay, e.delay + 20], [0, 1], {
            extrapolateLeft: 'clamp',
            extrapolateRight: 'clamp',
          });

          const isHighlighted = highlightStart !== undefined && frame > highlightStart && i % 4 === 0;
          const strokeColor = isHighlighted ? highlightColor : COLORS.border;

          return (
            <line
              key={i}
              x1={fromNode.x}
              y1={fromNode.y}
              x2={fromNode.x + (toNode.x - fromNode.x) * progress}
              y2={fromNode.y + (toNode.y - fromNode.y) * progress}
              stroke={strokeColor}
              strokeWidth={isHighlighted ? 2 : 1}
              opacity={isHighlighted ? 0.9 : 0.3}
            />
          );
        })}
      </svg>
      {/* Nodes */}
      {nodes.map((n) => {
        const s = spring({frame: Math.max(0, frame - n.delay), fps, config: {damping: 12, stiffness: 100}});
        const isHighlighted = highlightStart !== undefined && frame > highlightStart && parseInt(n.id.slice(1)) % 3 === 0;

        return (
          <div
            key={n.id}
            style={{
              position: 'absolute',
              left: n.x - 6,
              top: n.y - 6,
              width: 12,
              height: 12,
              borderRadius: '50%',
              backgroundColor: isHighlighted ? highlightColor : n.color,
              transform: `scale(${s * (isHighlighted ? 1.5 : 1)})`,
              boxShadow: isHighlighted
                ? `0 0 12px ${highlightColor}80`
                : `0 0 8px ${n.color}40`,
            }}
          />
        );
      })}
    </div>
  );
};
```

- [ ] **Step 2: Create Pipeline component**

```tsx
import React from 'react';
import {interpolate, spring, useCurrentFrame, useVideoConfig} from 'remotion';
import {COLORS, FONTS} from '../theme';

interface PipelineStep {
  label: string;
  color: string;
  activateAt: number; // frame when this step "lights up"
}

interface PipelineProps {
  steps: PipelineStep[];
  width?: number;
}

export const Pipeline: React.FC<PipelineProps> = ({steps, width = 700}) => {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();
  const stepWidth = width / steps.length;

  return (
    <div style={{display: 'flex', alignItems: 'center', width, gap: 0}}>
      {steps.map((step, i) => {
        const isActive = frame >= step.activateAt;
        const scale = isActive
          ? spring({frame: Math.max(0, frame - step.activateAt), fps, config: {damping: 12}})
          : 0.6;
        const opacity = isActive ? 1 : 0.3;

        return (
          <React.Fragment key={i}>
            {/* Node */}
            <div style={{display: 'flex', flexDirection: 'column', alignItems: 'center', width: stepWidth}}>
              <div
                style={{
                  width: 48,
                  height: 48,
                  borderRadius: 12,
                  backgroundColor: step.color,
                  opacity,
                  transform: `scale(${scale})`,
                  display: 'flex',
                  alignItems: 'center',
                  justifyContent: 'center',
                  boxShadow: isActive ? `0 0 20px ${step.color}60` : 'none',
                }}
              >
                <div style={{width: 16, height: 16, borderRadius: '50%', backgroundColor: 'white', opacity: 0.9}} />
              </div>
              <span style={{
                marginTop: 10,
                color: isActive ? COLORS.text : COLORS.textDim,
                fontSize: 13,
                fontFamily: FONTS.mono,
                fontWeight: isActive ? 600 : 400,
              }}>
                {step.label}
              </span>
            </div>
            {/* Connector */}
            {i < steps.length - 1 && (
              <div style={{flex: 1, height: 2, position: 'relative', marginTop: -24}}>
                {(() => {
                  const nextActive = frame >= steps[i + 1].activateAt;
                  const connProgress = nextActive
                    ? 1
                    : isActive
                    ? interpolate(
                        frame,
                        [step.activateAt, steps[i + 1].activateAt],
                        [0, 1],
                        {extrapolateLeft: 'clamp', extrapolateRight: 'clamp'}
                      )
                    : 0;
                  return (
                    <>
                      <div style={{width: '100%', height: 2, backgroundColor: COLORS.border}} />
                      <div style={{
                        position: 'absolute',
                        top: 0,
                        left: 0,
                        width: `${connProgress * 100}%`,
                        height: 2,
                        backgroundColor: step.color,
                        boxShadow: `0 0 8px ${step.color}80`,
                      }} />
                    </>
                  );
                })()}
              </div>
            )}
          </React.Fragment>
        );
      })}
    </div>
  );
};
```

- [ ] **Step 3: Commit**

```bash
git add video/src/components/GraphViz.tsx video/src/components/Pipeline.tsx
git commit -m "feat(video): add GraphViz and Pipeline visualization components"
```

---

### Task 5: Scene 1 — Cold Open

**Files:**
- Create: `video/src/scenes/ColdOpen.tsx`

- [ ] **Step 1: Create ColdOpen scene**

```tsx
import React from 'react';
import {AbsoluteFill, interpolate, useCurrentFrame} from 'remotion';
import {COLORS, FONTS} from '../theme';
import {ParticleField} from '../components/ParticleField';

export const ColdOpen: React.FC = () => {
  const frame = useCurrentFrame();

  // Line 1: "MATERIALS DISCOVERY TAKES YEARS." — fades in at frame 60
  const line1Opacity = interpolate(frame, [60, 90], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  // Line 2: "WHAT IF IT DIDN'T?" — fades in at frame 140
  const line2Opacity = interpolate(frame, [140, 170], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  // Exit: everything fades out in last 30 frames
  const exitOpacity = interpolate(frame, [210, 240], [1, 0], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  return (
    <AbsoluteFill style={{backgroundColor: COLORS.bg}}>
      <ParticleField
        count={80}
        color={COLORS.brand}
        speed={0.3}
        converge
        convergeFrame={30}
        convergeDuration={90}
      />
      <AbsoluteFill
        style={{
          display: 'flex',
          flexDirection: 'column',
          alignItems: 'center',
          justifyContent: 'center',
          gap: 24,
          opacity: exitOpacity,
        }}
      >
        <div style={{
          color: COLORS.textMuted,
          fontSize: 28,
          fontFamily: FONTS.title,
          fontWeight: 400,
          letterSpacing: 3,
          opacity: line1Opacity,
          textAlign: 'center',
        }}>
          MATERIALS DISCOVERY TAKES YEARS.
        </div>
        <div style={{
          color: COLORS.brand,
          fontSize: 32,
          fontFamily: FONTS.title,
          fontWeight: 700,
          letterSpacing: 3,
          opacity: line2Opacity,
          textAlign: 'center',
        }}>
          WHAT IF IT DIDN'T?
        </div>
      </AbsoluteFill>
    </AbsoluteFill>
  );
};
```

- [ ] **Step 2: Commit**

```bash
git add video/src/scenes/ColdOpen.tsx
git commit -m "feat(video): add Scene 1 — Cold Open with particle lattice"
```

---

### Task 6: Scene 2 — Title Reveal

**Files:**
- Create: `video/src/scenes/TitleReveal.tsx`

- [ ] **Step 1: Create TitleReveal scene**

```tsx
import React from 'react';
import {AbsoluteFill, Img, interpolate, spring, staticFile, useCurrentFrame, useVideoConfig} from 'remotion';
import {COLORS, FONTS} from '../theme';
import {TypeWriter} from '../components/TypeWriter';
import {GlowText} from '../components/GlowText';

export const TitleReveal: React.FC = () => {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();

  // PRISM title scale-up
  const titleScale = spring({frame, fps, config: {damping: 14, stiffness: 60}});

  // Tags fade in
  const tagsOpacity = interpolate(frame, [180, 220], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  // MARC27 logo pulse
  const logoPulse = interpolate(frame, [0, 60, 120, 180, 240, 300, 360], [0, 0, 0.7, 0.9, 1, 0.9, 1], {
    extrapolateRight: 'clamp',
  });

  // Exit slide
  const exitX = interpolate(frame, [320, 360], [0, -100], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  return (
    <AbsoluteFill style={{backgroundColor: COLORS.bg}}>
      <AbsoluteFill
        style={{
          display: 'flex',
          flexDirection: 'column',
          alignItems: 'center',
          justifyContent: 'center',
          gap: 16,
          transform: `translateX(${exitX}%)`,
        }}
      >
        {/* PRISM title */}
        <div style={{transform: `scale(${titleScale})`}}>
          <GlowText
            text="PRISM"
            color={COLORS.gold}
            fontSize={96}
            letterSpacing={12}
            glowRadius={40}
          />
        </div>

        {/* Subtitle types out */}
        <div style={{marginTop: 8, height: 30}}>
          <TypeWriter
            text="Platform for Research in Intelligent Synthesis of Materials"
            startFrame={60}
            charsPerFrame={1.2}
            color={COLORS.textMuted}
            fontSize={20}
            showCursor={false}
          />
        </div>

        {/* Tags */}
        <div style={{
          marginTop: 16,
          opacity: tagsOpacity,
          color: COLORS.blue,
          fontSize: 16,
          fontFamily: FONTS.title,
          letterSpacing: 2,
        }}>
          AI-Native · Autonomous · Materials Discovery
        </div>
      </AbsoluteFill>

      {/* MARC27 logo - bottom right */}
      <Img
        src={staticFile('marc27-logo.png')}
        style={{
          position: 'absolute',
          bottom: 40,
          right: 50,
          width: 120,
          opacity: logoPulse,
          filter: `drop-shadow(0 0 15px ${COLORS.brand}40)`,
        }}
      />
    </AbsoluteFill>
  );
};
```

- [ ] **Step 2: Commit**

```bash
git add video/src/scenes/TitleReveal.tsx
git commit -m "feat(video): add Scene 2 — Title Reveal with typing + glow"
```

---

### Task 7: Scene 3 — Ingest

**Files:**
- Create: `video/src/scenes/Ingest.tsx`

- [ ] **Step 1: Create Ingest scene**

```tsx
import React from 'react';
import {AbsoluteFill, interpolate, useCurrentFrame} from 'remotion';
import {COLORS, FONTS} from '../theme';
import {Terminal} from '../components/Terminal';
import {GraphViz} from '../components/GraphViz';
import {GlowText} from '../components/GlowText';

export const Ingest: React.FC = () => {
  const frame = useCurrentFrame();

  // Panels slide in
  const leftX = interpolate(frame, [0, 25], [-100, 0], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });
  const rightX = interpolate(frame, [0, 25], [100, 0], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  // Counter animates
  const counterProgress = interpolate(frame, [200, 350], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  return (
    <AbsoluteFill style={{backgroundColor: COLORS.bg}}>
      {/* Header */}
      <div style={{
        position: 'absolute',
        top: 50,
        width: '100%',
        textAlign: 'center',
      }}>
        <GlowText
          text="FEED IT EVERYTHING"
          color={COLORS.teal}
          fontSize={36}
          fadeInStart={10}
          fadeInDuration={20}
          glowRadius={20}
        />
      </div>

      {/* Split screen */}
      <div style={{
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        height: '100%',
        padding: '120px 60px 60px',
        gap: 40,
      }}>
        {/* Left: Terminal */}
        <div style={{flex: 1, transform: `translateX(${leftX}%)`}}>
          <Terminal
            command="prism ingest --corpus superalloys papers/*.pdf"
            output={[
              {text: 'Parsing 847 documents...'},
              {text: 'Extracted 12,431 entities (MAT, PRO, ELM)'},
              {text: 'Ingested to knowledge graph', color: COLORS.teal, prefix: '✓'},
            ]}
            startFrame={20}
          />
        </div>

        {/* Right: Graph */}
        <div style={{
          flex: 1,
          display: 'flex',
          flexDirection: 'column',
          alignItems: 'center',
          transform: `translateX(${rightX}%)`,
        }}>
          <div style={{
            backgroundColor: `${COLORS.panel}80`,
            borderRadius: 12,
            border: `1px solid ${COLORS.border}`,
            padding: 20,
            overflow: 'hidden',
          }}>
            <GraphViz
              nodeCount={35}
              edgeCount={45}
              seed={123}
              width={700}
              height={500}
            />
          </div>
          {/* Stats counter */}
          <div style={{
            marginTop: 16,
            display: 'flex',
            gap: 24,
            fontFamily: FONTS.mono,
            fontSize: 16,
          }}>
            <span style={{color: COLORS.brand}}>
              {Math.round(211000 * counterProgress).toLocaleString()} nodes
            </span>
            <span style={{color: COLORS.textDim}}>·</span>
            <span style={{color: COLORS.blue}}>
              {Math.round(44 * counterProgress)} corpora
            </span>
          </div>
        </div>
      </div>
    </AbsoluteFill>
  );
};
```

- [ ] **Step 2: Commit**

```bash
git add video/src/scenes/Ingest.tsx
git commit -m "feat(video): add Scene 3 — Ingest with split-screen CLI + graph"
```

---

### Task 8: Scene 4 — Query

**Files:**
- Create: `video/src/scenes/Query.tsx`

- [ ] **Step 1: Create Query scene**

```tsx
import React from 'react';
import {AbsoluteFill, interpolate, useCurrentFrame} from 'remotion';
import {COLORS} from '../theme';
import {Terminal} from '../components/Terminal';
import {GraphViz} from '../components/GraphViz';
import {GlowText} from '../components/GlowText';

export const Query: React.FC = () => {
  const frame = useCurrentFrame();

  const leftX = interpolate(frame, [0, 25], [-100, 0], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });
  const rightX = interpolate(frame, [0, 25], [100, 0], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  // Fade out at end
  const exitOpacity = interpolate(frame, [360, 390], [1, 0], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  return (
    <AbsoluteFill style={{backgroundColor: COLORS.bg, opacity: exitOpacity}}>
      {/* Header */}
      <div style={{position: 'absolute', top: 50, width: '100%', textAlign: 'center'}}>
        <GlowText
          text="ASK IT ANYTHING"
          color={COLORS.lavender}
          fontSize={36}
          fadeInStart={10}
          fadeInDuration={20}
          glowRadius={20}
        />
      </div>

      {/* Split screen */}
      <div style={{
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        height: '100%',
        padding: '120px 60px 60px',
        gap: 40,
      }}>
        {/* Left: Terminal */}
        <div style={{flex: 1, transform: `translateX(${leftX}%)`}}>
          <Terminal
            command='prism query "high-entropy alloys with oxidation resistance above 1200°C"'
            output={[
              {text: 'Searching 20+ federated providers...'},
              {text: 'OPTIMADE · Materials Project · AFLOW · ICSD', color: COLORS.textDim},
              {text: 'Found 23 candidates across 4 databases', color: COLORS.lavender, prefix: '✓'},
            ]}
            startFrame={20}
            typingSpeed={0.8}
          />
        </div>

        {/* Right: Graph with highlighted paths */}
        <div style={{
          flex: 1,
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          transform: `translateX(${rightX}%)`,
        }}>
          <div style={{
            backgroundColor: `${COLORS.panel}80`,
            borderRadius: 12,
            border: `1px solid ${COLORS.border}`,
            padding: 20,
            overflow: 'hidden',
          }}>
            <GraphViz
              nodeCount={35}
              edgeCount={45}
              seed={123}
              width={700}
              height={500}
              highlightStart={180}
              highlightColor={COLORS.lavender}
            />
          </div>
        </div>
      </div>
    </AbsoluteFill>
  );
};
```

- [ ] **Step 2: Commit**

```bash
git add video/src/scenes/Query.tsx
git commit -m "feat(video): add Scene 4 — Query with graph path highlighting"
```

---

### Task 9: Scene 5 — Mesh

**Files:**
- Create: `video/src/scenes/Mesh.tsx`

- [ ] **Step 1: Create Mesh scene**

```tsx
import React, {useMemo} from 'react';
import {AbsoluteFill, interpolate, spring, useCurrentFrame, useVideoConfig} from 'remotion';
import {COLORS, FONTS} from '../theme';
import {GlowText} from '../components/GlowText';
import {Terminal} from '../components/Terminal';

interface Peer {
  name: string;
  x: number;
  y: number;
  nodes: string;
  appearFrame: number;
}

const PEERS: Peer[] = [
  {name: 'lab-oxford', x: 750, y: 300, nodes: '211K', appearFrame: 30},
  {name: 'lab-munich', x: 1100, y: 450, nodes: '89K', appearFrame: 50},
  {name: 'lab-tokyo', x: 1350, y: 280, nodes: '156K', appearFrame: 70},
];

export const Mesh: React.FC = () => {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();

  // Connection lines appear after peers are discovered
  const connections = useMemo(() => [
    {from: 0, to: 1, drawFrame: 120},
    {from: 1, to: 2, drawFrame: 150},
    {from: 0, to: 2, drawFrame: 180},
  ], []);

  // Data flow particles along connections
  const dataFlowStart = 220;

  return (
    <AbsoluteFill style={{backgroundColor: COLORS.bg}}>
      {/* Header */}
      <div style={{position: 'absolute', top: 50, width: '100%', textAlign: 'center'}}>
        <GlowText text="FEDERATED MESH" color={COLORS.gold} fontSize={36} fadeInStart={10} fadeInDuration={20} />
        <div style={{
          marginTop: 8,
          color: COLORS.textMuted,
          fontSize: 18,
          fontFamily: FONTS.title,
          opacity: interpolate(frame, [20, 40], [0, 1], {extrapolateLeft: 'clamp', extrapolateRight: 'clamp'}),
        }}>
          No one else has this.
        </div>
      </div>

      {/* Terminal — left side */}
      <div style={{position: 'absolute', left: 60, top: 180, width: 550}}>
        <Terminal
          command="prism mesh discover"
          output={[
            {text: 'Scanning local network...'},
            {text: 'Found 3 peers:', color: COLORS.gold},
            {text: 'lab-oxford (211K nodes)', color: COLORS.teal, prefix: '  ✓', delay: 20},
            {text: 'lab-munich (89K nodes)', color: COLORS.teal, prefix: '  ✓', delay: 20},
            {text: 'lab-tokyo (156K nodes)', color: COLORS.teal, prefix: '  ✓', delay: 20},
          ]}
          startFrame={10}
        />
      </div>

      {/* Network visualization — right side */}
      <svg style={{position: 'absolute', right: 0, top: 0, width: 1920, height: 1080}} viewBox="0 0 1920 1080">
        {/* Connection lines */}
        {connections.map((conn, i) => {
          const fromPeer = PEERS[conn.from];
          const toPeer = PEERS[conn.to];
          const progress = interpolate(frame, [conn.drawFrame, conn.drawFrame + 30], [0, 1], {
            extrapolateLeft: 'clamp',
            extrapolateRight: 'clamp',
          });
          const endX = fromPeer.x + (toPeer.x - fromPeer.x) * progress;
          const endY = fromPeer.y + (toPeer.y - fromPeer.y) * progress;

          return (
            <line
              key={i}
              x1={fromPeer.x}
              y1={fromPeer.y}
              x2={endX}
              y2={endY}
              stroke={COLORS.gold}
              strokeWidth={2}
              opacity={0.6}
            />
          );
        })}

        {/* Data flow particles */}
        {frame > dataFlowStart && connections.map((conn, ci) => {
          const fromPeer = PEERS[conn.from];
          const toPeer = PEERS[conn.to];
          return Array.from({length: 3}).map((_, pi) => {
            const t = ((frame - dataFlowStart + pi * 20 + ci * 10) % 60) / 60;
            const px = fromPeer.x + (toPeer.x - fromPeer.x) * t;
            const py = fromPeer.y + (toPeer.y - fromPeer.y) * t;
            return (
              <circle
                key={`${ci}-${pi}`}
                cx={px}
                cy={py}
                r={3}
                fill={COLORS.teal}
                opacity={0.8}
              />
            );
          });
        })}
      </svg>

      {/* Peer nodes */}
      {PEERS.map((peer, i) => {
        const s = spring({
          frame: Math.max(0, frame - peer.appearFrame),
          fps,
          config: {damping: 12, stiffness: 80},
        });

        // Discovery rings
        const ringProgress = interpolate(
          frame,
          [peer.appearFrame, peer.appearFrame + 60],
          [0, 1],
          {extrapolateLeft: 'clamp', extrapolateRight: 'clamp'}
        );

        return (
          <React.Fragment key={i}>
            {/* Discovery rings */}
            {[1, 2, 3].map((ring) => {
              const ringDelay = ring * 0.2;
              const ringT = Math.max(0, ringProgress - ringDelay);
              const ringSize = ringT * 120;
              const ringOpacity = Math.max(0, 0.3 - ringT * 0.3);
              return (
                <div
                  key={ring}
                  style={{
                    position: 'absolute',
                    left: peer.x - ringSize / 2,
                    top: peer.y - ringSize / 2,
                    width: ringSize,
                    height: ringSize,
                    borderRadius: '50%',
                    border: `1px solid ${COLORS.gold}`,
                    opacity: ringOpacity,
                  }}
                />
              );
            })}
            {/* Node dot */}
            <div
              style={{
                position: 'absolute',
                left: peer.x - 12,
                top: peer.y - 12,
                width: 24,
                height: 24,
                borderRadius: '50%',
                backgroundColor: COLORS.gold,
                transform: `scale(${s})`,
                boxShadow: `0 0 20px ${COLORS.gold}60`,
              }}
            />
            {/* Label */}
            <div
              style={{
                position: 'absolute',
                left: peer.x - 60,
                top: peer.y + 20,
                width: 120,
                textAlign: 'center',
                opacity: s,
              }}
            >
              <div style={{color: COLORS.text, fontSize: 13, fontFamily: FONTS.mono, fontWeight: 600}}>
                {peer.name}
              </div>
              <div style={{color: COLORS.textMuted, fontSize: 11, fontFamily: FONTS.mono}}>
                {peer.nodes} nodes
              </div>
            </div>
          </React.Fragment>
        );
      })}
    </AbsoluteFill>
  );
};
```

- [ ] **Step 2: Commit**

```bash
git add video/src/scenes/Mesh.tsx
git commit -m "feat(video): add Scene 5 — Mesh with peer discovery + data flow"
```

---

### Task 10: Scene 6 — Workflow

**Files:**
- Create: `video/src/scenes/Workflow.tsx`

- [ ] **Step 1: Create Workflow scene**

```tsx
import React from 'react';
import {AbsoluteFill, interpolate, useCurrentFrame} from 'remotion';
import {COLORS, FONTS} from '../theme';
import {GlowText} from '../components/GlowText';
import {Pipeline} from '../components/Pipeline';

const YAML_LINES = [
  {text: 'workflow: discovery', color: COLORS.amber},
  {text: 'description: Full materials pipeline', color: COLORS.textMuted},
  {text: 'steps:', color: COLORS.amber},
  {text: '  - name: search_materials', color: COLORS.teal},
  {text: '    provider: OPTIMADE', color: COLORS.textDim},
  {text: '  - name: predict_property', color: COLORS.lavender},
  {text: '    model: M3GNet', color: COLORS.textDim},
  {text: '  - name: calphad_simulate', color: COLORS.gold},
  {text: '    engine: thermocalc', color: COLORS.textDim},
  {text: '  - name: validate_candidate', color: COLORS.amber},
  {text: '    threshold: 0.85', color: COLORS.textDim},
];

export const Workflow: React.FC = () => {
  const frame = useCurrentFrame();

  const leftX = interpolate(frame, [0, 25], [-100, 0], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });
  const rightOpacity = interpolate(frame, [30, 60], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  // YAML scroll
  const scrollY = interpolate(frame, [60, 300], [0, -80], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  return (
    <AbsoluteFill style={{backgroundColor: COLORS.bg}}>
      {/* Header */}
      <div style={{position: 'absolute', top: 50, width: '100%', textAlign: 'center'}}>
        <GlowText text="END-TO-END PIPELINES" color={COLORS.amber} fontSize={36} fadeInStart={10} fadeInDuration={20} />
      </div>

      <div style={{
        display: 'flex',
        alignItems: 'center',
        height: '100%',
        padding: '120px 80px 80px',
        gap: 60,
      }}>
        {/* Left: YAML */}
        <div style={{
          flex: 1,
          transform: `translateX(${leftX}%)`,
        }}>
          <div style={{
            backgroundColor: COLORS.panel,
            borderRadius: 12,
            border: `1px solid ${COLORS.border}`,
            overflow: 'hidden',
          }}>
            {/* File tab */}
            <div style={{
              padding: '8px 16px',
              borderBottom: `1px solid ${COLORS.border}`,
              color: COLORS.textMuted,
              fontSize: 12,
              fontFamily: FONTS.mono,
            }}>
              discovery.yaml
            </div>
            {/* Code */}
            <div style={{
              padding: '16px 20px',
              fontFamily: FONTS.mono,
              fontSize: 15,
              lineHeight: 1.8,
              overflow: 'hidden',
              height: 400,
            }}>
              <div style={{transform: `translateY(${scrollY}px)`}}>
                {YAML_LINES.map((line, i) => {
                  const lineOpacity = interpolate(frame, [30 + i * 8, 40 + i * 8], [0, 1], {
                    extrapolateLeft: 'clamp',
                    extrapolateRight: 'clamp',
                  });
                  return (
                    <div key={i} style={{color: line.color, opacity: lineOpacity}}>
                      {line.text}
                    </div>
                  );
                })}
              </div>
            </div>
          </div>
        </div>

        {/* Right: Pipeline visualization */}
        <div style={{
          flex: 1,
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          opacity: rightOpacity,
        }}>
          <Pipeline
            steps={[
              {label: 'Search', color: COLORS.teal, activateAt: 90},
              {label: 'Predict', color: COLORS.lavender, activateAt: 150},
              {label: 'Simulate', color: COLORS.gold, activateAt: 210},
              {label: 'Validate', color: COLORS.amber, activateAt: 270},
            ]}
            width={700}
          />
        </div>
      </div>
    </AbsoluteFill>
  );
};
```

- [ ] **Step 2: Commit**

```bash
git add video/src/scenes/Workflow.tsx
git commit -m "feat(video): add Scene 6 — Workflow with YAML + pipeline visualization"
```

---

### Task 11: Scene 7 — The Stack

**Files:**
- Create: `video/src/scenes/TheStack.tsx`

- [ ] **Step 1: Create TheStack scene**

```tsx
import React from 'react';
import {AbsoluteFill, interpolate, spring, useCurrentFrame, useVideoConfig} from 'remotion';
import {COLORS, FONTS} from '../theme';
import {Counter} from '../components/Counter';

const STATS = [
  {target: 211000, suffix: '', label: 'Graph Nodes', color: COLORS.brand},
  {target: 49, suffix: '', label: 'AI Tools', color: COLORS.blue},
  {target: 20, suffix: '+', label: 'Federated Databases', color: COLORS.teal},
  {target: 5, suffix: '', label: 'GPU Providers', color: COLORS.lavender},
];

const STACK_LAYERS = [
  {label: 'Rust CLI', color: COLORS.brand},
  {label: 'Python Tools', color: COLORS.blue},
  {label: 'Knowledge Graph', color: COLORS.teal},
  {label: 'Mesh Network', color: COLORS.gold},
  {label: 'Cloud Compute', color: COLORS.lavender},
];

export const TheStack: React.FC = () => {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();

  // Stack layers fade in
  const stackOpacity = interpolate(frame, [120, 160], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  return (
    <AbsoluteFill style={{backgroundColor: COLORS.bg}}>
      <AbsoluteFill style={{
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        justifyContent: 'center',
        gap: 60,
      }}>
        {/* Stats grid */}
        <div style={{
          display: 'grid',
          gridTemplateColumns: '1fr 1fr',
          gap: '40px 80px',
        }}>
          {STATS.map((stat, i) => {
            const entryScale = spring({
              frame: Math.max(0, frame - i * 10),
              fps,
              config: {damping: 14, stiffness: 80},
            });
            return (
              <div
                key={i}
                style={{
                  textAlign: 'center',
                  transform: `scale(${entryScale})`,
                }}
              >
                <Counter
                  target={stat.target}
                  suffix={stat.suffix}
                  color={stat.color}
                  fontSize={56}
                  startFrame={i * 10 + 10}
                  duration={50}
                />
                <div style={{
                  marginTop: 8,
                  color: COLORS.textMuted,
                  fontSize: 15,
                  fontFamily: FONTS.title,
                  letterSpacing: 1,
                }}>
                  {stat.label}
                </div>
              </div>
            );
          })}
        </div>

        {/* Architecture stack */}
        <div style={{
          display: 'flex',
          gap: 4,
          opacity: stackOpacity,
          alignItems: 'center',
        }}>
          {STACK_LAYERS.map((layer, i) => {
            const layerScale = spring({
              frame: Math.max(0, frame - 140 - i * 8),
              fps,
              config: {damping: 15},
            });
            return (
              <React.Fragment key={i}>
                <div style={{
                  padding: '10px 20px',
                  borderRadius: 8,
                  backgroundColor: `${layer.color}20`,
                  border: `1px solid ${layer.color}40`,
                  color: layer.color,
                  fontSize: 13,
                  fontFamily: FONTS.mono,
                  fontWeight: 600,
                  transform: `scale(${layerScale})`,
                }}>
                  {layer.label}
                </div>
                {i < STACK_LAYERS.length - 1 && (
                  <span style={{color: COLORS.textDim, fontSize: 16, opacity: stackOpacity}}>→</span>
                )}
              </React.Fragment>
            );
          })}
        </div>
      </AbsoluteFill>
    </AbsoluteFill>
  );
};
```

- [ ] **Step 2: Commit**

```bash
git add video/src/scenes/TheStack.tsx
git commit -m "feat(video): add Scene 7 — The Stack with animated counters"
```

---

### Task 12: Scene 8 — End Card

**Files:**
- Create: `video/src/scenes/EndCard.tsx`

- [ ] **Step 1: Create EndCard scene**

```tsx
import React from 'react';
import {AbsoluteFill, Img, interpolate, staticFile, useCurrentFrame} from 'remotion';
import {COLORS, FONTS} from '../theme';
import {GlowText} from '../components/GlowText';

export const EndCard: React.FC = () => {
  const frame = useCurrentFrame();

  const byOpacity = interpolate(frame, [40, 60], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  const logoOpacity = interpolate(frame, [60, 90], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  const urlOpacity = interpolate(frame, [100, 130], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  const taglineOpacity = interpolate(frame, [130, 160], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });

  // Subtle logo pulse
  const logoPulse = 1 + Math.sin(frame * 0.05) * 0.03;

  return (
    <AbsoluteFill style={{backgroundColor: COLORS.bg}}>
      <AbsoluteFill style={{
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        justifyContent: 'center',
        gap: 20,
      }}>
        {/* PRISM */}
        <GlowText
          text="PRISM"
          color={COLORS.gold}
          fontSize={80}
          letterSpacing={10}
          fadeInStart={0}
          fadeInDuration={30}
          glowRadius={50}
        />

        {/* by */}
        <div style={{
          color: COLORS.textMuted,
          fontSize: 16,
          fontFamily: FONTS.title,
          opacity: byOpacity,
          marginTop: -4,
        }}>
          by
        </div>

        {/* MARC27 logo */}
        <Img
          src={staticFile('marc27-logo.png')}
          style={{
            width: 140,
            opacity: logoOpacity,
            transform: `scale(${logoPulse})`,
            filter: `drop-shadow(0 0 20px ${COLORS.brand}40)`,
          }}
        />

        {/* GitHub URL */}
        <div style={{
          marginTop: 24,
          color: COLORS.blue,
          fontSize: 20,
          fontFamily: FONTS.mono,
          fontWeight: 500,
          opacity: urlOpacity,
        }}>
          github.com/Darth-Hidious/PRISM
        </div>

        {/* Tagline */}
        <div style={{
          color: COLORS.textDim,
          fontSize: 14,
          fontFamily: FONTS.title,
          letterSpacing: 2,
          opacity: taglineOpacity,
        }}>
          ESA SPARK Prime Contractor · ITER Supplier
        </div>
      </AbsoluteFill>
    </AbsoluteFill>
  );
};
```

- [ ] **Step 2: Commit**

```bash
git add video/src/scenes/EndCard.tsx
git commit -m "feat(video): add Scene 8 — End Card with logos + CTA"
```

---

### Task 13: Wire All Scenes into Video.tsx

**Files:**
- Modify: `video/src/Video.tsx`

- [ ] **Step 1: Update Video.tsx with all scenes**

Replace the contents of `video/src/Video.tsx` with:

```tsx
import {AbsoluteFill, Sequence} from 'remotion';
import {COLORS, SCENES} from './theme';
import {ColdOpen} from './scenes/ColdOpen';
import {TitleReveal} from './scenes/TitleReveal';
import {Ingest} from './scenes/Ingest';
import {Query} from './scenes/Query';
import {Mesh} from './scenes/Mesh';
import {Workflow} from './scenes/Workflow';
import {TheStack} from './scenes/TheStack';
import {EndCard} from './scenes/EndCard';

export const PrismShowcase: React.FC = () => {
  return (
    <AbsoluteFill style={{backgroundColor: COLORS.bg}}>
      <Sequence from={SCENES.coldOpen.from} durationInFrames={SCENES.coldOpen.duration} name="Cold Open">
        <ColdOpen />
      </Sequence>

      <Sequence from={SCENES.titleReveal.from} durationInFrames={SCENES.titleReveal.duration} name="Title Reveal">
        <TitleReveal />
      </Sequence>

      <Sequence from={SCENES.ingest.from} durationInFrames={SCENES.ingest.duration} name="Ingest">
        <Ingest />
      </Sequence>

      <Sequence from={SCENES.query.from} durationInFrames={SCENES.query.duration} name="Query">
        <Query />
      </Sequence>

      <Sequence from={SCENES.mesh.from} durationInFrames={SCENES.mesh.duration} name="Mesh">
        <Mesh />
      </Sequence>

      <Sequence from={SCENES.workflow.from} durationInFrames={SCENES.workflow.duration} name="Workflow">
        <Workflow />
      </Sequence>

      <Sequence from={SCENES.theStack.from} durationInFrames={SCENES.theStack.duration} name="The Stack">
        <TheStack />
      </Sequence>

      <Sequence from={SCENES.endCard.from} durationInFrames={SCENES.endCard.duration} name="End Card">
        <EndCard />
      </Sequence>
    </AbsoluteFill>
  );
};
```

- [ ] **Step 2: Preview the full video**

```bash
cd video && npx remotion studio
```

Expected: All 8 scenes play in sequence. Use the timeline scrubber to jump between scenes. Each scene should show properly:
- 0-8s: Particles converge, text fades in
- 8-20s: PRISM title with glow, subtitle types out
- 20-35s: Split screen — CLI ingest + graph nodes
- 35-48s: Split screen — CLI query + highlighted graph paths
- 48-60s: Mesh peers discovering + connecting
- 60-72s: YAML code + pipeline visualization
- 72-82s: Stats counting up + architecture stack
- 82-90s: End card with logos and GitHub URL

- [ ] **Step 3: Commit**

```bash
git add video/src/Video.tsx
git commit -m "feat(video): wire all 8 scenes into PrismShowcase composition"
```

---

### Task 14: Render Final Video

**Files:**
- None (output to `video/out/prism-showcase.mp4`)

- [ ] **Step 1: Add out/ to .gitignore**

Append to `video/.gitignore` (create if needed):

```
node_modules/
out/
```

- [ ] **Step 2: Render the video**

```bash
cd video && npx remotion render src/index.ts PrismShowcase out/prism-showcase.mp4 --codec h264
```

Expected: Renders 2700 frames (90 seconds) to `video/out/prism-showcase.mp4`. Takes ~2-5 minutes depending on machine.

- [ ] **Step 3: Verify output**

```bash
ls -lh video/out/prism-showcase.mp4
```

Expected: MP4 file, ~10-30MB.

- [ ] **Step 4: Commit .gitignore**

```bash
git add video/.gitignore
git commit -m "chore(video): add .gitignore for out/ and node_modules/"
```
