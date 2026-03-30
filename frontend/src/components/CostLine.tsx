import React from "react";
import { Box, Text } from "ink";
import { TEXT_DIM } from "../theme.js";

interface Props {
  inputTokens: number;
  outputTokens: number;
  turnCost?: number;
  sessionCost?: number;
}

function fmt(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

export function CostLine({ inputTokens, outputTokens, turnCost, sessionCost }: Props) {
  const parts: string[] = [];
  parts.push(`${fmt(inputTokens)} in · ${fmt(outputTokens)} out`);
  if (turnCost !== undefined) parts.push(`$${turnCost.toFixed(4)}`);
  if (sessionCost !== undefined) parts.push(`session $${sessionCost.toFixed(4)}`);

  return (
    <Box paddingLeft={3}>
      <Text color={TEXT_DIM}>{parts.join(" · ")}</Text>
    </Box>
  );
}
