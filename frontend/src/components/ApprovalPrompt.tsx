import React from "react";
import { Box, Text } from "ink";
import { WARNING, MUTED } from "../theme.js";

interface Props {
  toolName: string;
  toolArgs: Record<string, any>;
  onResponse: (response: string) => void;
}

export function ApprovalPrompt({ toolName, toolArgs, onResponse }: Props) {
  return (
    <Box flexDirection="column" borderStyle="round" borderColor={WARNING} paddingX={1}>
      <Text color={WARNING}>{"\u26a0 Approval Required"}</Text>
      <Text bold>{toolName}</Text>
      {Object.keys(toolArgs).length > 0 && (
        <Text dimColor>{JSON.stringify(toolArgs, null, 2)}</Text>
      )}
      <Box marginTop={1}>
        <Text color={MUTED}>{"[y]es / [n]o / [a]pprove all: "}</Text>
      </Box>
    </Box>
  );
}
