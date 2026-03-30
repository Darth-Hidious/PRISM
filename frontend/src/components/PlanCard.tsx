import React from "react";
import { Box, Text } from "ink";
import { ACCENT, TEXT_MUTED } from "../theme.js";
import { MarkdownText } from "./MarkdownText.js";

interface Props {
  content: string;
}

export function PlanCard({ content }: Props) {
  return (
    <Box
      flexDirection="column"
      borderStyle="single"
      borderLeft
      borderRight={false}
      borderTop={false}
      borderBottom={false}
      borderColor={ACCENT}
      paddingLeft={2}
      marginTop={1}
    >
      <Text color={ACCENT} bold>◆ Plan</Text>
      <Box marginTop={0}>
        <MarkdownText text={content} />
      </Box>
      <Text color={TEXT_MUTED}>  [y] execute  [n] reject</Text>
    </Box>
  );
}
