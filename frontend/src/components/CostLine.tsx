import React from "react";
import { Text } from "ink";

interface Props {
  inputTokens: number;
  outputTokens: number;
  turnCost?: number;
  sessionCost?: number;
}

function formatTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

export function CostLine({ inputTokens, outputTokens, turnCost, sessionCost }: Props) {
  const parts = [
    `${formatTokens(inputTokens)} in`,
    `${formatTokens(outputTokens)} out`,
  ];
  if (turnCost !== undefined) {
    parts.push(`$${turnCost.toFixed(4)}`);
  }
  if (sessionCost !== undefined) {
    parts.push(`total: $${sessionCost.toFixed(4)}`);
  }

  return <Text dimColor>{`\u2500 ${parts.join(" \u00b7 ")} \u2500`}</Text>;
}
