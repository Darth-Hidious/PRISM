import React from "react";
import { Box, Text } from "ink";
import { TEXT, BORDER_USER } from "../theme.js";

interface Props {
  text: string;
}

export function InputCard({ text }: Props) {
  return (
    <Box
      borderStyle="single"
      borderLeft
      borderRight={false}
      borderTop={false}
      borderBottom={false}
      borderColor={BORDER_USER}
      paddingLeft={2}
      marginTop={1}
    >
      <Text color={TEXT} bold>{text}</Text>
    </Box>
  );
}
