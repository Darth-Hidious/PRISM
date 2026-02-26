import React from "react";
import { Box, Text } from "ink";
import { ACCENT_CYAN } from "../theme.js";

interface Props {
  text: string;
}

export function InputCard({ text }: Props) {
  return (
    <Box borderStyle="round" borderColor={ACCENT_CYAN} paddingX={1}>
      <Text color={ACCENT_CYAN}>{"\u276f "}</Text>
      <Text>{text}</Text>
    </Box>
  );
}
