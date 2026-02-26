import React from "react";
import { Box, Text } from "ink";
import { ACCENT_MAGENTA } from "../theme.js";

interface Props {
  content: string;
}

export function PlanCard({ content }: Props) {
  return (
    <Box flexDirection="column" borderStyle="round" borderColor={ACCENT_MAGENTA} paddingX={1}>
      <Text color={ACCENT_MAGENTA} bold>{"\u25b7 Proposed Plan"}</Text>
      <Text>{content}</Text>
    </Box>
  );
}
