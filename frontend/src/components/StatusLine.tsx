import React from "react";
import { Text } from "ink";
import { DIM, PRIMARY, MUTED } from "../theme.js";

interface Props {
  autoApprove: boolean;
  messageCount: number;
  hasPlan: boolean;
}

export function StatusLine({ autoApprove, messageCount, hasPlan }: Props) {
  const parts: string[] = [];
  if (autoApprove) parts.push("auto-approve: ON");
  parts.push(`${messageCount} messages`);
  if (hasPlan) parts.push("plan active");

  return (
    <Text color={DIM}>
      <Text color={PRIMARY}>prism</Text>
      <Text color={MUTED}>  </Text>
      {parts.join("  ·  ")}
    </Text>
  );
}
