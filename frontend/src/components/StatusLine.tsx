import React from "react";
import { Box, Text } from "ink";
import { PRIMARY, TEXT_DIM, TEXT_MUTED, WARNING } from "../theme.js";

interface Props {
  autoApprove: boolean;
  messageCount: number;
  hasPlan: boolean;
}

export function StatusLine({ autoApprove, messageCount, hasPlan }: Props) {
  return (
    <Box gap={2}>
      <Text color={PRIMARY} bold>prism</Text>
      <Text color={TEXT_DIM}>{messageCount} messages</Text>
      {hasPlan ? <Text color={TEXT_MUTED}>plan active</Text> : null}
      {autoApprove ? <Text color={WARNING}>auto-approve</Text> : null}
      <Text color={TEXT_DIM}>ctrl+k commands</Text>
    </Box>
  );
}
