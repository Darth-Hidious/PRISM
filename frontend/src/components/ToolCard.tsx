import React from "react";
import { Box, Text } from "ink";
import { BORDERS, ICONS, MUTED } from "../theme.js";
import { MarkdownText } from "./MarkdownText.js";

interface Props {
  cardType: string;
  toolName: string;
  elapsedMs: number;
  content: string;
  data: Record<string, any>;
}

function formatElapsed(ms: number): string {
  if (ms >= 2000) return `${(ms / 1000).toFixed(1)}s`;
  if (ms > 0) return `${Math.round(ms)}ms`;
  return "";
}

export function ToolCard({ cardType, toolName, elapsedMs, content, data }: Props) {
  const border = BORDERS[cardType] || MUTED;
  const icon = ICONS[cardType] || "";
  const elapsed = formatElapsed(elapsedMs);

  return (
    <Box borderStyle="round" borderColor={border} paddingX={1} flexDirection="column">
      <Box>
        <Text color={border}>{icon} </Text>
        <Text dimColor>{cardType} </Text>
        {toolName && <Text color={MUTED}>{toolName} </Text>}
        {elapsed && <Text dimColor>{elapsed}</Text>}
      </Box>
      {content && <MarkdownText text={content} />}
    </Box>
  );
}
