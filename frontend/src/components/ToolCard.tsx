import React from "react";
import { Box, Text } from "ink";
import {
  SUCCESS, ERROR, TEXT, TEXT_MUTED, TEXT_DIM,
  BORDER, BG_PANEL, BORDER_AGENT,
} from "../theme.js";
import { MarkdownText } from "./MarkdownText.js";

interface Props {
  cardType: string;
  toolName: string;
  elapsedMs: number;
  content: string;
  data: Record<string, any>;
  pending?: boolean;
}

function formatElapsed(ms: number): string {
  if (ms >= 2000) return `${(ms / 1000).toFixed(1)}s`;
  if (ms > 0) return `${Math.round(ms)}ms`;
  return "";
}

// Tools that produce visible output get a bordered block.
const BLOCK_TOOLS = new Set([
  "execute", "bash", "shell", "write", "edit", "create",
  "python", "run_code", "search_code",
]);

/**
 * Inline tool — compact one-liner:  ✓ tool_name  120ms
 * Block tool — bordered container with output
 */
export function ToolCard({ cardType, toolName, elapsedMs, content, data, pending }: Props) {
  const isError = cardType === "error" || cardType === "error_partial";
  const isBlock = BLOCK_TOOLS.has(toolName) && content;
  const elapsed = formatElapsed(elapsedMs);

  if (pending) {
    // Still running — show spinner-style indicator
    return (
      <Box paddingLeft={3}>
        <Text color={TEXT_MUTED}>{"⠸ "}</Text>
        <Text color={TEXT_MUTED}>{toolName}</Text>
        {data?.summary ? (
          <Text color={TEXT_DIM}>{" "}{String(data.summary).slice(0, 60)}</Text>
        ) : null}
      </Box>
    );
  }

  if (isBlock) {
    // Block tool — bordered container with output
    return (
      <Box
        flexDirection="column"
        borderStyle="single"
        borderLeft
        borderRight={false}
        borderTop={false}
        borderBottom={false}
        borderColor={isError ? ERROR : BORDER}
        paddingLeft={2}
        marginTop={1}
      >
        <Box>
          <Text color={isError ? ERROR : SUCCESS}>{isError ? "✗" : "✓"}</Text>
          <Text color={TEXT} bold>{" "}{toolName}</Text>
          {elapsed ? <Text color={TEXT_DIM}>{" · "}{elapsed}</Text> : null}
        </Box>
        <Box marginTop={0} flexDirection="column">
          <MarkdownText text={content} />
        </Box>
      </Box>
    );
  }

  // Inline tool — compact single line
  return (
    <Box paddingLeft={3}>
      <Text color={isError ? ERROR : SUCCESS}>{isError ? "✗" : "✓"}</Text>
      <Text color={TEXT}>{" "}{toolName}</Text>
      {elapsed ? <Text color={TEXT_DIM}>{" · "}{elapsed}</Text> : null}
      {content ? (
        <Text color={TEXT_MUTED}>{" "}{content.split("\n")[0]?.slice(0, 60)}</Text>
      ) : null}
    </Box>
  );
}
