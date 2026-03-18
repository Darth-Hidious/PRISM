import React from "react";
import { Box, Text } from "ink";
import { PRIMARY, MUTED, TEXT } from "../theme.js";

interface Props {
  text: string;
}

export function InputCard({ text }: Props) {
  return (
    <Box flexDirection="column" marginBottom={1}>
      <Text color={PRIMARY}>you</Text>
      <Text color={TEXT}>{text}</Text>
    </Box>
  );
}
