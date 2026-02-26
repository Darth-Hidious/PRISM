/**
 * Lightweight Markdown → Ink renderer.
 *
 * Handles the subset of Markdown that LLMs actually produce:
 *   Block:  ## headings, - / * / 1. lists, --- rules, ``` code blocks
 *   Inline: **bold**, *italic*, `code`
 */
import React from "react";
import { Box, Text } from "ink";
import { PRIMARY, ACCENT_CYAN, MUTED, DIM } from "../theme.js";

// ── Inline parsing ──────────────────────────────────────────────────

interface Span {
  text: string;
  bold?: boolean;
  italic?: boolean;
  code?: boolean;
}

function parseInline(line: string): Span[] {
  const spans: Span[] = [];
  // Regex: code, bold, italic (in priority order)
  const re = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*]+\*)/g;
  let last = 0;
  let match: RegExpExecArray | null;

  while ((match = re.exec(line)) !== null) {
    // Plain text before this match
    if (match.index > last) {
      spans.push({ text: line.slice(last, match.index) });
    }
    const raw = match[0];
    if (raw.startsWith("`")) {
      spans.push({ text: raw.slice(1, -1), code: true });
    } else if (raw.startsWith("**")) {
      spans.push({ text: raw.slice(2, -2), bold: true });
    } else if (raw.startsWith("*")) {
      spans.push({ text: raw.slice(1, -1), italic: true });
    }
    last = match.index + raw.length;
  }

  if (last < line.length) {
    spans.push({ text: line.slice(last) });
  }

  return spans.length > 0 ? spans : [{ text: line }];
}

function InlineSpans({ spans }: { spans: Span[] }) {
  return (
    <Text>
      {spans.map((s, i) => {
        if (s.code) return <Text key={i} color={ACCENT_CYAN}>{s.text}</Text>;
        if (s.bold) return <Text key={i} bold>{s.text}</Text>;
        if (s.italic) return <Text key={i} dimColor>{s.text}</Text>;
        return <Text key={i}>{s.text}</Text>;
      })}
    </Text>
  );
}

// ── Block parsing ───────────────────────────────────────────────────

interface Props {
  text: string;
}

export function MarkdownText({ text }: Props) {
  if (!text) return null;

  const lines = text.split("\n");
  const elements: React.ReactNode[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i]!;

    // Fenced code block
    if (line.startsWith("```")) {
      const codeLines: string[] = [];
      i++;
      while (i < lines.length && !lines[i]!.startsWith("```")) {
        codeLines.push(lines[i]!);
        i++;
      }
      i++; // skip closing ```
      elements.push(
        <Box key={elements.length} marginLeft={2} flexDirection="column">
          {codeLines.map((cl, j) => (
            <Text key={j} color={ACCENT_CYAN}>{cl}</Text>
          ))}
        </Box>,
      );
      continue;
    }

    // Heading (## or ###)
    const headingMatch = line.match(/^(#{1,4})\s+(.+)/);
    if (headingMatch) {
      const level = headingMatch[1]!.length;
      const content = headingMatch[2]!;
      elements.push(
        <Text key={elements.length} bold color={level <= 2 ? PRIMARY : undefined}>
          {level <= 2 ? "\n" : ""}{content}
        </Text>,
      );
      i++;
      continue;
    }

    // Horizontal rule
    if (/^---+$/.test(line.trim())) {
      elements.push(
        <Text key={elements.length} color={MUTED}>{"\u2500".repeat(40)}</Text>,
      );
      i++;
      continue;
    }

    // Unordered list (- or *)
    const ulMatch = line.match(/^(\s*)[-*]\s+(.+)/);
    if (ulMatch) {
      const indent = Math.floor((ulMatch[1]?.length || 0) / 2);
      const content = ulMatch[2]!;
      elements.push(
        <Text key={elements.length}>
          {"  ".repeat(indent)}{"  \u2022 "}
          <InlineSpans spans={parseInline(content)} />
        </Text>,
      );
      i++;
      continue;
    }

    // Ordered list (1. 2. etc)
    const olMatch = line.match(/^(\s*)(\d+)\.\s+(.+)/);
    if (olMatch) {
      const indent = Math.floor((olMatch[1]?.length || 0) / 2);
      const num = olMatch[2]!;
      const content = olMatch[3]!;
      elements.push(
        <Text key={elements.length}>
          {"  ".repeat(indent)}{"  "}{num}{". "}
          <InlineSpans spans={parseInline(content)} />
        </Text>,
      );
      i++;
      continue;
    }

    // Empty line
    if (line.trim() === "") {
      elements.push(<Text key={elements.length}>{" "}</Text>);
      i++;
      continue;
    }

    // Regular paragraph line with inline formatting
    elements.push(
      <InlineSpans key={elements.length} spans={parseInline(line)} />,
    );
    i++;
  }

  return (
    <Box flexDirection="column">
      {elements}
    </Box>
  );
}
